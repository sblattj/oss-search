//! `oss_search_code` over live backends: per-probe `Query` construction via
//! oss-query, plan-informed fan-out to GitHub + grep.app with `tokio::join`,
//! per-backend attribution merge, truthful partial/has_more.
//!
//! Paging contract: the `off:N` cursor is forwarded to every backend as its
//! fetch offset, the merged, deduped hits form the page, and the next cursor
//! advances by the number of merged hits shown. `has_more` is true when any
//! backend still has pages upstream, so a completed local page never hides
//! that more results exist.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use oss_core::EngineOutput;
use oss_core::BackendState;
use oss_core::ToolError;
use oss_core::{CodeSearchMode, SearchCodeParams};
use oss_facade::{BackendClient, BackendResponse, Filters};
use oss_query::{Backend as PlannedBackend, Query, QueryError};
use serde_json::{json, Value};

use crate::status;
use crate::util::strip_probe_delimiters;
use crate::Inner;

pub(crate) async fn run(inner: &Inner, params: &SearchCodeParams) -> Result<EngineOutput, ToolError> {
    let input = serde_json::to_value(params).unwrap_or(Value::Null);
    let offset = cursor_offset(&params.cursor, &input)?;

    // 1. One typed Query per probe: from_json + plan so typed errors
    //    (unknown fields, invalid regex, non-indexable regex) propagate.
    let mut probe_queries: Vec<(String, Query)> = Vec::new();
    let mut any_regex = false;
    for probe in &params.probes {
        let (pattern, is_regex) = strip_probe_delimiters(probe);
        any_regex |= is_regex;
        let mut obj = json!({ "pattern": pattern });
        if is_regex {
            obj["kind"] = json!("regex");
        }
        if let Some(lang) = &params.language {
            obj["langs"] = json!([lang]);
        }
        if let Some(path) = &params.path {
            obj["paths"] = json!([path]);
        }
        if let Some(filter) = &params.repo_filter
            && filter.contains('/') {
                obj["repos"] = json!([filter]);
            }
        let q = Query::from_json(&obj.to_string()).map_err(|e| query_error(e, &input))?;
        q.plan().map_err(|e| query_error(e, &input))?;
        probe_queries.push((pattern.to_string(), q));
    }

    // 2. Shared filters; the cursor offset is the backend fetch offset.
    let filters = Filters {
        language: params.language.clone(),
        repo: params
            .repo_filter
            .clone()
            .filter(|f| f.contains('/') && !f.ends_with('/')),
        path: params.path.clone(),
        regexp: any_regex,
        limit: Some(params.limit.min(30) as u32),
        offset: Some(offset.min(u32::MAX as u64) as u32),
        ..Filters::default()
    };

    // 3. Fan out: GitHub always; grep.app legacy path (Unavailable tolerated).
    let (github_resps, grep_resps) = tokio::join!(
        run_all_probes(&inner.github, &probe_queries, &filters),
        run_all_probes(&inner.grep_app, &probe_queries, &filters),
    );

    // 4. Merge with per-backend attribution (dedup on repo+path).
    let mut merged: BTreeMap<(String, String), MergedHit> = BTreeMap::new();
    for (probe_idx, resp) in github_resps.iter().enumerate() {
        absorb(&mut merged, "github", probe_idx, &params.probes, resp);
    }
    for (probe_idx, resp) in grep_resps.iter().enumerate() {
        absorb(&mut merged, "grep.app", probe_idx, &params.probes, resp);
    }

    // 5. Truthful statuses: consulted backends first; backends the plan
    //    selected but this engine cannot dispatch are reported, not hidden.
    let mut statuses = vec![
        status::aggregate("github", github_resps.iter().collect::<Vec<_>>().as_slice()),
        status::aggregate("grep.app", grep_resps.iter().collect::<Vec<_>>().as_slice()),
    ];
    let planned: HashSet<PlannedBackend> = probe_queries
        .iter()
        .filter_map(|(_, q)| q.plan().ok())
        .flat_map(|p| p.backends.into_iter().map(|b| b.backend).collect::<Vec<_>>())
        .collect();
    for backend in planned {
        match backend {
            PlannedBackend::LocalIndex => statuses.push(status::unavailable(
                "local_index",
                "live engine has no local index; the stub engine serves the hot-set corpus",
            )),
            PlannedBackend::Sourcegraph => statuses.push(status::unavailable(
                "sourcegraph",
                "no Sourcegraph client is wired into the live engine yet",
            )),
            PlannedBackend::GitHub | PlannedBackend::GrepApp => {}
        }
    }
    if inner.github_token.is_none()
        && statuses.first().is_some_and(|s| s.backend == "github" && s.status == BackendState::Ok)
    {
        statuses[0].detail = Some(
            "GITHUB_TOKEN not set: GitHub code search requires auth and will answer 401".to_string(),
        );
    }
    // partial reflects only backends the engine actually consulted failing;
    // undispatchable planned backends (local index, Sourcegraph) are recorded
    // as statuses but do not mark the result partial.
    let partial = [&statuses[0], &statuses[1]]
        .into_iter()
        .any(|s| s.status == BackendState::Error || s.status == BackendState::Degraded);
    let backend_has_more = github_resps.iter().any(|r| r.has_more)
        || grep_resps.iter().any(|r| r.has_more);

    // 6. Shape rows by mode, then window to the limit.
    let mut rows: Vec<Value> = match params.mode {
        CodeSearchMode::Repos => repo_rows(&merged, params.probes.len()),
        CodeSearchMode::Snippets => snippet_rows(&merged),
    };
    let page_len = (params.limit as usize).min(rows.len());
    rows.truncate(page_len);
    let has_more = backend_has_more || page_len == params.limit as usize;

    inner.record("oss_search_code", statuses);
    Ok(EngineOutput {
        results: rows,
        total: page_len as u64 + offset,
        has_more,
        partial,
        next_cursor: has_more.then(|| format!("off:{}", offset + page_len as u64)),
    })
}

struct MergedHit {
    repo: String,
    path: String,
    url: Option<String>,
    snippet: String,
    license: Option<String>,
    backends: BTreeSet<&'static str>,
    probes: BTreeSet<String>,
}

async fn run_all_probes<C: BackendClient>(
    client: &C,
    probes: &[(String, Query)],
    filters: &Filters,
) -> Vec<BackendResponse> {
    let mut out = Vec::with_capacity(probes.len());
    for (pattern, _) in probes {
        let resp = match client.search_code(pattern, filters).await {
            Ok(resp) => resp,
            Err(e) => BackendResponse::unavailable(e.to_string(), std::time::Duration::ZERO),
        };
        out.push(resp);
    }
    out
}

fn absorb(
    merged: &mut BTreeMap<(String, String), MergedHit>,
    backend: &'static str,
    probe_idx: usize,
    probes: &[String],
    resp: &BackendResponse,
) {
    let probe = probes
        .get(probe_idx)
        .cloned()
        .unwrap_or_else(|| probe_idx.to_string());
    for hit in &resp.hits {
        let repo = hit.repo.clone().unwrap_or_default();
        let path = hit.path.clone().unwrap_or_default();
        if repo.is_empty() && path.is_empty() {
            continue;
        }
        let key = (repo, path);
        if let Some(existing) = merged.get_mut(&key) {
            existing.backends.insert(backend);
            existing.probes.insert(probe.clone());
            if existing.url.is_none() {
                existing.url = hit.url.clone();
            }
            if existing.license.is_none() {
                existing.license = hit.license.clone();
            }
            if existing.snippet.is_empty() {
                existing.snippet = hit.snippet.clone();
            }
        } else {
            let mut backends = BTreeSet::new();
            backends.insert(backend);
            let mut probe_set = BTreeSet::new();
            probe_set.insert(probe.clone());
            let (repo, path) = key;
            merged.insert(
                (repo.clone(), path.clone()),
                MergedHit {
                    repo,
                    path,
                    url: hit.url.clone(),
                    snippet: hit.snippet.clone(),
                    license: hit.license.clone(),
                    backends,
                    probes: probe_set,
                },
            );
        }
    }
}

fn repo_rows(merged: &BTreeMap<(String, String), MergedHit>, total_probes: usize) -> Vec<Value> {
    let mut per_repo: BTreeMap<String, Vec<&MergedHit>> = BTreeMap::new();
    for hit in merged.values() {
        per_repo.entry(hit.repo.clone()).or_default().push(hit);
    }
    let mut rows: Vec<(usize, usize, String, Value)> = per_repo
        .into_iter()
        .map(|(repo, hits)| {
            let probes_hit: BTreeSet<&str> = hits
                .iter()
                .flat_map(|h| h.probes.iter().map(|s| s.as_str()))
                .collect();
            let breadth = probes_hit.len();
            let license = hits.iter().find_map(|h| h.license.clone());
            let sample = hits
                .iter()
                .find(|h| !h.snippet.is_empty())
                .map(|h| {
                    format!(
                        "{}: {} <- probe '{}'",
                        h.path,
                        first_line(&h.snippet),
                        h.probes.iter().next().cloned().unwrap_or_default()
                    )
                });
            let backends: Vec<&str> = hits
                .iter()
                .flat_map(|h| h.backends.iter().copied())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let row = json!({
                "repo": repo,
                "probes_hit": probes_hit.into_iter().collect::<Vec<_>>(),
                "probe_breadth": breadth,
                "total_probes": total_probes,
                "hit_files": hits.len(),
                "license": license,
                "backends": backends,
                "sample_evidence": sample,
            });
            (breadth, hits.len(), repo, row)
        })
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.2.cmp(&b.2)));
    rows.into_iter().map(|(_, _, _, row)| row).collect()
}

fn snippet_rows(merged: &BTreeMap<(String, String), MergedHit>) -> Vec<Value> {
    merged
        .values()
        .map(|hit| {
            json!({
                "repo": hit.repo,
                "path": hit.path,
                "locator": hit.path,
                "url": hit.url,
                "probe": hit.probes.iter().next().cloned().unwrap_or_default(),
                "snippet": first_line(&hit.snippet),
                "backends": hit.backends.iter().copied().collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn first_line(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    line.chars().take(200).collect()
}

pub(crate) fn query_error(e: QueryError, input: &Value) -> ToolError {
    ToolError::InvalidValue {
        field: "probes".to_string(),
        message: e.to_string(),
        input: input.clone(),
        correction: "see oss_guide for the query dialect; regex probes need a literal run of 3+ characters"
            .to_string(),
    }
}

pub(crate) fn cursor_offset(cursor: &Option<String>, input: &Value) -> Result<u64, ToolError> {
    match cursor {
        None => Ok(0),
        Some(c) => c
            .strip_prefix("off:")
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| ToolError::InvalidValue {
                field: "cursor".into(),
                message: format!("malformed cursor `{c}`"),
                input: input.clone(),
                correction: "pass the next_cursor value from a previous response unmodified".into(),
            }),
    }
}
