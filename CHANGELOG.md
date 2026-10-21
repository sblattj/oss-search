# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The newest heading's version must match the `[workspace.package]` version and
`oss-mcp`'s `VERSION` constant (enforced by `oss-mcp/tests/version.rs`).

## [0.3.2] - 2026-10-21

### Fixed

- A rate-limited backend is no longer called again on every request until its
  window resets. Each backend keeps a cooldown per quota bucket (GitHub code
  search, repository search and contents are separate; so are ecosyste.ms
  repos and packages). While a bucket cools down, the call is skipped and the
  backend reports `unavailable` with "rate limited; resets in 12m 30s; not
  called", so the rest of the fan-out still answers (#6).
- When the server's requested wait is 2 s or less, the call waits and retries
  once instead of failing; Sourcegraph's retry stays inside its 20 s deadline
  (#6).
- Reset times are read from `Retry-After` as seconds or an HTTP-date, else
  from `x-ratelimit-reset` when `x-ratelimit-remaining` is 0, capped at 1 h.
  A GitHub 403 counts as a rate limit only with one of those signals, so a
  plain 403 (e.g. a file too large) does not cool anything down (#6).

### Changed

- The release workflow publishes `registry/server.json` to the MCP registry
  over GitHub OIDC after the npm shim is readable, skipping a version that is
  already listed.

## [0.3.1] - 2026-10-20

### Fixed

- Live backends (grep.app, Sourcegraph, npms.io, ecosyste.ms) work behind a
  TLS-intercepting proxy: reqwest now trusts the OS root store in addition to
  the bundled webpki roots, so a proxy CA installed in the system store is
  accepted (#4).
- Transport errors keep their full `source()` chain (e.g. the underlying
  connect or certificate error) instead of a bare
  `error sending request for url (...)` (#4).

### Changed

- The `oss-search` npm package is published by the release workflow over npm
  trusted publishing (GitHub OIDC), with a provenance attestation, after the
  release binaries are attached.
- The workspace crates are published to crates.io by the release workflow
  over crates.io trusted publishing, so `cargo install oss-cli` and
  `cargo install oss-mcp` track each release. Intra-workspace dependencies
  now carry a version requirement, checked against the workspace version by
  `oss-mcp/tests/version.rs`; CI runs `cargo publish --workspace --dry-run`.

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
