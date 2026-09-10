export const TARGETS = Object.freeze({
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
});

export function targetForPlatform(platform, arch) {
  const target = TARGETS[`${platform}-${arch}`];
  if (!target) {
    throw new Error(
      `Unsupported platform ${platform}-${arch}. Supported: ${Object.keys(TARGETS).join(", ")}.`
    );
  }
  return target;
}

export const RELEASE_BASE = "https://github.com/sblattj/oss-search/releases/download";

export function assetFilename(version, target) {
  return `oss-search-${version}-${target}.tar.gz`;
}

export function assetUrl(version, target) {
  return `${RELEASE_BASE}/v${version}/${assetFilename(version, target)}`;
}

export function checksumsUrl(version) {
  return `${RELEASE_BASE}/v${version}/checksums.txt`;
}
