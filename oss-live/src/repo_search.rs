//! `oss_search_repos` over live backends. Candidates come from two sources:
//! npms.io (npm packages) and GitHub repository search (any language). They
//! are deduped by `owner/name` (case-insensitive), npm candidates are
//! enriched via ecosyste.ms (dependents, stars, staleness), and everything is
//! scored by oss-rank's `Ranker` with the per-signal contribution map on every
//! row.
//!
//! How GitHub-only candidates are scored: the GitHub payload supplies stars,
//! forks, last push, archived and license, so they get real popularity,
//! maintenance and license contributions. They have no registry data, so
//! `dependents` is 0 (and centrality is 0 for every row: there is no
//! dependency graph here). The ranking model is unchanged; a GitHub-only
//! repo therefore cannot outrank an equally healthy package that other
//! packages depend on, which is the intended "load-bearing" bias.

use std::collections::BTreeMap;

use oss_core::EngineOutput;
use oss_core::BackendStatus;
use oss_core::ToolError;
use oss_core::{SearchReposParams, SortMode};
use oss_facade::{
    BackendClient, EcosystemsPackageMeta, EcosystemsRepoMeta, Filters, GitHubRepoHit,
    RepoSearchFilters, RepoSort,
};
use oss_rank::{Candidate, CandidateSource, RankResult, RepoSignals};
use serde_json::{json, Value};

use crate::status;
use crate::util::{activity, days_since, license_class, owner_name_from_url, star_tier};
use crate::Inner;

/// ecosyste.ms registry slug for npm (the live registry name).
const NPM_REGISTRY: &str = "npmjs.org";
/// Cap on ecosyste.ms enrichment candidates per call (2 lookups each),
/// staying far below the 5000/hour budget.
const MAX_ENRICHED: usize = 12;

struct CandidateRow {
    /// npm package name; `None` for GitHub-only candidates.
    package: Option<String>,
    description: Option<String>,
    npm_url: Option<String>,
    npm_license: Option<String>,
    npms_score: Option<f64>,
    package_meta: Option<EcosystemsPackageMeta>,
    repo_meta: Option<EcosystemsRepoMeta>,
    sources: Vec<&'static str>,
    rank: Option<RankResult>,
}

/// GitHub language qualifier for the npm-flavoured aliases this tool accepts.
fn github_language(lang: &str) -> String {
    match lang.to_ascii_lowercase().as_str() {
        "js" | "node" | "nodejs" | "npm" => "JavaScript".to_string(),
        "ts" => "TypeScript".to_string(),
        _ => lang.to_string(),
    }
}

/// Fold a GitHub payload into (possibly absent) ecosyste.ms repo metadata.
/// Live GitHub values win for the volatile signals (stars, forks, last push,
/// archived); descriptive fields only fill gaps.
fn merge_github(meta: Option<EcosystemsRepoMeta>, gh: &GitHubRepoHit) -> EcosystemsRepoMeta {
    let mut m = meta.unwrap_or_default();
    m.stargazers_count = Some(gh.stars);
    m.forks_count = Some(gh.forks);
    m.archived = Some(gh.archived);
    if gh.pushed_at.is_some() {
        m.pushed_at = gh.pushed_at.clone();
    }
    let url = gh
        .html_url
        .clone()
        .unwrap_or_else(|| format!("https://github.com/{}", gh.full_name));
    m.full_name.get_or_insert_with(|| gh.full_name.clone());
    if m.description.is_none() {
        m.description = gh.description.clone();
    }
    if m.license.as_deref().is_none_or(str::is_empty) {
        m.license = gh.license.clone();
    }
    if m.language.is_none() {
        m.language = gh.language.clone();
    }
    if m.topics.is_empty() {
        m.topics = gh.topics.clone();
    }
    m.html_url.get_or_insert_with(|| url.clone());
    m.repository_url.get_or_insert(url);
    m
}

/// Lowercased `owner/name` identifying the row's GitHub repo, if known.
fn dedupe_key(row: &CandidateRow, npm_hit_url: Option<&str>) -> Option<String> {
    row.repo_meta
        .as_ref()
        .and_then(|m| {
            m.full_name
                .clone()
                .or_else(|| m.repository_url.as_deref().and_then(owner_name_from_url))
        })
        .or_else(|| {
            row.package_meta
                .as_ref()
                .and_then(|p| p.repository_url.as_deref())
                .and_then(owner_name_from_url)
        })
        .or_else(|| npm_hit_url.and_then(owner_name_from_url))
        .map(|k| k.to_ascii_lowercase())
}

fn signals_for(row: &CandidateRow) -> RepoSignals {
    let mut signals = RepoSignals {
        dependents_count: row
            .package_meta
            .as_ref()
            .and_then(|p| p.dependent_packages_count)
            .unwrap_or(0),
        ..RepoSignals::default()
    };
    if let Some(m) = &row.repo_meta {
        signals.stars = m.stargazers_count.unwrap_or(0);
        signals.forks = m.forks_count.unwrap_or(0);
        signals.last_push_days_ago = m.pushed_at.as_deref().and_then(days_since);
        signals.archived = m.archived.unwrap_or(false);
    }
    signals.license_present = row.npm_license.as_deref().is_some_and(|l| !l.is_empty())
        || row
            .package_meta
            .as_ref()
            .is_some_and(|p| p.normalized_licenses.iter().any(|l| !l.is_empty()))
        || row
            .repo_meta
            .as_ref()
            .and_then(|m| m.license.clone())
            .is_some_and(|l| !l.is_empty());
    signals
}

pub(crate) async fn run(inner: &Inner, params: &SearchReposParams) -> Result<EngineOutput, ToolError> {
    let input = serde_json::to_value(params).unwrap_or(Value::Null);
    let pool = (params.limit as usize * 2).clamp(10, 25) as u32;
    let npms_filters = Filters {
        limit: Some(pool),
        ..Filters::default()
    };
    let gh_filters = RepoSearchFilters {
            language: params.language.as_deref().map(github_language),
            min_stars: params.min_stars,
            topic: params.topic.clone(),
            sort: match params.sort {
                SortMode::Fitness => RepoSort::BestMatch,
                SortMode::Stars => RepoSort::Stars,
                SortMode::Updated => RepoSort::Updated,
            },
            limit: Some(pool),
    };
    let npms_fut = inner.npms.search_code(&params.query, &npms_filters);
    let gh_fut = inner.github.search_repos(&params.query, &gh_filters);
    let (npms_resp, gh_resp) = tokio::join!(npms_fut, gh_fut);

    let (npms_hits, npms_has_more, npms_status, npms_failed) = match npms_resp {
        Ok(resp) => match &resp.status {
            oss_facade::Status::Ok | oss_facade::Status::Degraded { .. } => {
                let mut npms_detail: Option<String> = None;
                if params.topic.is_some() {
                    npms_detail = Some(
                        "topic filter is not supported by npms.io search; applied to GitHub results only"
                            .to_string(),
                    );
                }
                if let Some(lang) = &params.language {
                    npms_detail = Some(format!(
                        "language filter `{lang}` applied from ecosyste.ms repository metadata where known"
                    ));
                }
                let st = match npms_detail {
                    Some(d) => status::ok_with("npms.io", d),
                    None => status::ok("npms.io"),
                };
                (resp.hits.clone(), resp.has_more, st, false)
            }
            oss_facade::Status::Unavailable { reason } => (
                Vec::new(),
                false,
                status::unavailable("npms.io", reason.clone()),
                true,
            ),
        },
        Err(e) => (
            Vec::new(),
            false,
            status::unavailable("npms.io", e.to_string()),
            true,
        ),
    };

    let (gh_hits, gh_has_more, gh_status, gh_failed) = match gh_resp {
        Ok(resp) => match &resp.status {
            oss_facade::Status::Ok => {
                let st = if resp.warnings.is_empty() {
                    status::ok("github")
                } else {
                    status::ok_with("github", resp.warnings.join("; "))
                };
                (resp.hits, resp.has_more, st, false)
            }
            oss_facade::Status::Degraded { reason } => (
                resp.hits.clone(),
                resp.has_more,
                status::degraded("github", reason.clone()),
                true,
            ),
            oss_facade::Status::Unavailable { reason } => (
                Vec::new(),
                false,
                status::unavailable("github", reason.clone()),
                true,
            ),
        },
        Err(e) => (
            Vec::new(),
            false,
            status::unavailable("github", e.to_string()),
            true,
        ),
    };

    if npms_failed && gh_failed && npms_hits.is_empty() && gh_hits.is_empty() {
        return unavailable_empty(
            inner,
            vec![
                npms_status,
                gh_status,
                status::unavailable("ecosyste.ms", "skipped: no candidates to enrich"),
            ],
        );
    }

    // Enrich up to MAX_ENRICHED npm candidates via ecosyste.ms (package meta +
    // repo metadata), sequentially — bounded and far under the hourly budget.
    // GitHub-only candidates are not enriched: their GitHub payload already
    // carries the repo signals.
    let mut enrich_errors = 0usize;
    let mut enriched: BTreeMap<usize, (Option<EcosystemsPackageMeta>, Option<EcosystemsRepoMeta>)> =
        BTreeMap::new();
    for (idx, hit) in npms_hits.iter().enumerate().take(MAX_ENRICHED) {
        let Some(name) = hit.repo.clone().filter(|n| !n.is_empty()) else {
            continue;
        };
        let pkg = inner.ecosystems.package_metadata(NPM_REGISTRY, &name).await;
        let repo_url = match &pkg {
            Ok(Some(p)) => p.repository_url.clone(),
            _ => hit
                .url
                .clone()
                .filter(|u| owner_name_from_url(u).is_some()),
        };
        let repo_meta = match repo_url {
            Some(url) => inner.ecosystems.repository_lookup(&url).await.ok().flatten(),
            None => None,
        };
        match pkg {
            Ok(p) => {
                enriched.insert(idx, (p, repo_meta));
            }
            Err(_) => {
                enrich_errors += 1;
                enriched.insert(idx, (None, repo_meta));
            }
        }
    }
    let enrich_total = enriched.len();
    let eco_status = if enrich_total == 0 {
        status::ok("ecosyste.ms")
    } else if enrich_errors == enrich_total {
        status::unavailable("ecosyste.ms", "all enrichment lookups failed")
    } else if enrich_errors > 0 {
        status::degraded(
            "ecosyste.ms",
            format!("{enrich_errors} of {enrich_total} enrichment lookup(s) failed"),
        )
    } else {
        status::ok("ecosyste.ms")
    };

    // npm rows first.
    let mut rows: Vec<CandidateRow> = Vec::new();
    let mut keys: Vec<Option<String>> = Vec::new();
    for (idx, hit) in npms_hits.iter().enumerate() {
        let Some(name) = hit.repo.clone().filter(|n| !n.is_empty()) else {
            continue;
        };
        let (pkg, repo_meta) = enriched.get(&idx).cloned().unwrap_or((None, None));
        let description = if hit.snippet.is_empty() {
            None
        } else {
            Some(hit.snippet.clone())
        }
        .or_else(|| {
            pkg.as_ref()
                .and_then(|p| p.description.clone())
                .or_else(|| repo_meta.as_ref().and_then(|m| m.description.clone()))
        });
        let row = CandidateRow {
            package: Some(name),
            description,
            npm_url: hit.url.clone(),
            npm_license: hit.license.clone(),
            npms_score: hit.score,
            package_meta: pkg,
            repo_meta,
            sources: vec!["npms.io", "ecosyste.ms"],
            rank: None,
        };
        keys.push(dedupe_key(&row, hit.url.as_deref()));
        rows.push(row);
    }

    // Merge GitHub hits: a repo already present as an npm candidate (same
    // owner/name, case-insensitive) gains the GitHub source and fresh
    // signals; anything else becomes a GitHub-only candidate.
    for gh in &gh_hits {
        let key = gh.full_name.to_ascii_lowercase();
        if let Some(i) = keys.iter().position(|k| k.as_deref() == Some(key.as_str())) {
            let row = &mut rows[i];
            row.repo_meta = Some(merge_github(row.repo_meta.take(), gh));
            if !row.sources.contains(&"github") {
                row.sources.push("github");
            }
            continue;
        }
        keys.push(Some(key));
        rows.push(CandidateRow {
            package: None,
            description: gh.description.clone(),
            npm_url: None,
            npm_license: None,
            npms_score: None,
            package_meta: None,
            repo_meta: Some(merge_github(None, gh)),
            sources: vec!["github"],
            rank: None,
        });
    }

    // Score each row with the ranker (centrality 0: no dependency graph here
    // — the contribution map shows that honestly), then order by fitness.
    for row in &mut rows {
        let repo_url = row
            .repo_meta
            .as_ref()
            .and_then(|m| m.repository_url.clone())
            .or_else(|| {
                row.package_meta
                    .as_ref()
                    .and_then(|p| p.repository_url.clone())
            })
            .or_else(|| {
                row.npm_url
                    .clone()
                    .filter(|u| owner_name_from_url(u).is_some())
            })
            .unwrap_or_else(|| {
                format!(
                    "https://www.npmjs.com/package/{}",
                    row.package.clone().unwrap_or_default()
                )
            });
        let cand = Candidate::new(repo_url)
            .with_signals(signals_for(row))
            .with_source(CandidateSource::RegistrySearch);
        row.rank = Some(inner.ranker.score(&cand));
    }
    rows.sort_by(|a, b| {
        let (ra, rb) = (a.rank.as_ref().unwrap(), b.rank.as_ref().unwrap());
        rb.score
            .total_cmp(&ra.score)
            .then_with(|| ra.repo_url.cmp(&rb.repo_url))
    });

    // Filters applied truthfully where the data supports them.
    let mut filtered: Vec<&CandidateRow> = rows.iter().collect();
    if let Some(min) = params.min_stars {
        filtered.retain(|r| {
            r.repo_meta
                .as_ref()
                .and_then(|m| m.stargazers_count)
                .unwrap_or(0)
                >= min
        });
    }
    if let Some(lang) = &params.language {
        let lang = lang.to_ascii_lowercase();
        let npm_langs = ["javascript", "js", "typescript", "ts", "node", "nodejs", "npm"];
        if !npm_langs.contains(&lang.as_str()) {
            filtered.retain(|r| {
                r.repo_meta
                    .as_ref()
                    .and_then(|m| m.language.clone())
                    .is_some_and(|l| l.to_ascii_lowercase().contains(&lang))
            });
        }
    }
    match params.sort {
        SortMode::Fitness => {}
        SortMode::Stars => filtered.sort_by_key(|r| {
            std::cmp::Reverse(
                r.repo_meta
                    .as_ref()
                    .and_then(|m| m.stargazers_count)
                    .unwrap_or(0),
            )
        }),
        SortMode::Updated => filtered.sort_by_key(|r| {
            r.repo_meta
                .as_ref()
                .and_then(|m| m.pushed_at.as_deref().and_then(days_since))
                .unwrap_or(u32::MAX)
        }),
    }

    // Page over the final ordered list.
    let offset = crate::code_search::cursor_offset(&params.cursor, &input)?;
    let total = filtered.len() as u64;
    let end = ((offset + params.limit) as usize).min(filtered.len());
    let page: Vec<Value> = filtered[offset as usize..end]
        .iter()
        .map(|r| row_json(r))
        .collect();
    let has_more = end < filtered.len() || npms_has_more || gh_has_more;

    inner.record("oss_search_repos", vec![npms_status, gh_status, eco_status]);
    Ok(EngineOutput {
        results: page,
        total,
        has_more,
        partial: (enrich_errors > 0 && enrich_errors < enrich_total) || npms_failed || gh_failed,
        next_cursor: has_more.then(|| format!("off:{end}")),
    })
}

fn row_json(row: &CandidateRow) -> Value {
    // rows and ranked come from the same candidate list, so every row has a
    // rank; the fallback only exists for structural safety.
    let rank = row.rank.clone().unwrap_or(RankResult {
        repo_url: String::new(),
        score: 0.0,
        contributions: BTreeMap::new(),
        maintenance_gate: 1.0,
        provenance: vec![],
    });
    let stars = row
        .repo_meta
        .as_ref()
        .and_then(|m| m.stargazers_count)
        .unwrap_or(0);
    let days = row
        .repo_meta
        .as_ref()
        .and_then(|m| m.pushed_at.as_deref().and_then(days_since));
    let repo_name = row
        .repo_meta
        .as_ref()
        .and_then(|m| m.full_name.clone())
        .or_else(|| {
            row.repo_meta
                .as_ref()
                .and_then(|m| m.repository_url.as_deref())
                .and_then(owner_name_from_url)
        });
    let license = row
        .npm_license
        .clone()
        .or_else(|| {
            row.package_meta
                .as_ref()
                .and_then(|p| p.normalized_licenses.first().cloned())
        })
        .or_else(|| row.repo_meta.as_ref().and_then(|m| m.license.clone()))
        .map(|l| l.to_ascii_uppercase());
    json!({
        "repo": repo_name.clone().or_else(|| row.package.clone()),
        "package": row.package,
        "what": row.description,
        "score": (rank.score * 10_000.0).round() / 10_000.0,
        "contributions": rank.contributions,
        "maintenance_gate": rank.maintenance_gate,
        "stars": stars,
        "star_tier": star_tier(stars),
        "dependents": row.package_meta.as_ref().and_then(|p| p.dependent_packages_count),
        "license": license,
        "license_class": license.as_deref().map(license_class),
        "last_push": row.repo_meta.as_ref().and_then(|m| m.pushed_at.clone()),
        "days_since_activity": days,
        "activity": days.map(|d| activity(u64::from(d))),
        "npms_score": row.npms_score,
        "latest_release": row.package_meta.as_ref().and_then(|p| p.latest_release_number.clone()),
        "url": row
            .repo_meta
            .as_ref()
            .and_then(|m| m.html_url.clone().or_else(|| m.repository_url.clone()))
            .or_else(|| row.npm_url.clone()),
        "sources": row.sources,
    })
}

fn unavailable_empty(inner: &Inner, statuses: Vec<BackendStatus>) -> Result<EngineOutput, ToolError> {
    inner.record("oss_search_repos", statuses);
    Ok(EngineOutput {
        results: Vec::new(),
        total: 0,
        has_more: false,
        partial: true,
        next_cursor: None,
    })
}
