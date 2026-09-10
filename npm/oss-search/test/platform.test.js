import test from "node:test";
import assert from "node:assert/strict";
import {
  TARGETS,
  assetFilename,
  assetUrl,
  checksumsUrl,
  targetForPlatform,
} from "../lib/platform.js";
import { parseChecksums, sha256 } from "../lib/install.js";

test("targetForPlatform maps all four supported platform keys", () => {
  assert.equal(targetForPlatform("darwin", "x64"), "x86_64-apple-darwin");
  assert.equal(targetForPlatform("darwin", "arm64"), "aarch64-apple-darwin");
  assert.equal(targetForPlatform("linux", "x64"), "x86_64-unknown-linux-gnu");
  assert.equal(targetForPlatform("linux", "arm64"), "aarch64-unknown-linux-gnu");
});

test("targetForPlatform output covers every declared target", () => {
  const produced = new Set(Object.keys(TARGETS).map((k) => TARGETS[k]));
  assert.equal(produced.size, 4);
});

test("targetForPlatform throws on unknown platform or arch", () => {
  assert.throws(() => targetForPlatform("win32", "x64"), /Unsupported platform win32-x64/);
  assert.throws(() => targetForPlatform("freebsd", "arm64"), /Unsupported platform freebsd-arm64/);
  assert.throws(() => targetForPlatform("linux", "riscv64"), /Unsupported platform linux-riscv64/);
});

test("assetUrl and assetFilename build release URLs", () => {
  assert.equal(
    assetUrl("0.2.0", "aarch64-apple-darwin"),
    "https://github.com/sblattj/oss-search/releases/download/v0.2.0/oss-search-0.2.0-aarch64-apple-darwin.tar.gz"
  );
  assert.equal(assetFilename("0.2.0", "aarch64-apple-darwin"), "oss-search-0.2.0-aarch64-apple-darwin.tar.gz");
  assert.equal(
    checksumsUrl("0.2.0"),
    "https://github.com/sblattj/oss-search/releases/download/v0.2.0/checksums.txt"
  );
});

test("parseChecksums reads sha256sum-style lines and skips junk", () => {
  const hex = "a".repeat(64);
  const other = "0123456789abcdef".repeat(4);
  const map = parseChecksums(
    [
      `${hex}  oss-search-0.2.0-aarch64-apple-darwin.tar.gz`,
      `${other} *oss-search-0.2.0-x86_64-unknown-linux-gnu.tar.gz`,
      "not-a-checksum-line",
      "",
    ].join("\n")
  );
  assert.equal(map.get("oss-search-0.2.0-aarch64-apple-darwin.tar.gz"), hex);
  assert.equal(map.get("oss-search-0.2.0-x86_64-unknown-linux-gnu.tar.gz"), other);
  assert.equal(map.size, 2);
});

test("sha256 hashes a buffer", () => {
  assert.equal(
    sha256(Buffer.from("hello")),
    "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
  );
});
