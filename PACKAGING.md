# Packaging & distribution

## Decision

`oss-search` distributes its binaries via **GitHub Releases** and
**`cargo install --git`**. The internal crates are *not* published to
crates.io. There is no thin `oss-mcp` meta crate on crates.io.

```sh
cargo install --git https://github.com/sblattj/oss-search oss-mcp
cargo install --git https://github.com/sblattj/oss-search oss-cli
```

Prebuilt binaries for each release tag are attached to
<https://github.com/sblattj/oss-search/releases>.

## Why not crates.io (Option X)

Publishing `oss-mcp` to crates.io requires publishing its *entire* internal
dependency closure first, because a registry tarball cannot contain path
dependencies. Running `cargo package -p oss-mcp --allow-dirty --no-verify`
against the pre-0.2.0 manifests failed hard:

```
error: failed to verify manifest at `oss-mcp/Cargo.toml`

Caused by:
  all dependencies must have a version requirement specified when packaging.
  dependency `oss-core` does not specify a version
  Note: The packaged dependency will use the version from crates.io,
  the `path` specification will be removed from the dependency declaration.
```

Fixing that means adding version requirements to every internal path
dependency and then publishing 8 crates in dependency order
(`oss-core`, `oss-query`, `oss-facade`, `oss-rank`, `oss-index`,
`oss-semantic`, `oss-live`, then `oss-mcp` itself; `oss-eval` and `oss-cli`
are leaves). That path was rejected for now because:

1. **Name claims are irreversible and public.** Each crates.io publish
   permanently reserves a crate name and version. Renaming or restructuring
   the workspace later (this is a young project) would strand those names.
2. **Registry drift.** Once published, every internal dep resolves through
   the registry, so `Cargo.lock` in downstream installs can pick *different*
   internal-crate versions than the monorepo tests ran against. The
   `--git`/`--path` path always builds exactly the commit the user asked for.
3. **Cost with no current user demand.** Nobody is depending on these crates
   from crates.io yet; the audience installs binaries. Publishing is pure
   overhead until someone asks to `oss-core = "0.2"` as a library.

A thin meta crate on crates.io was also rejected: a meta crate cannot wrap or
re-export a binary, so it could only squat the name — misleading for anyone
who `cargo add`s it. If name reservation ever matters, publish the real
`oss-mcp` (Option X) rather than a stub.

## Why `cargo install --git` works today (Option Y)

`cargo install --git` clones the repository and resolves the **workspace**
manifest at that commit, so path dependencies between members resolve within
the checkout exactly as they do in `cargo build --release` here. No registry
metadata is required. This was verified by installing from the repository the
same way `--git` resolves it:

```sh
cargo install --path oss-mcp --root <tmp>   # same workspace + path-dep resolution as --git
```

which built and installed the `oss-mcp` binary successfully (see
`Verification` below). The literal `--git https://github.com/sblattj/oss-search`
form resolves identically once the tagged commit is pushed.

## Release checklist

1. Bump `[workspace.package] version` in the root `Cargo.toml`.
2. Update `oss-mcp/src/version.rs` (`VERSION` const) to match.
3. Add a `## [x.y.z]` heading to `CHANGELOG.md`.
4. `cargo test --workspace` — the version-gate tests in
   `oss-mcp/tests/version.rs` fail if any of the three disagree, or if a
   workspace member stops inheriting the workspace version.
5. Tag `vx.y.z` and push it — `.github/workflows/release.yml` then builds
   `oss-mcp` + `oss-cli` for all four targets (`--release --locked`), packages
   them as `oss-search-<version>-<target>.tar.gz` (+ `.sha256`), uploads them
   as workflow artifacts, and attaches them to the GitHub Release for the tag.
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

## Switching to crates.io later (Option X)

If library consumers appear, the migration is mechanical and the version
inheritance added in 0.2.0 already does the heavy lifting: add version
requirements next to each internal path dependency, publish the 8 crates in
dependency order, then `cargo publish -p oss-mcp`. Nothing in the current
tree blocks that move.
