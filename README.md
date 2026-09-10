# oss-search

Fast local + federated OSS code search and repo discovery, built for AI agents (MCP) with a CLI twin.

Not recreating the wheel is a superpower. `oss-search` answers two questions for an agent:

1. **Which library already solves this?** — multi-signal repo ranking (dependents, centrality, maintenance, security posture), not just star count.
2. **Show me real code that does this** — sub-50ms regex search over a local hot-set corpus, federated with live backends (GitHub code search, grep.app) for the long tail.

## Highlights

- **7 MCP tools** (`oss_search_repos`, `oss_search_code`, `oss_repo_profile`, `oss_repo_tree`, `oss_fetch_file`, `oss_fetch_docs`, `oss_guide`) over stdio, plus a byte-identical CLI.
- **Local trigram index**: pure-Rust positional-trigram engine. Measured p50 ≈ 34ms over a 13,408-file / 100-repo hot-set; incremental reindex touches only changed blobs.
- **Hybrid semantic layer**: tree-sitter function-level chunking (Rust/Python/JS/TS/Go/C), local embeddings (fastembed, BGE-small), RRF fusion with lexical-first routing — identifier queries never pay model latency.
- **Repo discovery ranking**: dependents count, dependency-graph PageRank (CSR power iteration), OpenSSF-style maintenance/security signals, awesome-list membership, archived/stale gates — with per-signal contributions inspectable in every result.
- **Corpus pipeline**: blobless depth-1 clones → BLAKE3 content-addressed zstd store → 3-tier dedup (content / token / MinHash-LSH with exact Jaccard verification) → SPDX license detection.
- **Strict agent-facing contract**: structured filters, typed pre-execution errors (`unknown_field` / `regex_error` / `not_indexable`) that echo the input and name the correction, declared `total`/`hasMore`/`partial`, ~25KB response cap, never silent degradation.
- **Security posture**: RE2-class regex only (linear time, ReDoS-immune), file admission gates, output escaping (ANSI/control chars, bidi overrides) — all verified by test.

## Install

Two front doors: `oss-mcp` (MCP server) and `oss-cli` (CLI twin). Three ways to get them:

**1. npm** — an `oss-search` shim that downloads the platform-matched prebuilt
binary on first run and caches it under `~/.cache/oss-search/bin/` (override
with `OSS_SEARCH_CACHE_DIR`):

```sh
npx -y oss-search --version
```

> **Status (pre-publish):** the `oss-search` npm package and its matching GitHub
> release (v0.2.0) are being published now. Until they land, `npx` will fail
> with a 404 on the release asset — use option 2 or 3 below.

**2. cargo install from git** (requires a Rust toolchain):

```sh
cargo install --git https://github.com/sblattj/oss-search oss-mcp
cargo install --git https://github.com/sblattj/oss-search oss-cli
```

Binaries land in `~/.cargo/bin/`.

**3. Prebuilt binaries** — every release publishes
`oss-search-<version>-<target>.tar.gz` plus `checksums.txt` (sha256) at
<https://github.com/sblattj/oss-search/releases/latest>, for targets
`aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`,
`x86_64-unknown-linux-gnu`:

```sh
curl -fsSL https://github.com/sblattj/oss-search/releases/latest/download/oss-search-0.2.0-aarch64-apple-darwin.tar.gz | tar xz
```

From a checkout instead:

```sh
cargo build --release
# binaries: target/release/oss-mcp  target/release/oss-cli
```

## Quickstart (CLI)

`oss-cli` mirrors the 7-tool surface one-to-one (offline stub engine by
default; `--live` switches to the real backends):

```sh
# Probe-writing and query-syntax playbook — offline, zero setup
oss-cli guide

# Repo discovery — which wheel exists?
oss-cli search-repos --query "express" --limit 5

# Code search by distinctive idioms (federated; GITHUB_TOKEN unlocks the github backend)
GITHUB_TOKEN=... oss-cli --live search-code --probe "retry_with_backoff" --limit 3
```

All commands emit the same structured JSON envelope (`results`, `total`,
`has_more`, `partial`, `backend_status`) that the MCP tools return.

## Quickstart (MCP)

`oss-mcp` speaks MCP over stdio. Invocation safety: `--help` / `--version`
print and exit 0 without ever starting the server; unknown arguments exit 2
with usage instead of silently serving a stub; with no arguments it serves
until stdin closes.

Claude Code — `.mcp.json` at the project root (use the absolute path your
install produced, or the `npx` form):

```json
{
  "mcpServers": {
    "oss-search": {
      "command": "/Users/YOU/.cargo/bin/oss-mcp",
      "args": ["--live"],
      "env": { "GITHUB_TOKEN": "ghp_..." }
    }
  }
}
```

npx form (no absolute path needed):

```json
{
  "mcpServers": {
    "oss-search": {
      "command": "npx",
      "args": ["-y", "oss-search", "--live"],
      "env": { "GITHUB_TOKEN": "ghp_..." }
    }
  }
}
```

Claude Desktop — `claude_desktop_config.json` (macOS:
`~/Library/Application Support/Claude/claude_desktop_config.json`; `~` is not
expanded inside the JSON — use the absolute path):

```json
{
  "mcpServers": {
    "oss-search": {
      "command": "/Users/YOU/.cargo/bin/oss-mcp",
      "args": ["--live"],
      "env": { "GITHUB_TOKEN": "ghp_..." }
    }
  }
}
```

opencode — `opencode.json` at the project root:

```json
{
  "mcp": {
    "oss-search": {
      "type": "local",
      "command": ["oss-mcp", "--live"],
      "environment": { "GITHUB_TOKEN": "ghp_..." },
      "enabled": true
    }
  }
}
```

`--live` enables the remote backends (GitHub REST code search, grep.app,
deps.dev, ecosyste.ms, npms.io); without it `oss-mcp` serves an offline stub.
`oss-mcp --help` prints the full invocation contract.

## Query language

```
retry repo:tokio-rs/tokio lang:rust path:src/ case:yes scope:def in:hotset type:regex
```

Structured JSON fields are canonical (arrays = OR, across fields = AND, `-` prefixes = excludes). Unknown fields are rejected loudly with the known-field list — never silently dropped.

## Verification

The repo ships its own evidence: `data/e2e/E2E-REPORT.md` (9/9 gate through the release binaries), golden-set eval (`data/e2e/eval/EVAL.md`), and the hot-set latency table (`data/e2e/hotset/LATENCY.md`).

```sh
cargo test --workspace   # 368 tests
```

## Workspace layout

| Crate | Role |
|---|---|
| `oss-query` | query schema, strict text⇄structured grammar, typed errors, backend planning |
| `oss-facade` | remote clients: GitHub, grep.app, deps.dev, ecosyste.ms, npms.io (rate-limited) |
| `oss-corpus` | cloning, CAS blob store, dedup, license detection |
| `oss-index` | positional-trigram index, regex planning, ranking, incremental updates |
| `oss-semantic` | tree-sitter chunking, embeddings, vector store, RRF fusion, query routing |
| `oss-rank` | repo signals, PageRank over dependency graph, multi-signal ranker |
| `oss-eval` | golden-set evaluation (nDCG/MRR/Recall), latency harness |
| `oss-core` | envelope, shaping, escaping, engine trait |
| `oss-live` | production engine wiring facade + ranker |
| `oss-mcp` / `oss-cli` | the two front doors |

## License

MIT
