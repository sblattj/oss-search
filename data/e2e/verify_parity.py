#!/usr/bin/env python3
"""E2E gate: verify schema parity between the MCP transcript envelopes and the
CLI envelope outputs (same tool, same args), plus envelope schema validity.
Exit 0 = parity holds."""
import json
import sys
from pathlib import Path

E2E = Path(__file__).resolve().parent
REQUIRED = {"results", "total", "has_more", "partial", "backend_status", "warnings", "truncated"}
ALLOWED = REQUIRED | {"note", "next_cursor"}

def fail(msg):
    print(f"PARITY FAIL: {msg}")
    sys.exit(1)

# 1. Load MCP envelopes from the transcript (tools/call responses, isError false)
mcp = {}
transcript = (E2E / "mcp-transcript.jsonl").read_text().splitlines()
for line in transcript:
    try:
        msg = json.loads(line)
    except json.JSONDecodeError:
        continue
    if msg.get("method") == "tools/call":
        continue  # request side
    if not isinstance(msg.get("id"), str) or not msg["id"].startswith("call:"):
        continue
    tool = msg["id"][len("call:"):]
    if msg.get("result", {}).get("isError"):
        fail(f"{tool}: MCP call returned isError")
    env = json.loads(msg["result"]["content"][0]["text"])
    mcp[tool] = env
if set(mcp) != {"oss_search_repos","oss_search_code","oss_repo_profile","oss_repo_tree","oss_fetch_file","oss_fetch_docs","oss_guide"}:
    fail(f"MCP transcript lacks the 7 tool envelopes: {sorted(mcp)}")

# 2. Load CLI envelopes
def env_tool_name(filename):
    return filename.split("-", 1)[1].removesuffix(".json")

cli = {}
for p in sorted((E2E / "cli").glob("*.json")):
    if p.name == "summary.json":
        continue
    env = json.loads(p.read_text())
    cli[env_tool_name(p.name)] = env

if set(cli) != set(mcp):
    fail(f"CLI outputs {sorted(cli)} != MCP tools {sorted(mcp)}")

# 3. Field-set parity + per-envelope schema validity
for tool in sorted(mcp):
    mk = set(mcp[tool].keys())
    ck = set(cli[tool].keys())
    if mk - ALLOWED or ck - ALLOWED:
        fail(f"{tool}: unexpected envelope fields mcp={mk - ALLOWED} cli={ck - ALLOWED}")
    if mk != ck:
        fail(f"{tool}: MCP field set {sorted(mk)} != CLI field set {sorted(ck)}")
    for side, env in (("mcp", mcp[tool]), ("cli", cli[tool])):
        if not isinstance(env["results"], list): fail(f"{tool}/{side}: results")
        if not isinstance(env["total"], int): fail(f"{tool}/{side}: total")
        for b in ("has_more", "partial", "truncated"):
            if not isinstance(env[b], bool): fail(f"{tool}/{side}: {b}")
        bs = env["backend_status"]
        if not bs or not isinstance(bs, list): fail(f"{tool}/{side}: backend_status")
        for b in bs:
            if not isinstance(b.get("backend"), str) or not isinstance(b.get("status"), str):
                fail(f"{tool}/{side}: backend entry {b}")
        if not isinstance(env["warnings"], list): fail(f"{tool}/{side}: warnings")

print(f"PARITY PASS: 7/7 tools — MCP transcript envelopes and CLI outputs carry identical top-level schemas "
      f"(required {sorted(REQUIRED)}, optional note/next_cursor), all schema-valid.")
