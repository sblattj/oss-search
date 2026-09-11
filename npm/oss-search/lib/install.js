import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { assetFilename, assetUrl, checksumsUrl } from "./platform.js";

const DOWNLOAD_TIMEOUT_MS = 300_000;
const BINARY_NAME = "oss-mcp";

export function cacheDir(env = process.env) {
  return env.OSS_SEARCH_CACHE_DIR ?? join(homedir(), ".cache", "oss-search", "bin");
}

export function binPath({ version, target }, env = process.env) {
  return join(cacheDir(env), `${version}-${target}`, BINARY_NAME);
}

async function download(url) {
  const res = await fetch(url, {
    redirect: "follow",
    signal: AbortSignal.timeout(DOWNLOAD_TIMEOUT_MS),
  });
  if (!res.ok) {
    throw new Error(`GET ${url} failed: HTTP ${res.status} ${res.statusText}`);
  }
  return Buffer.from(await res.arrayBuffer());
}

export function sha256(buf) {
  return createHash("sha256").update(buf).digest("hex");
}

export function parseChecksums(text) {
  const map = new Map();
  for (const line of String(text).split(/\r?\n/)) {
    const m = line.match(/^([0-9a-fA-F]{64})\s+\*?(.+?)\s*$/);
    if (m) map.set(m[2], m[1].toLowerCase());
  }
  return map;
}

function findBinary(root) {
  let best = null;
  const walk = (dir, depth) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(path, depth + 1);
      } else if (entry.name === BINARY_NAME && (!best || depth < best.depth)) {
        best = { path, depth };
      }
    }
  };
  walk(root, 0);
  return best;
}

async function fetchAndVerify({ version, target }) {
  const name = assetFilename(version, target);
  const url = assetUrl(version, target);
  process.stderr.write(`oss-mcp: downloading ${url}\n`);
  const tarball = await download(url);

  let verified = false;
  try {
    const sidecar = String(await download(`${url}.sha256`));
    const m = sidecar.match(/^([0-9a-fA-F]{64})/);
    if (m) {
      const actual = sha256(tarball);
      if (actual !== m[1].toLowerCase()) {
        throw new Error(`sha256 mismatch for ${name}: expected ${m[1].toLowerCase()}, got ${actual}`);
      }
      process.stderr.write(`oss-mcp: sha256 verified for ${name}\n`);
      verified = true;
    }
  } catch (e) {
    if (e instanceof Error && e.message.startsWith("sha256 mismatch")) throw e;
  }
  if (!verified) {
    let checksums = null;
    try {
      checksums = parseChecksums(await download(checksumsUrl(version)));
    } catch {}
    if (checksums) {
      const expected = checksums.get(name);
      if (!expected) throw new Error(`checksums.txt has no entry for ${name}`);
      const actual = sha256(tarball);
      if (actual !== expected) {
        throw new Error(`sha256 mismatch for ${name}: expected ${expected}, got ${actual}`);
      }
      process.stderr.write(`oss-mcp: sha256 verified for ${name}\n`);
    } else {
      process.stderr.write(
        "oss-mcp: no .sha256 sidecar or checksums.txt in this release; skipping sha256 verification\n"
      );
    }
  }
  return { tarball, name };
}

function extractInto(tarball, name, dir) {
  const tgz = join(dir, name);
  writeFileSync(tgz, tarball);
  execFileSync("tar", ["-xzf", tgz, "-C", dir], { stdio: "pipe" });
}

export async function ensureBinary({ version, target }, env = process.env) {
  const dest = binPath({ version, target }, env);
  if (existsSync(dest) && statSync(dest).isFile()) return dest;

  const { tarball, name } = await fetchAndVerify({ version, target });

  const root = cacheDir(env);
  mkdirSync(root, { recursive: true });
  const staging = mkdtempSync(join(root, ".staging-"));
  try {
    extractInto(tarball, name, staging);
    const found = findBinary(staging);
    if (!found) throw new Error(`no ${BINARY_NAME} binary found inside ${name}`);
    const finalDir = join(root, `${version}-${target}`);
    mkdirSync(finalDir, { recursive: true });
    renameSync(found.path, dest);
    chmodSync(dest, 0o755);
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
  return dest;
}
