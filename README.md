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

```sh
cargo build --release
# binaries: target/release/oss-mcp  target/release/oss-cli
```

## Quickstart (CLI)

```sh
# Repo discovery — which wheel exists?
oss-cli search-repos --query "express" --limit 5

# Code search (federated, needs GITHUB_TOKEN for the github backend)
GITHUB_TOKEN=... oss-cli --live search-code --pattern "retry" --limit 3

# Local hot-set (after building a corpus; see data/corpus/REPORT.md)
oss-cli search-code --pattern "retry_with_backoff" --in hotset
```

## Quickstart (MCP)

`oss-mcp` speaks MCP over stdio. Point your agent host at the binary:

```json
{ "mcpServers": { "oss-search": { "command": "/path/to/target/release/oss-mcp" } } }
```

Add `--live` to enable remote backends (GitHub REST code search, grep.app, deps.dev, ecosyste.ms, npms.io).

## Query language

```
retry repo:tokio-rs/tokio lang:rust path:src/ case:yes scope:def in:hotset type:regex
```

Structured JSON fields are canonical (arrays = OR, across fields = AND, `-` prefixes = excludes). Unknown fields are rejected loudly with the known-field list — never silently dropped.

## Verification

The repo ships its own evidence: `data/e2e/E2E-REPORT.md` (9/9 gate through the release binaries), golden-set eval (`data/e2e/eval/EVAL.md`), and the hot-set latency table (`data/e2e/hotset/LATENCY.md`).

```sh
cargo test --workspace   # 355 tests
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
