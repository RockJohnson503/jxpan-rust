#!/usr/bin/env python3
"""Collect two narrowly scoped, private read-only inputs for live JxPan verification."""

import argparse
import json
import os
import urllib.parse

from baseline import PRIVATE, get, private_json


def records(base, search):
    query = urllib.parse.urlencode({"action":"get_records", "limit":3, "page":1, "search":search})
    status, _, payload = get(base, "/?" + query)
    if status != 200:
        raise RuntimeError("baseline records unavailable")
    data = json.loads(payload).get("data", {})
    return data.get("records", data.get("success", []) + data.get("failed", [])) if isinstance(data, dict) else []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    args = parser.parse_args()
    PRIVATE.mkdir(parents=True, exist_ok=True)
    cases = []
    for provider, search in [("mcloud", "139.com"), ("cloud189", "cloud.189.cn")]:
        for record in records(args.base, search):
            url = record.get("url") or ""
            pwd = record.get("pwd") or ""
            for _ in range(3):
                wrapped = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
                if "url" not in wrapped:
                    break
                pwd = wrapped.get("pwd", [pwd])[0]
                url = wrapped["url"][0]
            host = urllib.parse.urlsplit(url).hostname or ""
            allowed = (host == "cloud.189.cn" or host.endswith(".cloud.189.cn")) if provider == "cloud189" else host in {"yun.139.com", "caiyun.139.com"}
            if allowed:
                cases.append({"provider":provider, "url":url, "pwd":pwd})
                break
    private_json(PRIVATE / "live-cases.json", cases)
    print(json.dumps({"case_providers":[case["provider"] for case in cases], "private_files":True}))


if __name__ == "__main__":
    main()
