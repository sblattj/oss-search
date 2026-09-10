use clap::{Parser, Subcommand};
use oss_core::{call_tool, SearchEngine, StubEngine};
use serde_json::{json, Value};

mod hotset;

use hotset::HotsetAction;

#[derive(Parser)]
#[command(name = "oss-cli", version, about = "Agent-facing OSS search over the 7-tool surface")]
struct Cli {
    /// Use the live engine (real backends: github, grep.app, npms.io,
    /// ecosyste.ms, deps.dev). Default is the offline stub engine.
    #[arg(long)]
    live: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Build the local hot-set corpus (staged, resumable)")]
    Hotset {
        #[command(subcommand)]
        action: HotsetAction,
    },
    #[command(about = "Find repositories by topic, name, or description")]
    SearchRepos {
        #[arg(long)]
        query: String,
        #[arg(long)]
        topic: Option<String>,
        #[arg(long, alias = "lang")]
        language: Option<String>,
        #[arg(long)]
        min_stars: Option<u64>,
        #[arg(long)]
        sort: Option<String>,
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, value_delimiter = ',')]
        fields: Option<Vec<String>>,
        #[arg(long)]
        response_format: Option<String>,
    },
    #[command(about = "Find repos by distinctive code idioms (probes)")]
    SearchCode {
        #[arg(long = "probe", value_name = "PROBE", alias = "pattern")]
        probes: Vec<String>,
        #[arg(long, alias = "lang")]
        language: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        repo_filter: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        snippets_per_repo: Option<u64>,
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        exclude_known: Option<String>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        response_format: Option<String>,
    },
    #[command(about = "Pre-digested brief for one known repo")]
    RepoProfile {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        deep_docs: bool,
    },
    #[command(about = "Whole-repo file list, path-filtered")]
    RepoTree {
        #[arg(long)]
        repo: String,
        #[arg(long = "ref", value_name = "REF")]
        ref_: Option<String>,
        #[arg(long)]
        path_prefix: Option<String>,
        #[arg(long)]
        glob: Option<String>,
        #[arg(long)]
        limit: Option<u64>,
    },
    #[command(about = "Read one file's exact contents, optionally line-ranged")]
    FetchFile {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        path: String,
        #[arg(long = "ref", value_name = "REF")]
        ref_: Option<String>,
        #[arg(long)]
        line_start: Option<u64>,
        #[arg(long)]
        line_end: Option<u64>,
    },
    #[command(about = "Search project docs with Source: breadcrumbs")]
    FetchDocs {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        pattern: String,
        #[arg(long)]
        docs_host: Option<String>,
    },
    #[command(about = "Probe-writing and query-syntax playbook")]
    Guide {},
}

fn put(map: &mut serde_json::Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        map.insert(key.to_string(), v);
    }
}

fn opt_str(s: &Option<String>) -> Option<Value> {
    s.clone().map(Value::String)
}

fn args_for(command: &Command) -> (String, Value) {
    match command {
        Command::SearchRepos {
            query,
            topic,
            language,
            min_stars,
            sort,
            limit,
            cursor,
            fields,
            response_format,
        } => {
            let mut m = serde_json::Map::new();
            m.insert("query".into(), json!(query));
            put(&mut m, "topic", opt_str(topic));
            put(&mut m, "language", opt_str(language));
            put(&mut m, "min_stars", min_stars.map(|v| json!(v)));
            put(&mut m, "sort", opt_str(sort));
            put(&mut m, "limit", limit.map(|v| json!(v)));
            put(&mut m, "cursor", opt_str(cursor));
            put(
                &mut m,
                "fields",
                fields
                    .as_ref()
                    .map(|f| json!(f)),
            );
            put(&mut m, "response_format", opt_str(response_format));
            ("oss_search_repos".into(), Value::Object(m))
        }
        Command::SearchCode {
            probes,
            language,
            path,
            repo_filter,
            mode,
            snippets_per_repo,
            limit,
            exclude_known,
            cursor,
            response_format,
        } => {
            let mut m = serde_json::Map::new();
            m.insert("probes".into(), json!(probes));
            put(&mut m, "language", opt_str(language));
            put(&mut m, "path", opt_str(path));
            put(&mut m, "repo_filter", opt_str(repo_filter));
            put(&mut m, "mode", opt_str(mode));
            put(&mut m, "snippets_per_repo", snippets_per_repo.map(|v| json!(v)));
            put(&mut m, "limit", limit.map(|v| json!(v)));
            put(&mut m, "exclude_known", opt_str(exclude_known));
            put(&mut m, "cursor", opt_str(cursor));
            put(&mut m, "response_format", opt_str(response_format));
            ("oss_search_code".into(), Value::Object(m))
        }
        Command::RepoProfile { repo, deep_docs } => {
            let mut m = serde_json::Map::new();
            m.insert("repo".into(), json!(repo));
            put(&mut m, "deep_docs", Some(json!(deep_docs)));
            ("oss_repo_profile".into(), Value::Object(m))
        }
        Command::RepoTree {
            repo,
            ref_,
            path_prefix,
            glob,
            limit,
        } => {
            let mut m = serde_json::Map::new();
            m.insert("repo".into(), json!(repo));
            put(&mut m, "ref", opt_str(ref_));
            put(&mut m, "path_prefix", opt_str(path_prefix));
            put(&mut m, "glob", opt_str(glob));
            put(&mut m, "limit", limit.map(|v| json!(v)));
            ("oss_repo_tree".into(), Value::Object(m))
        }
        Command::FetchFile {
            repo,
            path,
            ref_,
            line_start,
            line_end,
        } => {
            let mut m = serde_json::Map::new();
            m.insert("repo".into(), json!(repo));
            m.insert("path".into(), json!(path));
            put(&mut m, "ref", opt_str(ref_));
            if line_start.is_some() || line_end.is_some() {
                let start = line_start.unwrap_or(1);
                let end = line_end.unwrap_or(start);
                put(&mut m, "line_range", Some(json!({"start": start, "end": end})));
            }
            ("oss_fetch_file".into(), Value::Object(m))
        }
        Command::FetchDocs {
            repo,
            pattern,
            docs_host,
        } => {
            let mut m = serde_json::Map::new();
            m.insert("repo".into(), json!(repo));
            m.insert("pattern".into(), json!(pattern));
            put(&mut m, "docs_host", opt_str(docs_host));
            ("oss_fetch_docs".into(), Value::Object(m))
        }
        Command::Guide {} => ("oss_guide".into(), json!({})),
        Command::Hotset { .. } => unreachable!("hotset is handled directly in main"),
    }
}

fn main() {
    let cli = Cli::parse();
    if let Command::Hotset { action } = &cli.command {
        let code = match action {
            HotsetAction::Build(args) => hotset::run(args),
        };
        std::process::exit(code);
    }
    let (tool, args) = args_for(&cli.command);
    let engine: Box<dyn SearchEngine> = if cli.live {
        if std::env::var("GITHUB_TOKEN").map_or(true, |t| t.is_empty()) {
            eprintln!(
                "warning: GITHUB_TOKEN is not set; GitHub code search requires auth and will answer 401 (unauthenticated contents/tree budget is 60/hr)"
            );
        }
        Box::new(oss_live::LiveEngine::new())
    } else {
        Box::new(StubEngine::new())
    };
    match call_tool(engine.as_ref(), &tool, Some(args)) {
        Ok(payload) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).expect("serialize envelope")
            );
        }
        Err(e) => {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&e.to_json()).expect("serialize error")
            );
            std::process::exit(1);
        }
    }
}
