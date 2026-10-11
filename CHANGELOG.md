# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The newest heading's version must match the `[workspace.package]` version and
`oss-mcp`'s `VERSION` constant (enforced by `oss-mcp/tests/version.rs`).

## [0.3.0] - 2026-10-11

### Added

- `oss_search_repos` takes candidates from GitHub repository search as well as
  npms.io, so non-npm projects (e.g. `johnbalvin/pyairbnb`) are found. Results
  are deduped by owner/name, `github` is reported in `backend_status`, and rate
  limits surface as an error status instead of an empty list. `language`,
  `min_stars` and `topic` map to GitHub search qualifiers.
- Sourcegraph code search via the public streaming API (`/.api/search/stream`)
  in `oss_search_code`, with regex support, an optional `SOURCEGRAPH_TOKEN`, and
  a 20 s total deadline. `backend_status` no longer reports "no Sourcegraph
  client is wired into the live engine yet".

### Changed

- `oss_search_code` queries grep.app through its supported MCP endpoint
  (`mcp.grep.app`) instead of the bot-gated legacy API.

### Fixed

- The MCP grep.app client works against the real stateless server (optional
  session id, required `Accept: application/json, text/event-stream`, the
  current `Repository:`/`Path:`/`URL:` result format); a 429 maps to
  rate-limited and an error result to an unavailable backend.
- `oss_search_repos` scores each candidate individually before ordering (rows
  and scores could previously be paired out of order).

## [0.2.0] - 2026-09-10

### Added

- Version gate: `oss-mcp` now exposes a `VERSION` constant (`oss-mcp/src/version.rs`)
  reported in MCP `serverInfo`, asserted equal to the workspace version and the
  newest `CHANGELOG.md` heading by `oss-mcp/tests/version.rs`.
- `PACKAGING.md` documenting the distribution decision (GitHub Releases +
  `cargo install --git`, no crates.io publishing of the internal crates yet).
- `oss-mcp` library target so integration tests can reach the version constant.

### Changed

- Workspace-wide version moved to `[workspace.package]` (0.1.0 → 0.2.0); all 11
  crates now inherit `version` (plus `license` and `repository`) from the root
  manifest instead of declaring their own.
- `oss-mcp` `serverInfo` reports the `VERSION` constant instead of
  `env!("CARGO_PKG_VERSION")`.

## [0.1.0] - 2026-09-09

- Initial workspace: local trigram index, hybrid semantic layer, repo discovery
  ranking, corpus pipeline, federated live backends, 7 MCP tools over stdio and
  a byte-identical CLI (355 tests).
