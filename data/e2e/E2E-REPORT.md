# E2E Final Verification Report — oss-search

**Date:** 2026-09-10 · **Gate:** goal action a12 · **Host:** macOS (darwin, arm64)
**Workspace:** ~/oss-search (10 crates) · **Profile:** release

Every check below was run through a **production entry point** (the release
`oss-mcp` binary over stdio JSON-RPC, the release `oss-cli`, the scripted
`oss-index` bench, `oss_eval::run_all`), with the raw evidence saved under
`data/e2e/`. Nothing was verified through an in-process shortcut.

## Headline table

| # | Check | Command (production entry point) | Exit | Key evidence | Control: what failure would look like |
|---|-------|----------------------------------|------|--------------|----------------------------------------|
| 1 | Release build | `cargo build --release` | 0 | 17.56 s wall / 2m30s CPU (deps cached; all 11 workspace crates recompiled); binaries 9.4 MB `oss-mcp`, 7.9 MB `oss-cli` (`logs/build-release.log`) | Non-zero exit; `exit=N` line recorded in log |
| 2 | MCP stdio session (all 7 tools) | `oss-mcp/examples/e2e_session.rs` → spawns `target/release/oss-mcp` | 0 | initialize + tools/list asserting **exactly** the 7 tool names; 7/7 `tools/call` schema-valid envelopes, `isError=false`, all ≤ 25,000 B; session 288 ms, per-call 0.05–0.19 ms. Full transcript: `mcp-transcript.jsonl` (19 raw JSON-RPC lines, chronological), timings: `mcp-session.json` | Wrong tool count/names → driver exits 1 with both lists printed; missing envelope field → driver names the field; `isError=true` → payload printed; oversized payload → byte count printed |
| 3 | CLI parity (all 7 tools) | `oss-cli/examples/e2e_cli.rs` → runs `target/release/oss-cli` + `verify_parity.py` | 0 | 7/7 exit-0 runs, envelopes byte-identical in size to the MCP envelopes (933/802/926/222/695/917/1600 B); parity verifier: identical top-level schema (required 7 fields, optional `note`/`next_cursor`) across MCP transcript ↔ CLI outputs | CLI nonzero exit → stderr captured; field-set mismatch → verifier names tool + both field sets |
| 4 | Live backends (network, polite: 3 CLI invocations, ~20 backend HTTP requests) | `GITHUB_TOKEN=… oss-cli --live search-code --pattern retry --limit 3` (repos + snippets mode) and `--live search-repos --query express --limit 5` | 0 | **Truthful:** github `ok`; grep.app `error` (429, bot-challenged) → `partial=true` correctly set; npms.io + ecosyste.ms `ok` → `partial=false`. **Attributed:** every hit carries `owner/name` + `backends` list + pinned `…/blob/<sha>/…` URL. **No faked content:** all 3 code hits re-fetched from their **pinned SHAs** — snippet line verbatim in the real file and `retry` present in all 3 files (`live/hit-verification.json`); expressjs/express returned (stars 69,417, dependents 93,237) | Backend down but status `ok` → consistency check fails ("partial flag untruthful"); fabricated hit → verbatim/pinned-SHA check fails and names the repo; empty results → driver exits 1 |
| 5 | Hot-set bench (real corpus index) | `oss-index/examples/bench_hotset.rs` (release) + `regex_demo.rs` | 0 | Index loaded: **13,408 docs in 1.12 s**; **51 queries** (incl. 10 regex) each asserted ≥ 1 hit: p50 **33.68 ms**, p90 72.31 ms, p99 104.66 ms, total 1.99 s — **p50 < 100 ms budget PASS** (`hotset/LATENCY.md` regenerated). Regex demo: 4 regex queries with visible hits, e.g. `urllib3:src/urllib3/util/retry.py:L43 class Retry:` | Pattern with zero hits → example panics naming the pattern; p50 ≥ 100 ms → final assert fails |
| 6 | Security (hostile bytes) | `oss-mcp/examples/e2e_security.rs` → release `oss-mcp`; request query contains raw `U+202E rdm U+202C ESC[31m` | 0 | **2,198 raw response bytes policed: ESC ×0, BEL ×0, RLO ×0, PDF ×0, LRI ×0, PDI ×0.** Error-echo path visibly escapes (`<U+202E>`, `isError=true`, code `unknown_field`); fixture success path shows `<U+202E>`, `<U+2066>`, `chars truncated]` + `bidi`/`capped` warnings. Raw bytes: `security/hostile-query-raw-response.txt`; verdict: `security/verdict.json` | A single surviving ESC/RLO byte → count > 0 → driver exits 1 printing the per-byte counts; missing visible marker/warning → named failure |
| 7 | Eval suite | `oss-eval/examples/run_eval.rs` → `oss_eval::run_all` (release) | 0 | ndcg@10 **1.0000** (bar ≥ 0.5), mrr 1.0000, recall@50 1.0000, repo hit_rate@10 1.0000, latency 60 queries p50 **1,515 µs** (bar < 100,000). Wrote `eval/EVAL.md` + `code_eval.json` + `repo_eval.json` + `latency.json` | Metric under bar → assert fires with the value; missing artifact → named file |
| 8 | Workspace tests | `cargo test --workspace` | 0 | **355 passed, 0 failed, 1 ignored** (unchanged baseline) in 35.0 s (`logs/cargo-test-workspace.log`) | Any failure → nonzero exit + failed count |
| 9 | Lint | `cargo clippy --workspace --all-targets -- -D warnings` | 0 | Clean, no warnings (`logs/clippy.log`) | Any warning → nonzero exit |

## Build & timing summary

| Step | Wall time |
|---|---|
| `cargo build --release` (incremental, deps cached, 11 crates) | **17.56 s** (2m30s CPU) |
| `cargo build --release --examples` (mcp/cli/eval drivers) | ~9 s |
| `cargo build --release -p oss-index --example regex_demo` | 0.89 s |
| Full MCP session (initialize → list → 7 calls) | 288 ms |
| 7 CLI invocations | 301 ms total (291 ms first incl. engine init) |
| Eval `run_all` | 0.2 s |

## Artifacts (all under `data/e2e/`)

- `mcp-transcript.jsonl` — 19 chronological raw JSON-RPC wire lines (10 requests, 9 responses)
- `cli/01..07-*.json` + `summary.json` — CLI outputs, exit codes, per-call timings
- `live/*.json` — live backend captures; `live/hit-verification.json` — pinned-SHA no-fake proof
- `hotset/LATENCY.md` — regenerated 51-query latency table
- `security/` — raw hostile response bytes, repro, verdict
- `eval/` — EVAL.md + 3 JSON reports
- `logs/` — every command's full output incl. `exit=` codes
- `verify_parity.py`, `verify_live.py`, `verify_hits.py` — re-runnable verifiers

## Additive code (no product code touched, no fixes needed)

- `oss-mcp/examples/e2e_session.rs`, `oss-mcp/examples/e2e_security.rs`
- `oss-cli/examples/e2e_cli.rs`
- `oss-eval/examples/run_eval.rs`
- `oss-index/examples/regex_demo.rs`

## Surprises / observations

1. **grep.app live backend 429** (undocumented endpoint, Vercel bot-challenge) during the live gate. Not a blocker: the engine reported it truthfully (`status: error` + `partial: true` + actionable detail). Genuine third-party degradation, handled exactly per the truthfulness contract.
2. **Snippet first-line trimming quirk (observation, not fixed):** `oss_search_code` snippets mode returns `first_line()` of a multi-line GitHub text-match fragment, which can be a *context* line that doesn't itself contain the probe term. Verified NOT fabrication via pinned-SHA cross-check (snippet line verbatim + probe term present in file). Flagged per instructions; a candidate future improvement (pick the matching line), deliberately not changed under the additive-only mandate.
3. Environment, not product: `cargo` is absent from non-interactive `$PATH` on this host (first background build attempt exited 127); rerun with `$HOME/.cargo/bin` prepended. The workspace root is not itself a git repo — each crate is its own repo, so the additive examples are untracked files in `oss-mcp`, `oss-cli`, `oss-eval`, `oss-index`.
4. One pre-existing ignored test in the suite (355 passed + 1 ignored) — matches the stated 355-green baseline.

## Re-running the gate

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release && cargo build --release --examples
./target/release/examples/e2e_session          # gate 2
./target/release/examples/e2e_cli              # gate 3
python3 data/e2e/verify_parity.py              # gate 3 parity
GITHUB_TOKEN=$(gh auth token) ./target/release/oss-cli --live search-code --pattern retry --mode snippets --limit 3 > data/e2e/live/search-code-retry-snippets.json
GITHUB_TOKEN=$(gh auth token) python3 data/e2e/verify_hits.py   # gate 4 (re-fetch pinned SHAs)
python3 data/e2e/verify_live.py                # gate 4
./target/release/examples/bench_hotset         # gate 5
./target/release/examples/regex_demo           # gate 5
./target/release/examples/e2e_security         # gate 6
./target/release/examples/run_eval             # gate 7
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings  # gates 8–9
```

**Verdict: E2E GATE PASS — 9/9 checks green, 355/355 tests green, clippy clean, zero product-code changes.**
