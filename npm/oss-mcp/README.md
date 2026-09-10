# oss-mcp (npm distribution)

npm/npx distribution of [oss-search](https://github.com/sblattj/oss-search) — fast local + federated OSS code search and repo discovery, exposing 7 MCP tools over stdio plus a byte-identical CLI.

This package contains no compiled code and has **zero runtime dependencies**. On first run, the `oss-mcp` shim downloads the prebuilt Rust binary for your platform from GitHub Releases, verifies it against the release's `checksums.txt` (when present), and caches it. Subsequent runs start instantly from the cache.

## Requirements

- Node.js >= 18 (only to bootstrap the binary)
- macOS (x64, arm64) or Linux (x64, arm64)
- A published release `v<version>` with assets named `oss-search-<version>-<target>.tar.gz` (containing the `oss-mcp` binary) and, optionally, `checksums.txt`

## Run once

```sh
npx oss-mcp            # speaks MCP over stdio
npx oss-mcp --live     # enable remote backends (GitHub code search, grep.app, deps.dev, ...)
```

## Install globally

```sh
npm install -g oss-mcp
oss-mcp
```

## Use with an MCP client

```json
{
  "mcpServers": {
    "oss-search": {
      "command": "npx",
      "args": ["oss-mcp"]
    }
  }
}
```

Or point directly at the cached binary (`~/.cache/oss-search/bin/<version>-<target>/oss-mcp`) to skip the Node shim entirely.

## Cache

Binaries are cached at `~/.cache/oss-search/bin/<version>-<target>/oss-mcp`. Override the location with the `OSS_SEARCH_CACHE_DIR` environment variable. Delete the cache to force a re-download.

For `GITHUB_TOKEN`-backed live search and other options, see the [repository README](https://github.com/sblattj/oss-search#readme).

## License

MIT — see [LICENSE](./LICENSE).
