#!/usr/bin/env node
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { ensureBinary } from "../lib/install.js";
import { targetForPlatform } from "../lib/platform.js";

const SIGNAL_EXIT_CODES = { SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGTERM: 15 };

async function main() {
  const { version } = JSON.parse(
    readFileSync(new URL("../package.json", import.meta.url), "utf8")
  );
  const target = targetForPlatform(process.platform, process.arch);
  const bin = await ensureBinary({ version, target });

  const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
  child.on("error", (err) => {
    console.error(`oss-mcp: failed to run ${bin}: ${err.message}`);
    process.exit(1);
  });
  child.on("close", (code, signal) => {
    if (code !== null) process.exit(code);
    process.exit(128 + (SIGNAL_EXIT_CODES[signal] ?? 0));
  });
}

main().catch((err) => {
  console.error(`oss-mcp: ${err && err.stack ? err.stack : String(err)}`);
  process.exit(1);
});
