# Hot-set corpus build report

Generated 2026-09-10T07:30:18Z by `oss-corpus/examples/build_hotset.rs` (CloneCoordinator -> CorpusIngestor -> CasStore).
Clone policy: unauthenticated `git clone --depth 1 --single-branch --no-checkout --filter=blob:none` over https, 8-way parallel.

- Corpus root: `~/oss-search/oss-corpus/../data/corpus`
- Repos attempted: 100
- Repos succeeded: 100
- Repos failed: 0
- Repos ingested (incl. 3 fixtures): 103
- Clone actions this pass: 0 cloned, 0 updated, 100 unchanged (pipeline is idempotent; a warm repos/ cache reports unchanged)
- Wall clock: 43.9s total (38.8s clone+materialize + 5.1s ingest)

## Per-ecosystem

| ecosystem | attempted | succeeded | failed | files accepted | bytes offered |
|---|---|---|---|---|---|
| go | 18 | 18 | 0 | 1978 | 7.93 MiB |
| javascript | 24 | 24 | 0 | 2924 | 20.04 MiB |
| python | 26 | 26 | 0 | 5108 | 43.76 MiB |
| rust | 32 | 32 | 0 | 3398 | 29.44 MiB |
| **total** | 100 | 100 | 0 | 13408 | 101.18 MiB |

## CasStore stats

| metric | value |
|---|---|
| blobs (distinct) | 13190 |
| packs | 103 |
| bytes_raw | 105120194 (100.25 MiB) |
| bytes_stored (zstd-3) | 27460368 (26.19 MiB) |
| bytes_offered (all puts) | 106101390 (101.19 MiB) |
| dedup_ratio (offered/raw) | 1.009 |
| compression (raw/stored) | 3.83x |

## Ingestion totals (repos only, fixtures excluded)

- files seen: 13408
- files accepted into manifest: 13408
- bytes offered: 106095260 (101.18 MiB)
- dedup verdicts: unique=12676 exact=223 token=238 near=271

## Top near-dup pairs (MinHash/LSH, threshold 0.85)

| file | duplicate_of | jaccard | estimated |
|---|---|---|---|
| rust-url:idna/tests/IdnaTestV2.txt | `idna/tests/IdnaTestV2-Unicode16.txt` | 0.997 | 1.000 |
| cobra:LICENSE.txt | `LICENSE-APACHE` | 0.996 | 1.000 |
| requests:src/requests/cookies.py | `src/pip/_vendor/requests/cookies.py` | 0.995 | 0.992 |
| attrs:docs/_static/attrs_logo_white.svg | `docs/_static/attrs_logo.svg` | 0.995 | 0.992 |
| rich:rich/progress.py | `src/pip/_vendor/rich/progress.py` | 0.993 | 0.992 |
| axios:docs/public/logo.svg | `docs/public/logo-light.svg` | 0.993 | 0.984 |
| env_logger:deny.toml | `deny.toml` | 0.993 | 1.000 |
| env_logger:LICENSE-APACHE | `LICENSE` | 0.991 | 0.992 |
| freezegun:LICENSE | `LICENSE-APACHE` | 0.991 | 0.992 |
| rich:rich/live.py | `src/pip/_vendor/rich/live.py` | 0.991 | 1.000 |
| urllib3:src/urllib3/util/timeout.py | `src/pip/_vendor/urllib3/util/timeout.py` | 0.991 | 0.992 |
| rich:rich/text.py | `src/pip/_vendor/rich/text.py` | 0.990 | 1.000 |
| bitflags:LICENSE-APACHE | `LICENSE` | 0.989 | 0.969 |
| crossbeam:LICENSE-APACHE | `LICENSE` | 0.989 | 0.992 |
| pip:src/pip/_vendor/idna/uts46data.py | `idna/uts46data.py` | 0.989 | 0.992 |
| rich:rich/pretty.py | `src/pip/_vendor/rich/pretty.py` | 0.989 | 0.984 |
| rich:rich/_unicode_data/unicode6-2-0.py | `rich/_unicode_data/unicode6-1-0.py` | 0.987 | 0.992 |
| rich:rich/_win32_console.py | `src/pip/_vendor/rich/_win32_console.py` | 0.987 | 0.992 |
| rich:rich/control.py | `src/pip/_vendor/rich/control.py` | 0.986 | 0.992 |
| urllib3:src/urllib3/util/ssl_.py | `src/pip/_vendor/urllib3/util/ssl_.py` | 0.986 | 0.992 |

(272 near-dup pairs total)

## License distribution (repo-level detection)

| license | repos |
|---|---|
| MIT | 44 |
| Apache-2.0 | 27 |
| BSD-3-Clause | 14 |
| unidentified | 14 |
| ISC | 2 |
| 0BSD | 1 |
| BSD-2-Clause | 1 |

## Admission-gate rejections

| reason | count (clone phase) |
|---|---|
| too_large | 21 |
| binary_nul | 470 |
| high_entropy | 14 |
| **total** | 505 |

Ingest-phase rejections (work trees are pre-gated, expect 0): 0.

## Deliberate dedup control

Fixtures `fixture-alpha` and `fixture-beta` both vendor the identical file `vendored/tiny-lru.js` (plus distinct READMEs); `fixture-gamma` vendors a near-identical variant (one small `has()` method added) as a MinHash near-dup control.

- vendored-file blake3: `b3:becd9067b992003dfd21b3f1a621af4bc1aada014002e0e73fe043bbf036c58f`
- distinct blob hashes for that file: 1 (expected 1)
- `repo_blobs` manifest entries for that hash: 2 (expected 2: fixture-alpha + fixture-beta)
- near-dup control: flagged Near by MinHash/LSH (jaccard=0.940 vs `vendored/tiny-lru.js`) (expected: flagged Near vs the alpha/beta blob)

Verdict: PASS — one canonical blob, two repo manifest entries; gamma variant flagged near-dup as designed.

## Operator notes (acquisition history)

- Cold acquisition (first full pass: 92 fresh clones + materialize over the network at 8-way) took ~780s; a second pass fetched the 8 stragglers in ~82s. The 43.9s above is the final uniform re-materialize+ingest pass against warm local git object caches.
- 6 repos in the original curated list had moved owners by 2026-09 and returned genuine 404s (verified via authenticated GitHub API): either -> rayon-rs/either, env_logger -> rust-cli/env_logger, lazy_static -> rust-lang-nursery/lazy-static.rs, minimist -> minimistjs/minimist, deep-equal -> inspect-js/node-deep-equal, and tokio-util was folded into the tokio monorepo (replaced with BurntSushi/globset to stay monorepo-free).
- 2 transient network failures on the first pass (HTTP/2 stream CANCEL on ljharb/qs; HTTP 504 on BurntSushi/walkdir) — both succeeded on retry. No GitHub rate limiting observed for git-over-https at 8-way parallelism.
