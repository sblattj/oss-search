# Hot-set query latency (real corpus)

Measured by `oss-index/examples/bench_hotset.rs` against the trigram index built by `index_hotset.rs` over the real corpus (`data/corpus/work`, 13408 files). Every pattern below was verified to exist in the corpus and asserted to return at least one hit. Build profile: **release** (`cargo run --release -p oss-index --example bench_hotset`). Index load time: 1.12s; docs: 13408.

| stat | value |
|---|---|
| queries | 51 |
| p50 | 33.68 ms |
| p90 | 72.31 ms |
| p99 | 104.66 ms |
| max | 104.66 ms |
| total | 1988 ms |
| requirement | p50 < 100 ms |
| verdict | PASS |

| # | kind | pattern | origin | ms | hits | candidates scanned |
|---|---|---|---|---|---|---|
| 1 | literal (ci) | `Serialize` | serde | 29.95 | 50 | 880 |
| 2 | literal (ci) | `deserialize_struct` | serde de | 67.99 | 9 | 4022 |
| 3 | literal (ci) | `serialize_str` | serde ser | 90.57 | 46 | 6158 |
| 4 | literal (ci) | `Visitor` | serde de | 9.91 | 50 | 274 |
| 5 | literal (ci) | `route` | flask | 65.06 | 50 | 3027 |
| 6 | literal (ci) | `render_template` | flask | 72.31 | 47 | 4231 |
| 7 | literal (ci) | `before_request` | flask | 48.78 | 24 | 1938 |
| 8 | literal (ci) | `debounce` | lodash | 44.70 | 35 | 2099 |
| 9 | literal (ci) | `throttle` | lodash | 55.16 | 31 | 2442 |
| 10 | literal (ci) | `memoize` | lodash | 39.65 | 50 | 1303 |
| 11 | literal (ci) | `chunkSize` | lodash | 16.46 | 9 | 423 |
| 12 | literal (ci) | `backoff_factor` | urllib3 retry | 56.44 | 6 | 2599 |
| 13 | literal (ci) | `get_backoff_time` | urllib3 retry | 70.77 | 3 | 4626 |
| 14 | literal (ci) | `parse_obj_as` | pydantic | 85.71 | 11 | 5909 |
| 15 | literal (ci) | `BaseModel` | pydantic | 35.57 | 50 | 1218 |
| 16 | literal (ci) | `par_iter` | rayon | 82.83 | 50 | 5909 |
| 17 | literal (ci) | `join_context` | rayon | 39.22 | 6 | 1577 |
| 18 | literal (ci) | `BytesMut` | bytes | 16.35 | 17 | 523 |
| 19 | literal (ci) | `NaiveDateTime` | chrono | 2.94 | 37 | 78 |
| 20 | literal (ci) | `ArgMatches` | clap | 22.08 | 35 | 1086 |
| 21 | literal (ci) | `GlobMatcher` | globset | 9.66 | 3 | 221 |
| 22 | literal (ci) | `RawTable` | hashbrown | 11.30 | 10 | 266 |
| 23 | literal (ci) | `maybe_uninit` | smallvec | 50.64 | 3 | 1991 |
| 24 | literal (ci) | `urlAlphabet` | nanoid | 41.23 | 18 | 2117 |
| 25 | literal (ci) | `deepEqual` | node-deep-equal | 32.93 | 50 | 1033 |
| 26 | literal (ci) | `isBefore` | dayjs | 4.03 | 27 | 94 |
| 27 | literal (ci) | `WithError` | logrus | 31.03 | 17 | 1069 |
| 28 | literal (ci) | `NewRandom` | go.uuid | 21.96 | 3 | 830 |
| 29 | literal (ci) | `Unmarshal` | go-toml | 3.95 | 50 | 131 |
| 30 | literal (ci) | `Middleware` | gin | 5.25 | 50 | 110 |
| 31 | literal (ci) | `Mount` | chi | 4.05 | 50 | 84 |
| 32 | literal (ci) | `stringify` | qs/json5 | 104.66 | 50 | 7867 |
| 33 | literal (ci) | `module.exports` | js packages | 66.98 | 50 | 4047 |
| 34 | literal (ci) | `subscriber` | tracing | 64.71 | 50 | 2927 |
| 35 | literal (cs) | `Serialize` | serde trait | 16.63 | 50 | 880 |
| 36 | literal (cs) | `deserialize_struct` | serde macro | 54.73 | 9 | 3772 |
| 37 | literal (cs) | `Retry` | urllib3 class | 32.61 | 50 | 1207 |
| 38 | literal (cs) | `DateTime` | chrono | 33.68 | 50 | 1534 |
| 39 | literal (cs) | `debounce` | lodash | 29.68 | 35 | 1305 |
| 40 | literal (cs) | `deepEqual` | node-deep-equal | 20.57 | 50 | 788 |
| 41 | literal (cs) | `Span` | tracing | 13.72 | 50 | 478 |
| 42 | regex | `function debounce` | lodash | 55.12 | 1 | 3757 |
| 43 | regex | `fn visit_` | serde de | 29.18 | 50 | 2429 |
| 44 | regex | `class Retry` | urllib3 | 52.48 | 4 | 3561 |
| 45 | regex | `self\.sleep` | urllib3 retry | 56.66 | 8 | 3521 |
| 46 | regex | `serialize_(u8|u16)` | serde ser | 5.56 | 16 | 136 |
| 47 | regex | `impl.*Serializer` | serde ser | 17.09 | 50 | 735 |
| 48 | regex | `def __init__` | python corpus | 73.96 | 50 | 6141 |
| 49 | regex | `fn main\(\)` | rust corpus | 30.83 | 50 | 2429 |
| 50 | regex | `module\.exports` | javascript corpus | 55.76 | 50 | 3886 |
| 51 | regex | `\.MustParse|ParseU` | go.uuid/google-uuid | 3.49 | 4 | 63 |
