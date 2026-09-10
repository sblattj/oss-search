use serde_json::{json, Value};

use crate::engine::{EngineOutput, SearchEngine};
use crate::envelope::{BackendState, BackendStatus};
use crate::error::ToolError;
use crate::params::{
    CodeSearchMode, FetchDocsParams, FetchFileParams, GuideParams, RepoProfileParams,
    RepoTreeParams, SearchCodeParams, SearchReposParams, SortMode,
};
use crate::shaping::{format_snippet, DEFAULT_CONTEXT_LINES};

struct StubFile {
    path: &'static str,
    lines: Vec<&'static str>,
}

struct StubDoc {
    path: &'static str,
    url: &'static str,
    lines: Vec<&'static str>,
}

struct StubRepo {
    repo: &'static str,
    language: &'static str,
    stars: u64,
    license: &'static str,
    license_class: &'static str,
    topics: Vec<&'static str>,
    last_release: &'static str,
    days_since_activity: u64,
    archived: bool,
    what: &'static str,
    catch: &'static str,
    fitness: u64,
    readme_digest: &'static str,
    release_cadence: &'static str,
    runtime_support: Vec<&'static str>,
    files: Vec<StubFile>,
    docs: Vec<StubDoc>,
}

fn corpus() -> Vec<StubRepo> {
    vec![
        StubRepo {
            repo: "gabe/hashopen",
            language: "Python",
            stars: 812,
            license: "MIT",
            license_class: "permissive",
            topics: vec!["calendar", "finance", "market-data"],
            last_release: "2026-08-28",
            days_since_activity: 3,
            archived: false,
            what: "Exchange calendar library with special-closes and half-day session handling",
            catch: "exchange coverage is uneven outside US/EU; no async API",
            fitness: 92,
            readme_digest: "hashopen computes open/closed market sessions from exchange calendars. Exposes get_special_closes(year) and get_early_closes(year) per exchange. Ships ICS export and a CLI. Supported exchanges: NYSE, NASDAQ, LSE, XETRA.",
            release_cadence: "monthly minor, hotfixes within days",
            runtime_support: vec!["python >=3.10", "no C extensions"],
            files: vec![
                StubFile {
                    path: "src/market.py",
                    lines: vec![
                        "import datetime",
                        "from .types import Session, CloseKind",
                        "",
                        "SPECIAL_CLOSES: dict[int, set[datetime.date]] = {}",
                        "EARLY_CLOSES: dict[int, set[datetime.date]] = {}",
                        "",
                        "",
                        "def get_special_closes(calendar: str, year: int):",
                        "    \"\"\"Full-day closures for a calendar and year.\"\"\"",
                        "    return SPECIAL_CLOSES.setdefault(year, load(calendar, year))",
                        "",
                        "",
                        "def get_early_closes(calendar: str, year: int):",
                        "    \"\"\"Half-day sessions before holidays (earlyCloses upstream config).\"\"\"",
                        "    cfg = upstream_config(calendar, key=\"earlyCloses\")",
                        "    return EARLY_CLOSES.setdefault(year, expand(cfg, year))",
                        "",
                        "",
                        "def is_open(calendar: str, when: datetime.datetime) -> bool:",
                        "    d = when.date()",
                        "    return d not in get_special_closes(calendar, d.year) and d.weekday() < 5",
                    ],
                },
                StubFile {
                    path: "src/ics_export.py",
                    lines: vec![
                        "from .market import get_special_closes, get_early_closes",
                        "",
                        "",
                        "def to_ics(calendar: str, year: int) -> str:",
                        "    events = [",
                        "        f\"DTSTART:{d.isoformat()}\" for d in sorted(get_special_closes(calendar, year))",
                        "    ]",
                        "    events += [",
                        "        f\"DTSTART:{d.isoformat()}\" for d in sorted(get_early_closes(calendar, year))",
                        "    ]",
                        "    return \"\\r\\n\".join(events)",
                    ],
                },
                StubFile {
                    path: "README.md",
                    lines: vec![
                        "# hashopen",
                        "",
                        "Exchange calendars with special closes and early closes.",
                        "```python",
                        "from hashopen import get_special_closes",
                        "get_special_closes(\"NYSE\", 2026)",
                        "```",
                    ],
                },
            ],
            docs: vec![
                StubDoc {
                    path: "docs/calendars.md",
                    url: "https://gabe.dev/hashopen/docs/calendars",
                    lines: vec![
                        "# Calendars",
                        "",
                        "Each exchange maps to a calendar id: NYSE, NASDAQ, LSE, XETRA.",
                        "Special closes are full-day closures; early closes are half-day sessions",
                        "before a holiday. Use `get_early_closes(exchange, year)` for half-days.",
                    ],
                },
                StubDoc {
                    path: "docs/export.md",
                    url: "https://gabe.dev/hashopen/docs/export",
                    lines: vec![
                        "# Export",
                        "",
                        "ICS export covers special closes and early closes in one calendar feed.",
                    ],
                },
            ],
        },
        StubRepo {
            repo: "helix/edge-sdk",
            language: "TypeScript",
            stars: 12400,
            license: "Apache-2.0",
            license_class: "permissive",
            topics: vec!["edge", "http-client", "rate-limiting"],
            last_release: "2026-09-01",
            days_since_activity: 8,
            archived: false,
            what: "Edge-first TS SDK: streaming fetch with adaptive rate-limit backoff",
            catch: "fast-moving APIs; major-version churn between minors",
            fitness: 88,
            readme_digest: "edge-sdk is a streaming HTTP client for edge runtimes. Reads x-ratelimit-remaining response headers to pace requests, retries on 429 with server-advised delays, and multiplexes SSE streams. Works on Cloudflare Workers, Deno Deploy, and Node 20+.",
            release_cadence: "biweekly minor, majors ~quarterly",
            runtime_support: vec!["node >=20", "deno >=1.44", "cloudflare-workers", "engines field enforced"],
            files: vec![
                StubFile {
                    path: "packages/core/src/stream.ts",
                    lines: vec![
                        "export interface RateLimitState {",
                        "  remaining: number;",
                        "  resetAt: number;",
                        "}",
                        "",
                        "export function parseRateLimit(headers: Headers): RateLimitState {",
                        "  const remaining = Number(headers.get('x-ratelimit-remaining') ?? '0');",
                        "  const resetAt = Number(headers.get('x-ratelimit-reset') ?? '0') * 1000;",
                        "  return { remaining, resetAt };",
                        "}",
                        "",
                        "export async function pacedFetch(url: string, init?: RequestInit) {",
                        "  const res = await fetch(url, init);",
                        "  const rl = parseRateLimit(res.headers);",
                        "  if (res.status === 429 || rl.remaining === 0) {",
                        "    await sleep(Math.max(rl.resetAt - Date.now(), 1000));",
                        "    return pacedFetch(url, init);",
                        "  }",
                        "  return res;",
                        "}",
                    ],
                },
                StubFile {
                    path: "packages/core/src/client.ts",
                    lines: vec![
                        "import { pacedFetch } from './stream.js';",
                        "",
                        "export class EdgeClient {",
                        "  constructor(private baseUrl: string) {}",
                        "",
                        "  get(path: string) {",
                        "    return pacedFetch(this.baseUrl + path, { method: 'GET' });",
                        "  }",
                        "}",
                    ],
                },
                StubFile {
                    path: "packages/core/src/vendor/note.md",
                    lines: vec![
                        "vendor snapshot \u{1b}[31mredacted\u{1b}[0m fork",
                        "if (user !== '\u{202e}admin\u{202c}') { deny(); }",
                    ],
                },
                StubFile {
                    path: "packages/core/src/vendor/escape-lab.md",
                    lines: vec![
                        "lab: control file exercising the output escaper",
                        "csi: \u{1b}[31mred\u{1b}[0m text",
                        "osc8: \u{1b}]8;;https://evil.example\u{7}click me\u{1b}]8;;\u{7}",
                        "rlo: \u{202e}evil\u{202c}",
                        "lri: \u{2066}iso\u{2069}",
                        "bell: a\u{7}b",
                        long_line(),
                    ],
                },
                StubFile {
                    path: "README.md",
                    lines: vec![
                        "# edge-sdk",
                        "",
                        "Streaming HTTP client with rate-limit awareness.",
                        "Respects x-ratelimit-remaining automatically.",
                    ],
                },
            ],
            docs: vec![
                StubDoc {
                    path: "docs/streaming.md",
                    url: "https://helix.dev/edge-sdk/docs/streaming",
                    lines: vec![
                        "# Streaming",
                        "",
                        "pacedFetch reads the x-ratelimit-remaining header on every response",
                        "and parks the caller when the budget is exhausted.",
                    ],
                },
                StubDoc {
                    path: "docs/llms.txt",
                    url: "https://helix.dev/edge-sdk/docs/llms.txt",
                    lines: vec![
                        "# edge-sdk docs",
                        "- Streaming: pacedFetch and rate-limit headers",
                        "- Client: EdgeClient basics",
                    ],
                },
            ],
        },
        StubRepo {
            repo: "ferrum/poolite",
            language: "Rust",
            stars: 320,
            license: "MIT",
            license_class: "permissive",
            topics: vec!["rust", "connection-pool", "async"],
            last_release: "2026-07-14",
            days_since_activity: 41,
            archived: false,
            what: "Small async connection pool with worker recycling and leases",
            catch: "single-backend per pool; no runtime-generic API yet",
            fitness: 74,
            readme_digest: "poolite is a minimal async connection pool. spawn_worker creates detached connection workers; leases expose checked-out connections with automatic return-on-drop. tokio and smol runtimes supported via feature flags.",
            release_cadence: "irregular; last release 8 weeks ago",
            runtime_support: vec!["tokio feature", "smol feature", "msrv 1.80"],
            files: vec![
                StubFile {
                    path: "src/pool.rs",
                    lines: vec![
                        "use std::sync::Arc;",
                        "use tokio::sync::Semaphore;",
                        "",
                        "pub struct Pool<C> {",
                        "    slots: Semaphore,",
                        "    conns: Vec<C>,",
                        "}",
                        "",
                        "pub fn spawn_worker<C>(pool: Arc<Pool<C>>) -> tokio::task::JoinHandle<()>",
                        "where",
                        "    C: Conn,", 
                        "{",
                        "    tokio::spawn(async move {",
                        "        loop {",
                        "            let lease = pool.acquire().await;",
                        "            lease.recycle().await;",
                        "        }",
                        "    })",
                        "}",
                        "",
                        "impl<C: Conn> Pool<C> {",
                        "    pub async fn acquire(&self) -> Lease<'_, C> {",
                        "        self.slots.acquire().await.unwrap();",
                        "        Lease { pool: self }",
                        "    }",
                        "}",
                    ],
                },
                StubFile {
                    path: "src/lib.rs",
                    lines: vec![
                        "mod pool;",
                        "pub use pool::{Pool, spawn_worker};",
                    ],
                },
                StubFile {
                    path: "README.md",
                    lines: vec![
                        "# poolite",
                        "",
                        "spawn_worker(pool) recycles idle connections.",
                    ],
                },
            ],
            docs: vec![StubDoc {
                path: "docs/leases.md",
                url: "https://ferrum.rs/poolite/docs/leases",
                lines: vec![
                    "# Leases",
                    "",
                    "A lease checks a connection out of the pool and returns it on drop.",
                    "spawn_worker keeps a warm population of recycled connections.",
                ],
            }],
        },
    ]
}

pub struct StubEngine;

impl Default for StubEngine {
    fn default() -> Self {
        StubEngine
    }
}

impl StubEngine {
    pub fn new() -> Self {
        StubEngine
    }
}

fn star_tier(stars: u64) -> &'static str {
    match stars {
        s if s >= 10_000 => "high",
        s if s >= 500 => "medium",
        _ => "low",
    }
}

/// 5000-char single line, built once and leaked so the static StubFile
/// fixture can reference it without leaking per corpus() call.
fn long_line() -> &'static str {
    static LONG: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    LONG.get_or_init(|| Box::leak("z".repeat(5000).into_boxed_str()))
}

fn activity(days: u64) -> &'static str {
    match days {
        d if d <= 30 => "active",
        d if d <= 180 => "steady",
        _ => "stale",
    }
}

fn cursor_offset(cursor: &Option<String>, input: &Value) -> Result<u64, ToolError> {
    match cursor {
        None => Ok(0),
        Some(c) => c.strip_prefix("off:").and_then(|n| n.parse().ok()).ok_or_else(|| {
            ToolError::InvalidValue {
                field: "cursor".into(),
                message: format!("malformed cursor `{c}`"),
                input: input.clone(),
                correction: "pass the next_cursor value from a previous response unmodified"
                    .into(),
            }
        }),
    }
}

fn make_cursor(offset: u64) -> String {
    format!("off:{offset}")
}

fn repo_row(r: &StubRepo, fields: &Option<Vec<String>>) -> Value {
    let mut row = json!({
        "repo": r.repo,
        "stars": r.stars,
        "star_tier": star_tier(r.stars),
        "license": r.license,
        "license_class": r.license_class,
        "last_release": r.last_release,
        "days_since_activity": r.days_since_activity,
        "activity": activity(r.days_since_activity),
        "archived": r.archived,
        "what": r.what,
        "catch": r.catch,
        "url": format!("https://github.com/{}", r.repo),
    });
    if let Some(fields) = fields {
        row.as_object_mut().unwrap().retain(|k, _| {
            let k = k.as_str();
            k == "repo"
                || fields.iter().any(|f| match f.as_str() {
                    "what" => k == "what",
                    "catch" => k == "catch",
                    "license" => k == "license" || k == "license_class",
                    "recency" => k == "last_release" || k == "days_since_activity" || k == "activity",
                    "stars" => k == "stars" || k == "star_tier",
                    "urls" => k == "url",
                    _ => false,
                })
        });
        row["fields_returned"] = json!(fields);
    }
    row
}

impl SearchEngine for StubEngine {
    fn backend_status(&self, tool: &str) -> Vec<BackendStatus> {
        match tool {
            "oss_search_repos" | "oss_search_code" => vec![
                BackendStatus::ok("grep.app"),
                BackendStatus::ok("sourcegraph"),
            ],
            "oss_repo_profile" | "oss_repo_tree" | "oss_fetch_file" => {
                vec![BackendStatus::ok("github-raw")]
            }
            "oss_fetch_docs" => vec![
                BackendStatus::ok("llms-txt"),
                BackendStatus {
                    backend: "docs-pages".into(),
                    status: BackendState::Degraded,
                    detail: Some("one page behind origin".into()),
                },
            ],
            _ => vec![BackendStatus::ok("static")],
        }
    }

    fn search_repos(&self, params: &SearchReposParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let offset = cursor_offset(&params.cursor, &input)?;
        let phrasings: Vec<String> = params
            .query
            .split(';')
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty())
            .collect();
        let all = corpus();
        let mut matched: Vec<&StubRepo> = all
            .iter()
            .filter(|r| {
                let hay = format!(
                    "{} {} {} {}",
                    r.repo,
                    r.what,
                    r.catch,
                    r.topics.join(" ")
                )
                .to_lowercase();
                phrasings.iter().any(|p| hay.contains(p))
                && params
                    .topic
                    .as_ref()
                    .map(|t| r.topics.iter().any(|top| top.eq_ignore_ascii_case(t)))
                    .unwrap_or(true)
                && params
                    .language
                    .as_ref()
                    .map(|l| r.language.eq_ignore_ascii_case(l))
                    .unwrap_or(true)
                && params.min_stars.map(|m| r.stars >= m).unwrap_or(true)
            })
            .collect();
        match params.sort {
            SortMode::Fitness => matched.sort_by_key(|r| std::cmp::Reverse(r.fitness)),
            SortMode::Stars => matched.sort_by_key(|r| std::cmp::Reverse(r.stars)),
            SortMode::Updated => matched.sort_by_key(|r| r.days_since_activity),
        }
        let total = matched.len() as u64;
        let end = ((offset + params.limit) as usize).min(matched.len());
        let rows: Vec<Value> = matched[offset as usize..end]
            .iter()
            .map(|r| repo_row(r, &params.fields))
            .collect();
        let next = if end < matched.len() {
            Some(make_cursor(end as u64))
        } else {
            None
        };
        Ok(EngineOutput::paged(rows, total, next))
    }

    fn search_code(&self, params: &SearchCodeParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let offset = cursor_offset(&params.cursor, &input)?;
        let probes: Vec<String> = params
            .probes
            .iter()
            .map(|p| {
                p.strip_prefix('/')
                    .and_then(|i| i.strip_suffix('/'))
                    .unwrap_or(p)
                    .to_string()
            })
            .collect();
        let corpus = corpus();
        let mut snippet_rows: Vec<Value> = Vec::new();
        let mut repo_rows: Vec<Value> = Vec::new();
        for r in &corpus {
            if let Some(filter) = &params.repo_filter
                && !r.repo.starts_with(filter.as_str()) && r.repo != *filter {
                    continue;
                }
            if let Some(lang) = &params.language
                && !r.language.eq_ignore_ascii_case(lang) {
                    continue;
                }
            let mut probes_hit: Vec<String> = Vec::new();
            let mut hit_files: Vec<&StubFile> = Vec::new();
            let mut first_hit: Option<(&StubFile, usize, &String)> = None;
            let mut repo_snippets = 0u64;
            for probe in &probes {
                let mut probe_hit = false;
                for file in &r.files {                    if let Some(p) = &params.path
                        && !file.path.contains(p.as_str()) {
                            continue;
                        }
                    for (li, line) in file.lines.iter().enumerate() {
                        if !line.contains(probe.as_str()) {
                            continue;
                        }
                        probe_hit = true;
                        if !hit_files.iter().any(|f| f.path == file.path) {
                            hit_files.push(file);
                        }
                        if first_hit.is_none() {
                            first_hit = Some((file, li + 1, probe));
                        }
                        if params.mode == CodeSearchMode::Snippets
                            && repo_snippets < params.snippets_per_repo
                            && snippet_rows.len() < params.limit as usize
                        {
                            repo_snippets += 1;
                            let lines: Vec<String> = file.lines.iter().map(|l| l.to_string()).collect();
                            let snip = format_snippet(file.path, &lines, (li + 1) as u64, DEFAULT_CONTEXT_LINES);
                            snippet_rows.push(json!({
                                "repo": r.repo,
                                "license": r.license,
                                "license_class": r.license_class,
                                "path": snip.path,
                                "locator": snip.locator,
                                "start_line": snip.start_line,
                                "end_line": snip.end_line,
                                "match_line": snip.match_line,
                                "probe": probe,
                                "lines": snip.lines,
                                "url": format!("https://github.com/{}/blob/main/{}", r.repo, file.path),
                            }));
                        }
                    }
                }
                if probe_hit {
                    probes_hit.push(probe.clone());
                }
            }
            if probes_hit.is_empty() {
                continue;
            }
            if let Some((file, line_no, probe)) = first_hit {
                let sample_evidence = format!(
                    "{}:{} {} <- probe '{}'",
                    file.path,
                    line_no,
                    file.lines[line_no - 1],
                    probe
                );
                repo_rows.push(json!({
                    "repo": r.repo,
                    "probes_hit": probes_hit,
                    "probe_breadth": probes_hit.len(),
                    "total_probes": probes.len(),
                    "hit_files": hit_files.len(),
                    "license": r.license,
                    "license_class": r.license_class,
                    "stars": r.stars,
                    "days_since_activity": r.days_since_activity,
                    "sample_evidence": sample_evidence,
                }));
            }
        }
        if params.mode == CodeSearchMode::Snippets {
            let total = snippet_rows.len() as u64;
            let end = ((offset + params.limit) as usize).min(snippet_rows.len());
            let rows: Vec<Value> = snippet_rows[offset as usize..end].to_vec();
            let next = if end < snippet_rows.len() {
                Some(make_cursor(end as u64))
            } else {
                None
            };
            return Ok(EngineOutput::paged(rows, total, next));
        }
        repo_rows.sort_by(|a, b| {
            let breadth_a = a["probe_breadth"].as_u64().unwrap_or(0);
            let breadth_b = b["probe_breadth"].as_u64().unwrap_or(0);
            breadth_b
                .cmp(&breadth_a)
                .then_with(|| {
                    b["stars"].as_u64().unwrap_or(0).cmp(&a["stars"].as_u64().unwrap_or(0))
                })
        });
        let total = repo_rows.len() as u64;
        let end = ((offset + params.limit) as usize).min(repo_rows.len());
        let rows: Vec<Value> = repo_rows[offset as usize..end].to_vec();
        let next = if end < repo_rows.len() {
            Some(make_cursor(end as u64))
        } else {
            None
        };
        Ok(EngineOutput::paged(rows, total, next))
    }

    fn repo_profile(&self, params: &RepoProfileParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let r = corpus()
            .into_iter()
            .find(|r| r.repo.eq_ignore_ascii_case(&params.repo))
            .ok_or_else(|| ToolError::NotFound {
                kind: "repo",
                name: params.repo.clone(),
                input,
                correction: "call oss_search_repos to find the exact owner/name first".into(),
            })?;
        let mut top_dirs: Vec<String> = r
            .files
            .iter()
            .map(|f| f.path)
            .chain(r.docs.iter().map(|d| d.path))
            .filter_map(|p| p.split('/').next().map(|s| s.to_string()))
            .filter(|s| !s.contains('.'))
            .collect();
        top_dirs.sort();
        top_dirs.dedup();
        let mut row = json!({
            "repo": r.repo,
            "what": r.what,
            "catch": r.catch,
            "stars": r.stars,
            "star_tier": star_tier(r.stars),
            "license": r.license,
            "license_class": r.license_class,
            "archived": r.archived,
            "last_release": r.last_release,
            "days_since_activity": r.days_since_activity,
            "activity": activity(r.days_since_activity),
            "readme_digest": r.readme_digest,
            "architecture_top_level": top_dirs,
            "release_cadence": r.release_cadence,
            "runtime_support": r.runtime_support,
        });
        if params.deep_docs {
            if let Some(llms) = r.docs.iter().find(|d| d.path.ends_with("llms.txt")) {
                row["deep_docs"] = json!({
                    "source": llms.url,
                    "digest": llms.lines.join(" "),
                });
            } else {
                row["deep_docs"] = json!("no llms.txt published by this repo");
            }
        }
        Ok(EngineOutput::complete(vec![row]))
    }

    fn repo_tree(&self, params: &RepoTreeParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let r = corpus()
            .into_iter()
            .find(|r| r.repo.eq_ignore_ascii_case(&params.repo))
            .ok_or_else(|| ToolError::NotFound {
                kind: "repo",
                name: params.repo.clone(),
                input,
                correction: "call oss_search_repos to find the exact owner/name first".into(),
            })?;
        let mut paths: Vec<&str> = r
            .files
            .iter()
            .map(|f| f.path)
            .chain(r.docs.iter().map(|d| d.path))
            .collect();
        paths.sort_unstable();
        if let Some(prefix) = &params.path_prefix {
            paths.retain(|p| p.starts_with(prefix.as_str()));
        }
        if let Some(glob) = &params.glob {
            let pattern = glob.trim_start_matches('*');
            paths.retain(|p| p.split('/').next_back().map(|n| n.ends_with(pattern)).unwrap_or(false));
        }
        let total = paths.len() as u64;
        let end = (params.limit as usize).min(paths.len());
        let rows: Vec<Value> = paths[..end].iter().map(|p| json!({ "path": p })).collect();
        let has_more = end < paths.len();
        Ok(EngineOutput {
            results: rows,
            total,
            has_more,
            partial: false,
            next_cursor: has_more.then(|| make_cursor(end as u64)),
        })
    }

    fn fetch_file(&self, params: &FetchFileParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let r = corpus()
            .into_iter()
            .find(|r| r.repo.eq_ignore_ascii_case(&params.repo))
            .ok_or_else(|| ToolError::NotFound {
                kind: "repo",
                name: params.repo.clone(),
                input: input.clone(),
                correction: "call oss_search_repos to find the exact owner/name first".into(),
            })?;
        let file = r
            .files
            .iter()
            .find(|f| f.path == params.path)
            .ok_or_else(|| ToolError::NotFound {
                kind: "file",
                name: format!("{}/{}", params.repo, params.path),
                input,
                correction: format!(
                    "call oss_repo_tree for {} first — a 404 from a guessed path proves nothing",
                    params.repo
                ),
            })?;
        let total_lines = file.lines.len() as u64;
        let (start, end) = match &params.line_range {
            Some(range) => (range.start.min(total_lines.max(1)), range.end.min(total_lines)),
            None => (1, total_lines.min(100)),
        };
        let content = if total_lines == 0 {
            String::new()
        } else {
            file.lines[(start as usize - 1)..(end as usize)].join("\n")
        };
        let row = json!({
            "repo": r.repo,
            "path": file.path,
            "ref": params.ref_.clone().unwrap_or_else(|| "main".into()),
            "start_line": start,
            "end_line": end,
            "total_lines": total_lines,
            "content": content,
        });
        Ok(EngineOutput::complete(vec![row]))
    }

    fn fetch_docs(&self, params: &FetchDocsParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let r = corpus()
            .into_iter()
            .find(|r| r.repo.eq_ignore_ascii_case(&params.repo))
            .ok_or_else(|| ToolError::NotFound {
                kind: "repo",
                name: params.repo.clone(),
                input: input.clone(),
                correction: "call oss_search_repos to find the exact owner/name first".into(),
            })?;
        let needle = params
            .pattern
            .strip_prefix('/')
            .and_then(|i| i.strip_suffix('/'))
            .unwrap_or(&params.pattern)
            .to_lowercase();
        let mut rows: Vec<Value> = Vec::new();
        for doc in &r.docs {
            let hits: Vec<Value> = doc
                .lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.to_lowercase().contains(&needle))
                .map(|(i, l)| {
                    json!({
                        "line": i + 1,
                        "text": l,
                        "breadcrumb": format!("Source: {}", doc.url),
                    })
                })
                .collect();
            if !hits.is_empty() {
                rows.push(json!({
                    "repo": r.repo,
                    "page": doc.path,
                    "source_url": doc.url,
                    "hits": hits,
                }));
            }
        }
        Ok(EngineOutput::complete(rows))
    }

    fn guide(&self, _params: &GuideParams) -> Result<EngineOutput, ToolError> {
        let row = json!({
            "which_tool_when": [
                "oss_search_repos: discovery — 'does a tool for X exist?' (README-visible categories)",
                "oss_search_code: vetting — 'who actually implements idiom X?' (implementation details)",
                "oss_repo_profile: one-repo brief before adopting",
                "oss_repo_tree: file map before guessing any path",
                "oss_fetch_file: byte-exact file read, prefer line_range",
                "oss_fetch_docs: doc search with Source: breadcrumbs",
            ],
            "probe_writing": {
                "good": ["spawn_worker", "special_closes", "x-ratelimit-remaining", "/early[Cc]loses/"],
                "bad": ["market calendars", "rust http library", "error handling"],
                "rule": "search for code, not concepts; uniqueness beats correctness; 2-5 orthogonal probes per call",
            },
            "regex_backend_matrix": {
                "grep.app": "regex via /.../ probes",
                "sourcegraph": "regex via /.../ probes; pass count:100 style limits server-side",
                "github_code_search": "literal only, no regex, ~10 req/min, 256-char queries, needs a bare term",
            },
            "rate_limit_traps": "grep.app 504/429s: the server retries once then degrades to Sourcegraph; an empty result with an error backend_status is a fetch failure, not a negative finding",
            "license_classes": {
                "permissive": "MIT/Apache-2.0/BSD — safe to vendor",
                "copyleft": "GPL family — pattern-only study, linking has obligations",
                "AGPL": "network copyleft — treat as toxic for SaaS embedding",
            },
            "response_budget": "default calls stay under ~25KB; truncated=true means results were dropped to fit — narrow the query or page with next_cursor",
        });
        Ok(EngineOutput::complete(vec![row]))
    }
}
