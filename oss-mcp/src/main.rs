use std::sync::Arc;

use oss_core::{SearchEngine, StubEngine};
use oss_mcp::version::VERSION;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ContentBlock, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, TextContent, Tool,
};
use rmcp::service::{QuitReason, RequestContext, RoleServer};
use rmcp::transport::io::stdio;
use rmcp::ServiceExt;
use serde_json::Value;

#[derive(Clone)]
struct OssServer {
    engine: Arc<dyn SearchEngine>,
}

impl ServerHandler for OssServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new("oss-mcp", VERSION))
            .with_instructions(
                "OSS search for agents: discover repos, vet by code idioms, drill into files and docs. Call oss_guide once when unsure which tool to use.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let tools = oss_core::tool_specs()
            .into_iter()
            .map(|spec| {
                Tool::new(
                    spec.name,
                    spec.description,
                    Arc::new(schema_to_map(spec.input_schema)),
                )
            })
            .collect();
        Ok(ListToolsResult {
            tools,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let name = request.name.to_string();
        let arguments = request
            .arguments
            .map(|obj| Value::Object(obj.into_iter().collect()));
        // The live engine blocks on its own private tokio runtime, so the
        // synchronous dispatch must run on a blocking thread, not this one.
        let engine = self.engine.clone();
        let result =
            tokio::task::spawn_blocking(move || oss_core::call_tool(engine.as_ref(), &name, arguments))
                .await
                .map_err(|e| rmcp::ErrorData::internal_error(format!("engine join error: {e}"), None))?
                ;
        let (payload, is_ok) = match result {
            Ok(v) => (v, true),
            Err(e) => (e.to_json(), false),
        };
        let text = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
        let block = ContentBlock::Text(TextContent::new(text));
        let call_result = if is_ok {
            rmcp::model::CallToolResult::success(vec![block])
        } else {
            rmcp::model::CallToolResult::error(vec![block])
        };
        Ok(call_result.into())
    }
}

fn schema_to_map(v: Value) -> serde_json::Map<String, Value> {
    match v {
        Value::Object(map) => map,
        _ => Default::default(),
    }
}

const USAGE: &str = "\
oss-mcp — OSS search MCP server for agents

USAGE:
    oss-mcp [--live]

OPTIONS:
        --live      Use the live engine (real backends: GitHub code search, grep.app,
                    npms.io, ecosyste.ms, deps.dev). Default is the offline stub engine.
    -h, --help      Print this help and exit (never starts the stdio server)
    -V, --version   Print version and exit

With no arguments, oss-mcp serves MCP over stdio; run it from your agent host
and it exits when stdin closes. Set GITHUB_TOKEN in the environment for --live
GitHub code search.";

fn main() {
    // Invocation guard: resolve --help/--version and reject unknown arguments
    // BEFORE any environment read or engine construction, so inspection and
    // typo'd invocations can never start (or block on) the stdio server.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("oss-mcp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Some(bad) = args.iter().find(|a| a.as_str() != "--live") {
        eprintln!("oss-mcp: unrecognized argument '{bad}'\n\n{USAGE}");
        std::process::exit(2);
    }
    let live = args.iter().any(|a| a == "--live");
    let handler = if live {
        if std::env::var("GITHUB_TOKEN").map_or(true, |t| t.is_empty()) {
            eprintln!(
                "warning: GITHUB_TOKEN is not set; GitHub code search requires auth and will answer 401 (unauthenticated contents/tree budget is 60/hr)"
            );
        }
        OssServer {
            engine: Arc::new(oss_live::LiveEngine::new()),
        }
    } else {
        OssServer {
            engine: Arc::new(StubEngine::new()),
        }
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(async move {
        let transport = stdio();
        let server = match handler.serve(transport).await {
            Ok(server) => server,
            Err(e) => {
                eprintln!("oss-mcp: stdio server failed to start: {e}");
                std::process::exit(1);
            }
        };
        let code = match server.waiting().await {
            Ok(QuitReason::Closed) | Ok(QuitReason::Cancelled) => 0,
            Ok(QuitReason::JoinError(e)) | Err(e) => {
                eprintln!("oss-mcp: server stopped abnormally: {e}");
                1
            }
            _ => 0,
        };
        std::process::exit(code);
    });
}
