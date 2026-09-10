//! `oss_search_repos` over live backends: npms.io search for candidates,
//! ecosyste.ms enrichment (dependents, stars, staleness), scored by
//! oss-rank's `Ranker` with the per-signal contribution map on every row.

use std::collections::BTreeMap;

use oss_core::EngineOutput;
use oss_core::BackendStatus;
use oss_core::ToolError;
use oss_core::{SearchReposParams, SortMode};
use oss_facade::{BackendClient, EcosystemsPackageMeta, EcosystemsRepoMeta, Filters};
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
    package: String,
    description: Option<String>,
    npm_url: Option<String>,
    npm_license: Option<String>,
    npms_score: Option<f64>,
    package_meta: Option<EcosystemsPackageMeta>,
    repo_meta: Option<EcosystemsRepoMeta>,
    rank: Option<RankResult>,
}

pub(crate) async fn run(inner: &Inner, params: &SearchReposParams) -> Result<EngineOutput, ToolError> {
    let input = serde_json::to_value(params).unwrap_or(Value::Null);
    let pool = (params.limit as usize * 2).clamp(10, 25) as u32;
    let npms_resp = inner
        .npms
        .search_code(
            &params.query,
            &Filters {
                limit: Some(pool),
                ..Filters::default()
            },
        )
        .await;

    let (npms_hits, npms_has_more) = match npms_resp {
        Ok(resp) => match &resp.status {
            oss_facade::Status::Ok | oss_facade::Status::Degraded { .. } => {
                (resp.hits.clone(), resp.has_more)
            }
            oss_facade::Status::Unavailable { reason } => {
                return unavailable_empty(inner, vec![
                    status::unavailable("npms.io", reason.clone()),
                    status::unavailable("ecosyste.ms", "skipped: no candidates to enrich"),
                ]);
            }
        },
        Err(e) => {
            return unavailable_empty(inner, vec![
                status::unavailable("npms.io", e.to_string()),
                status::unavailable("ecosyste.ms", "skipped: no candidates to enrich"),
            ]);
        }
    };

    let mut npms_detail: Option<String> = None;
    if params.topic.is_some() {
        npms_detail = Some("topic filter is not supported by npms.io search; ignored".to_string());
    }
    if let Some(lang) = &params.language {
        npms_detail = Some(format!(
            "language filter `{lang}` applied from ecosyste.ms repository metadata where known"
        ));
    }
    let npms_status = match npms_detail {
        Some(d) => status::ok_with("npms.io", d),
        None => status::ok("npms.io"),
    };

    // Enrich up to MAX_ENRICHED candidates via ecosyste.ms (package meta +
    // repo metadata), sequentially — bounded and far under the hourly budget.
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

    // Candidates with RepoSignals assembled from npms + ecosyste.ms data.
    let mut rows: Vec<CandidateRow> = Vec::new();
    let mut cand_objs: Vec<Candidate> = Vec::new();
    for (idx, hit) in npms_hits.iter().enumerate() {
        let Some(name) = hit.repo.clone().filter(|n| !n.is_empty()) else {
            continue;
        };
        let (pkg, repo_meta) = enriched.get(&idx).cloned().unwrap_or((None, None));
        let mut signals = RepoSignals {
            dependents_count: pkg
                .as_ref()
                .and_then(|p| p.dependent_packages_count)
                .unwrap_or(0),
            ..RepoSignals::default()
        };
        if let Some(m) = &repo_meta {
            signals.stars = m.stargazers_count.unwrap_or(0);
            signals.forks = m.forks_count.unwrap_or(0);
            signals.last_push_days_ago = m.pushed_at.as_deref().and_then(days_since);
            signals.archived = m.archived.unwrap_or(false);
        }
        signals.license_present = hit.license.as_deref().is_some_and(|l| !l.is_empty())
            || pkg
                .as_ref()
                .is_some_and(|p| p.normalized_licenses.iter().any(|l| !l.is_empty()))
            || repo_meta
                .as_ref()
                .and_then(|m| m.license.clone())
                .is_some_and(|l| !l.is_empty());
        let repo_url = repo_meta
            .as_ref()
            .and_then(|m| m.repository_url.clone())
            .or_else(|| pkg.as_ref().and_then(|p| p.repository_url.clone()))
            .or_else(|| {
                hit.url
                    .clone()
                    .filter(|u| owner_name_from_url(u).is_some())
            })
            .unwrap_or_else(|| format!("https://www.npmjs.com/package/{name}"));
        cand_objs.push(
            Candidate::new(repo_url)
                .with_signals(signals)
                .with_source(CandidateSource::RegistrySearch),
        );
        rows.push(CandidateRow {
            package: name,
            description: if hit.snippet.is_empty() {
                None
            } else {
                Some(hit.snippet.clone())
            }
            .or_else(|| {
                pkg.as_ref()
                    .and_then(|p| p.description.clone())
                    .or_else(|| repo_meta.as_ref().and_then(|m| m.description.clone()))
            }),
            npm_url: hit.url.clone(),
            npm_license: hit.license.clone(),
            npms_score: hit.score,
            package_meta: pkg,
            repo_meta,
            rank: None,
        });
    }

    // Score with the ranker (centrality 0: no dependency graph here — the
    // contribution map shows that honestly).
    let ranked = inner.ranker.rank(&cand_objs);
    for (row, rank) in rows.iter_mut().zip(ranked.iter()) {
        row.rank = Some(rank.clone());
    }

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
    let has_more = end < filtered.len() || npms_has_more;

    inner.record("oss_search_repos", vec![npms_status, eco_status]);
    Ok(EngineOutput {
        results: page,
        total,
        has_more,
        partial: enrich_errors > 0 && enrich_errors < enrich_total,
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
        "repo": repo_name.clone().unwrap_or_else(|| row.package.clone()),
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
        "sources": ["npms.io", "ecosyste.ms"],
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
