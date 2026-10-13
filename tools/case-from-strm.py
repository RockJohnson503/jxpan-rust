#!/usr/bin/env python3
"""Read one explicitly supplied STRM URL into ignored private verification inputs."""
import argparse
import json
import urllib.parse
from pathlib import Path

from baseline import PRIVATE, private_json

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("file", type=Path)
args = parser.parse_args()
url = args.file.read_text(encoding="utf-8-sig").strip()
fields = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
share = fields.get("url", [""])[0]
host = urllib.parse.urlsplit(share).hostname or ""
if host != "cloud.189.cn" and not host.endswith(".cloud.189.cn"):
    raise SystemExit("Explicit sample is not a Cloud189 share; values not emitted.")
case = {
    "provider":"cloud189", "url":share, "pwd":fields.get("pwd", [""])[0],
    "file_id":fields.get("id", fields.get("d", [""]))[0],
    "sid":fields.get("sid", [""])[0],
}
path = PRIVATE / "live-cases.json"
cases = json.loads(path.read_text())
cases = [existing for existing in cases if existing["provider"] != "cloud189"] + [case]
private_json(path, cases)
print(json.dumps({"case_providers":[item["provider"] for item in cases], "private_files":True}))
