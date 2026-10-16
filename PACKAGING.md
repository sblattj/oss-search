# Packaging & distribution

## Decision

`oss-search` ships through four channels, all from the release workflow on a
`v*` tag: prebuilt binaries on the GitHub Release, the `oss-search` npm shim,
the workspace crates on crates.io, and `cargo install --git` for any commit.

```sh
cargo install oss-mcp oss-cli
cargo install --git https://github.com/sblattj/oss-search oss-mcp   # any commit
```

## crates.io

A registry tarball cannot contain path dependencies, so every non-dev
intra-workspace dependency carries a version requirement equal to the
workspace version, e.g. `oss-core = { path = "../oss-core", version = "0.3.1" }`.
`oss-mcp/tests/version.rs` fails when one of them drifts, and CI runs
`cargo publish --workspace --dry-run --locked`, which packages and verifies
all 11 crates against a temporary local registry.

The `crates` job in `.github/workflows/release.yml` publishes them in
dependency order (`oss-core`, `oss-corpus`, `oss-facade`, `oss-index`,
`oss-query`, `oss-rank`, `oss-semantic`, `oss-eval`, `oss-live`, `oss-cli`,
`oss-mcp`), skipping any version already on crates.io so a re-run is safe.
Auth is crates.io trusted publishing (GitHub OIDC via
`rust-lang/crates-io-auth-action`): each crate lists repository
`sblattj/oss-search`, workflow `release.yml`, no environment. There is no
registry token secret.

0.2.0 and 0.3.0 were published by hand before the job existed.

## Release checklist

1. Bump `[workspace.package] version` in the root `Cargo.toml`, and every
   intra-workspace `version = "x.y.z"` requirement in the member manifests.
2. Update `oss-mcp/src/version.rs` (`VERSION` const) to match.
3. Add a `## [x.y.z]` heading to `CHANGELOG.md`.
4. `cargo test --workspace` — the version-gate tests in
   `oss-mcp/tests/version.rs` fail if any of the three disagree, or if a
   workspace member stops inheriting the workspace version.
5. Tag `vx.y.z` and push it — `.github/workflows/release.yml` then builds
   `oss-mcp` + `oss-cli` for all four targets (`--release --locked`), packages
   them as `oss-search-<version>-<target>.tar.gz` (+ `.sha256`), uploads them
   as workflow artifacts, and attaches them to the GitHub Release for the tag.
   The `npm` and `crates` jobs then publish the shim and the crates.
   Or run the workflow manually ("Run workflow") for a dry run: pass `version`,
   or leave it empty to use the `[workspace.package]` version from the root
   `Cargo.toml`. Artifacts upload; no Release is touched.

## Release targets & build notes

| Asset | Target | Runner |
|---|---|---|
| `oss-search-<version>-x86_64-apple-darwin.tar.gz` | Intel macOS | `macos-latest` (cross-compile via Apple SDK) |
| `oss-search-<version>-aarch64-apple-darwin.tar.gz` | Apple silicon macOS | `macos-latest` |
| `oss-search-<version>-x86_64-unknown-linux-gnu.tar.gz` | x86_64 Linux (glibc) | `ubuntu-latest` |
| `oss-search-<version>-aarch64-unknown-linux-gnu.tar.gz` | aarch64 Linux (glibc) | `ubuntu-24.04-arm` (native, no cross toolchain) |

- Command: `cargo build --release --locked --target <target> -p oss-mcp -p oss-cli`.
  `--locked` requires the committed `Cargo.lock`; do not regenerate it on CI.
- **fastembed is OFF** in release binaries. It is an optional feature of
  `oss-semantic` (`fastembed = ["dep:fastembed"]`, default-off), and neither
  `oss-mcp` nor `oss-cli` depends on `oss-semantic` at all, so embeddings never
  enter the release dependency graph. Enabling it later would pull ONNX
  Runtime/model downloads and change this workflow.
- No C toolchain is needed on runners: tree-sitter and bundled SQLite also live
  only in `oss-semantic`, and `reqwest` uses `rustls`, so there is no OpenSSL to
  link. (If `oss-semantic` is ever added to the release set, each runner needs a
  C compiler — present by default on GitHub runners.)
- The asset version is the tag with the leading `v` stripped (`v0.2.0` →
  `0.2.0`); tag to match the workspace version — the `oss-mcp/tests/version.rs`
  gate already enforces manifest/`VERSION`/changelog agreement.
- Third-party actions are pinned to commit SHAs (first-party `actions/*`
  included) as a supply-chain guard; bump deliberately via `git ls-remote`.

## Verification

- `cargo build --release` — clean.
- `cargo test --workspace` — all green (existing suite plus the version gate).
- `cargo clippy --workspace --all-targets` — clean.
- `cargo install --path oss-mcp --root <tmp>` — builds and installs the
  binary standalone, the same resolution `cargo install --git` performs.
