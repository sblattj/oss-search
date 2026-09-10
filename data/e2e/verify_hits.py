#!/usr/bin/env python3
"""E2E gate: independent no-fake-content check for live code-search hits.
Fetches each hit's file from its pinned SHA via the GitHub contents API and
records (a) whether the returned snippet line appears verbatim in the real
file, (b) whether the probe term occurs in the file. Writes
data/e2e/live/hit-verification.json. Requires GITHUB_TOKEN."""
import json, os, re, sys, urllib.request
from pathlib import Path

E2E = Path(__file__).resolve().parent
token = os.environ.get("GITHUB_TOKEN") or sys.exit("GITHUB_TOKEN not set")
s = json.loads((E2E / "live" / "search-code-retry-snippets.json").read_text())
out = []
for r in s["results"]:
    m = re.match(r"https://github\.com/([^/]+)/([^/]+)/blob/([0-9a-f]{40})/(.+)$", r["url"])
    if not m:
        sys.exit(f"unpinned url: {r['url']}")
    owner, name, sha, path = m.groups()
    api = f"https://api.github.com/repos/{owner}/{name}/contents/{path}?ref={sha}"
    req = urllib.request.Request(api, headers={
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github.raw+json",
        "User-Agent": "oss-search-e2e-verification",
    })
    body = urllib.request.urlopen(req, timeout=30).read().decode("utf-8", "replace")
    out.append({
        "repo": f"{owner}/{name}", "sha": sha, "path": path,
        "snippet_line_verbatim_in_file": (r.get("snippet") or "").strip() in body,
        "retry_ci_in_file": "retry" in body.lower(),
    })
ev = {"method": "fetched each hit file from its pinned SHA via GitHub contents API (raw) and checked (a) the returned snippet line appears verbatim, (b) the probe term occurs in the file", "hits": out}
json.dump(ev, open(E2E / "live" / "hit-verification.json", "w"), indent=2)
bad = [h for h in out if not (h["snippet_line_verbatim_in_file"] and h["retry_ci_in_file"])]
print(f"HIT VERIFICATION {'FAIL' if bad else 'PASS'}: {len(out)} hits checked, {len(bad)} bad")
sys.exit(1 if bad else 0)
