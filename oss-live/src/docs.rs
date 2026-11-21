//! `oss_fetch_docs` over live backends: ecosyste.ms tells us whether the
//! repository publishes a readme (and where), the GitHub contents API
//! fetches its bytes, and the pattern is matched against the real lines with
//! a `Source:` breadcrumb. When no docs source exists the status says
//! Unavailable and the results stay empty — content is never faked.

use oss_core::EngineOutput;
use oss_core::ToolError;
use oss_core::FetchDocsParams;
use serde_json::{json, Value};

use crate::status;
use crate::util::{split_repo, strip_probe_delimiters};
use crate::Inner;

pub(crate) async fn run(inner: &Inner, params: &FetchDocsParams) -> Result<EngineOutput, ToolError> {
    let input = serde_json::to_value(params).unwrap_or(Value::Null);
    let (owner, name) = split_repo(&params.repo).ok_or_else(|| ToolError::InvalidValue {
        field: "repo".into(),
        message: format!("repo `{}` is not in owner/name form", params.repo),
        input: input.clone(),
        correction: "use owner/name form, e.g. \"tokio-rs/tokio\"; call oss_search_repos if unsure".into(),
    })?;
    let lookup_url = format!("https://github.com/{}/{}", owner, name);

    let meta = match inner.ecosystems.repository_lookup(&lookup_url).await {
        Ok(Some(meta)) => meta,
        Ok(None) => {
            return Err(ToolError::NotFound {
                kind: "repo",
                name: params.repo.clone(),
                input: input.clone(),
                correction: "call oss_search_repos to find the exact owner/name first".into(),
            });
        }
        Err(e) => {
            inner.record(
                "oss_fetch_docs",
                vec![
                    status::unavailable("ecosyste.ms", e.to_string()),
                    status::unavailable("github-contents", "skipped: no readme path known"),
                ],
            );
            return Ok(partial_empty());
        }
    };

    let Some(readme_path) = meta.metadata.files.readme.clone() else {
        inner.record(
            "oss_fetch_docs",
            vec![
                status::ok("ecosyste.ms"),
                status::unavailable(
                    "github-contents",
                    format!("{} publishes no readme path in ecosyste.ms metadata", params.repo),
                ),
            ],
        );
        return Ok(partial_empty());
    };

    let file = match inner
        .github_content
        .file_contents(owner, name, &readme_path, None)
        .await
    {
        Ok(Some(file)) => file,
        Ok(None) => {
            inner.record(
                "oss_fetch_docs",
                vec![
                    status::ok("ecosyste.ms"),
                    status::unavailable(
                        "github-contents",
                        format!("readme `{readme_path}` listed by ecosyste.ms but absent at the default branch"),
                    ),
                ],
            );
            return Ok(partial_empty());
        }
        Err(e) => {
            inner.record(
                "oss_fetch_docs",
                vec![
                    status::ok("ecosyste.ms"),
                    status::unavailable("github-contents", e.to_string()),
                ],
            );
            return Ok(partial_empty());
        }
    };

    // Plain text: case-insensitive literal substring. `/regex/`: compiled with
    // the same regex engine `oss_search_code` uses (case-insensitive, per
    // line); an invalid regex is a typed error, never a silent 0 hits.
    let (pattern, is_regex) = strip_probe_delimiters(&params.pattern);
    let matcher = if is_regex {
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|e| ToolError::InvalidValue {
                field: "pattern".into(),
                message: format!("invalid regex `{pattern}`: {e}"),
                input: input.clone(),
                correction: "use RE2-style /regex/ syntax, or pass plain text for a literal match; see oss_guide".into(),
            })?;
        Matcher::Regex(re)
    } else {
        Matcher::Literal(pattern.to_lowercase())
    };
    let source_url = meta
        .html_url
        .clone()
        .map(|base| format!("{base}/blob/HEAD/{readme_path}"))
        .unwrap_or_else(|| format!("{lookup_url}/blob/HEAD/{readme_path}"));

    let hits: Vec<Value> = file
        .text
        .lines()
        .enumerate()
        .filter(|(_, l)| matcher.is_match(l))
        .map(|(i, l)| {
            json!({
                "line": i + 1,
                "text": l,
                "breadcrumb": format!("Source: {source_url}"),
            })
        })
        .collect();

    let found = !hits.is_empty();
    let hit_count = hits.len();
    let row = json!({
        "repo": meta.full_name.clone().unwrap_or_else(|| params.repo.clone()),
        "page": readme_path,
        "source_url": source_url,
        "hits": hits,
    });
    inner.record(
        "oss_fetch_docs",
        vec![
            status::ok("ecosyste.ms"),
            status::ok_with("github-contents", format!("readme fetched, {hit_count} matching line(s)")),
        ],
    );
    Ok(EngineOutput::complete(if found { vec![row] } else { vec![] }))
}

enum Matcher {
    Literal(String),
    Regex(regex::Regex),
}

impl Matcher {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Matcher::Literal(needle) => line.to_lowercase().contains(needle),
            Matcher::Regex(re) => re.is_match(line),
        }
    }
}

fn partial_empty() -> EngineOutput {
    EngineOutput {
        results: Vec::new(),
        total: 0,
        has_more: false,
        partial: true,
        next_cursor: None,
    }
}
