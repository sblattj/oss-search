//! `oss_repo_tree` / `oss_fetch_file` over the GitHub REST contents/trees
//! API. 404s are typed `NotFound` tool errors (repo/ref/file genuinely
//! absent); transport and auth failures degrade into a recorded backend
//! status plus an empty, `partial` result rather than a faked negative.

use oss_core::EngineOutput;
use oss_core::ToolError;
use oss_core::{FetchFileParams, RepoTreeParams};
use oss_facade::FacadeError;
use serde_json::{json, Value};

use crate::status;
use crate::util::split_repo;
use crate::Inner;

pub(crate) async fn run_tree(inner: &Inner, params: &RepoTreeParams) -> Result<EngineOutput, ToolError> {
    let input = serde_json::to_value(params).unwrap_or(Value::Null);
    let (owner, name) = split_repo(&params.repo).ok_or_else(|| ToolError::InvalidValue {
        field: "repo".into(),
        message: format!("repo `{}` is not in owner/name form", params.repo),
        input: input.clone(),
        correction: "use owner/name form, e.g. \"tokio-rs/tokio\"; call oss_search_repos if unsure".into(),
    })?;
    let ref_ = params.ref_.clone().unwrap_or_else(|| "HEAD".to_string());

    let tree = match inner.github_content.tree(owner, name, &ref_, true).await {
        Ok(Some(tree)) => tree,
        Ok(None) => {
            return Err(ToolError::NotFound {
                kind: "repo",
                name: params.repo.clone(),
                input: input.clone(),
                correction: format!(
                    "repo or ref `{ref_}` not found on GitHub; call oss_search_repos to confirm the name"
                ),
            });
        }
        Err(e) => {
            return degraded_empty(inner, "oss_repo_tree", e.to_string());
        }
    };

    let mut paths: Vec<(String, String)> = tree
        .entries
        .iter()
        .map(|e| (e.path.clone(), e.kind.clone()))
        .collect();
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(prefix) = &params.path_prefix {
        paths.retain(|(p, _)| p.starts_with(prefix.as_str()));
    }
    if let Some(glob) = &params.glob {
        let suffix = glob.trim_start_matches('*');
        paths.retain(|(p, _)| {
            p.rsplit('/')
                .next()
                .is_some_and(|file| file.ends_with(suffix))
        });
    }
    let total = paths.len() as u64;
    let end = (params.limit as usize).min(paths.len());
    let rows: Vec<Value> = paths[..end]
        .iter()
        .map(|(p, kind)| json!({ "path": p, "type": kind }))
        .collect();
    let has_more = end < paths.len();

    let gh_status = if tree.truncated {
        status::degraded(
            "github-contents",
            "GitHub truncated the recursive tree (repos over 100k entries / 7MB); results are partial — page with path_prefix",
        )
    } else {
        status::ok("github-contents")
    };
    inner.record("oss_repo_tree", vec![gh_status]);
    Ok(EngineOutput {
        results: rows,
        total,
        has_more,
        partial: tree.truncated,
        next_cursor: has_more.then(|| format!("off:{end}")),
    })
}

pub(crate) async fn run_file(inner: &Inner, params: &FetchFileParams) -> Result<EngineOutput, ToolError> {
    let input = serde_json::to_value(params).unwrap_or(Value::Null);
    let (owner, name) = split_repo(&params.repo).ok_or_else(|| ToolError::InvalidValue {
        field: "repo".into(),
        message: format!("repo `{}` is not in owner/name form", params.repo),
        input: input.clone(),
        correction: "use owner/name form, e.g. \"tokio-rs/tokio\"; call oss_search_repos if unsure".into(),
    })?;

    let file = match inner
        .github_content
        .file_contents(owner, name, &params.path, params.ref_.as_deref())
        .await
    {
        Ok(Some(file)) => file,
        Ok(None) => {
            return Err(ToolError::NotFound {
                kind: "file",
                name: format!("{}/{}", params.repo, params.path),
                input: input.clone(),
                correction: format!(
                    "call oss_repo_tree for {} first — a 404 from a guessed path proves nothing",
                    params.repo
                ),
            });
        }
        Err(FacadeError::GitHub(g)) if g.contains("too large") => {
            return degraded_empty(inner, "oss_fetch_file", g);
        }
        Err(e) => {
            return degraded_empty(inner, "oss_fetch_file", e.to_string());
        }
    };

    let lines: Vec<&str> = file.text.split('\n').collect();
    let total_lines = lines.len() as u64;
    let (start, end) = match &params.line_range {
        Some(range) => (
            range.start.min(total_lines.max(1)),
            range.end.min(total_lines),
        ),
        None => (1, total_lines.min(100)),
    };
    let content = if total_lines == 0 || start > end {
        String::new()
    } else {
        lines[(start as usize - 1)..(end as usize)].join("\n")
    };
    let row = json!({
        "repo": params.repo,
        "path": file.path,
        "ref": params.ref_.clone().unwrap_or_else(|| "default".into()),
        "start_line": start,
        "end_line": end,
        "total_lines": total_lines,
        "bytes": file.size,
        "content": content,
    });
    inner.record(
        "oss_fetch_file",
        vec![status::ok_with(
            "github-contents",
            format!(
                "contents API; {} byte file",
                file.size
            ),
        )],
    );
    Ok(EngineOutput::complete(vec![row]))
}

fn degraded_empty(inner: &Inner, tool: &str, reason: String) -> Result<EngineOutput, ToolError> {
    inner.record(tool, vec![status::unavailable("github-contents", reason)]);
    Ok(EngineOutput {
        results: Vec::new(),
        total: 0,
        has_more: false,
        partial: true,
        next_cursor: None,
    })
}
