# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The newest heading's version must match the `[workspace.package]` version and
`oss-mcp`'s `VERSION` constant (enforced by `oss-mcp/tests/version.rs`).

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
