#!/usr/bin/env python3
"""E2E gate: verify live-backend outputs. Asserts (1) schema-valid envelopes,
(2) truthful backend_status: every consulted backend recorded, partial flag
consistent with degraded/error statuses, (3) hits attributed to real repos
with query-consistent content — no fabricated hits.
Exit 0 = verification holds."""
import json
import re
import sys
from pathlib import Path

E2E = Path(__file__).resolve().parent
REQUIRED = {"results", "total", "has_more", "partial", "backend_status", "warnings", "truncated"}
REPO_RE = re.compile(r"^[\w.\-]+/[\w.\-]+$")

def fail(msg):
    print(f"LIVE FAIL: {msg}")
    sys.exit(1)

def load(name):
    env = json.loads((E2E / "live" / name).read_text())
    missing = REQUIRED - set(env)
    if missing:
        fail(f"{name}: missing {missing}")
    return env

code = load("search-code-retry.json")
code_snip = load("search-code-retry-snippets.json")
repos = load("search-repos-express.json")

def statuses_ok(env, expected_backends, name):
    bs = {b["backend"]: b["status"] for b in env["backend_status"]}
    for b in expected_backends:
        if b not in bs:
            fail(f"{name}: consulted backend {b} missing from backend_status {bs}")
    degraded = [b for b, s in bs.items() if s in ("degraded", "error") and b in expected_backends]
    if degraded and not env["partial"]:
        fail(f"{name}: backends degraded {degraded} but partial=false — untruthful")
    if not degraded and env["partial"]:
        fail(f"{name}: all consulted backends ok but partial=true — untruthful")
    return bs

# --- search-code (repos mode, probe 'retry', limit 3) ---
bs = statuses_ok(code, {"github", "grep.app"}, "search-code-retry")
if bs.get("github") != "ok":
    fail(f"search-code: github backend not ok: {bs} (GITHUB_TOKEN problem?)")
rows = code["results"]
if not rows:
    fail("search-code: no results for probe 'retry'")
print(f"search-code(repos mode): {len(rows)} rows, backends={bs}, partial={code['partial']}")
for r in rows:
    if not REPO_RE.match(r.get("repo", "")):
        fail(f"search-code row not attributed to owner/name repo: {r.get('repo')!r}")
    if not r.get("backends"):
        fail(f"search-code row lacks backend attribution: {r}")
    ev = r.get("sample_evidence") or ""
    if "retry" not in ev.lower() and "retry" not in r.get("repo", "").lower():
        fail(f"search-code row evidence not query-consistent ('retry'): {ev[:120]}")

# --- search-code (snippets mode: independent hit verification) ---
bs2 = statuses_ok(code_snip, {"github", "grep.app"}, "search-code-retry-snippets")
srows = code_snip["results"]
if not srows:
    fail("search-code snippets: no results")
for r in srows:
    if not REPO_RE.match(r.get("repo", "")):
        fail(f"snippets row repo malformed: {r.get('repo')!r}")
    if not re.match(r"^https://github\.com/[\w.\-]+/[\w.\-]+/blob/[0-9a-f]{40}/", r.get("url", "")):
        fail(f"snippets row url not a pinned blob: {r.get('url')!r}")
# Independent no-fake-content check: each hit's file was fetched from its
# pinned SHA via the GitHub contents API; the snippet line must appear
# verbatim and the probe term must occur in the real file.
hv = json.loads((E2E / "live" / "hit-verification.json").read_text())
if len(hv["hits"]) != len(srows):
    fail(f"hit-verification covers {len(hv['hits'])} hits but snippets returned {len(srows)}")
for h in hv["hits"]:
    if not h["snippet_line_verbatim_in_file"]:
        fail(f"hit {h['repo']}/{h['path']}: snippet line NOT in the real file — fabricated content")
    if not h["retry_ci_in_file"]:
        fail(f"hit {h['repo']}/{h['path']}: probe term 'retry' absent from the real file at the pinned SHA")
print(f"search-code(snippets mode): {len(srows)} rows, all verified against pinned SHAs "
      f"(snippet line verbatim + 'retry' present in the real file)")

# --- search-repos (query 'express', limit 5) ---
bs3 = statuses_ok(repos, {"npms.io", "ecosyste.ms"}, "search-repos-express")
if not any(s == "ok" for s in bs3.values()):
    fail(f"search-repos: no consulted backend ok: {bs3}")
rrows = repos["results"]
if not rrows:
    fail("search-repos: no results for 'express'")
full_names = [r.get("repo", "") for r in rrows]
if not any(n == "expressjs/express" for n in full_names):
    fail(f"search-repos: expressjs/express not in results for query 'express': {full_names}")
for r in rrows:
    for field in ("repo", "url", "sources"):
        if field not in r:
            fail(f"search-repos row lacks {field}: {sorted(r)}")
express_row = next(r for r in rrows if r["repo"] == "expressjs/express")
if not isinstance(express_row.get("stars"), int) or express_row["stars"] < 10000:
    fail(f"expressjs/express stars implausible: {express_row.get('stars')}")
print(f"search-repos: {len(rrows)} rows, backends={bs3}, expressjs/express present "
      f"(stars={express_row['stars']}, dependents={express_row.get('dependents')})")

print("\nLIVE E2E PASS: backends truthful (status + partial consistent), every hit attributed to a real "
      "owner/name repo with query-consistent evidence; expressjs/express found for 'express'.")
