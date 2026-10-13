#!/usr/bin/env python3
"""Private read-only differential: JSON, redirect, then at most one CDN byte."""
import argparse
import copy
import json
import os
import selectors
import subprocess
import urllib.error
import urllib.parse
import urllib.request

from baseline import PRIVATE, ROOT, get


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


def first_leaf(value):
    if isinstance(value, dict):
        if "id" in value and "size" in value:
            return str(value["id"])
        for child in value.values():
            found = first_leaf(child)
            if found:
                return found
    return None


def down(base, query):
    request = urllib.request.Request(base + query, headers={"Accept-Encoding":"identity"})
    try:
        response = urllib.request.build_opener(NoRedirect).open(request, timeout=40)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        status, location = response.status, response.headers.get("Location")
        if status == 302:
            parts = urllib.parse.urlsplit(location or "")
            if parts.scheme not in {"http", "https"} or not parts.hostname:
                raise AssertionError("invalid download redirect")
        return status, location


def probe(url, method):
    request = urllib.request.Request(url, method=method,
        headers={"User-Agent":"Mozilla/5.0", "Range":"bytes=0-0"})
    try:
        response = urllib.request.urlopen(request, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        status = response.status
        count = len(response.read(1)) if method == "GET" and status in {200, 206} else 0
        return {"status":status, "bytes_read":count, "content_range":response.headers.get("Content-Range")}


def normalize_signed_fields(value):
    value = copy.deepcopy(value)
    data = value.get("data")
    if isinstance(data, dict) and data.get("is_folder") is False:
        if isinstance(data.get("download_url"), str):
            data["download_url"] = "<time-varying-signed-url>"
        for row in data.get("files", []):
            if isinstance(row.get("url"), str):
                row["url"] = "<time-varying-signed-url>"
    return value


def verify_case(baseline, local, case):
    query = "/?" + urllib.parse.urlencode({"url":case["url"], "pwd":case["pwd"], "type":"json"})
    old_status, _, old_data = get(baseline, query)
    new_status, _, new_data = get(local, query)
    old, new = json.loads(old_data), json.loads(new_data)
    if old_status != 200 or new_status != 200 or old.get("success") is not True:
        raise AssertionError(f"{case['provider']}: listing failed; baseline_code={old.get('code')}, rust_code={new.get('code')}")
    if normalize_signed_fields(old) != normalize_signed_fields(new):
        raise AssertionError(f"{case['provider']}: listing differs; baseline_code={old.get('code')}, rust_code={new.get('code')}")
    file_id = first_leaf(old.get("data", {}).get("tree", {})) or case.get("file_id")
    if not file_id:
        raise AssertionError("sample contains no downloadable file")
    sid = old.get("data", {}).get("sid", "") or case.get("sid", "")
    play = "/?" + urllib.parse.urlencode({"url":case["url"], "pwd":case["pwd"], "id":file_id, "sid":sid, "type":"down"})
    old_status, old_location = down(baseline, play)
    new_status, location = down(local, play)
    if old_status != 302 or new_status != 302:
        raise AssertionError(f"{case['provider']}: download differs; baseline={old_status}, rust={new_status}")
    old_head, new_head = probe(old_location, "HEAD"), probe(location, "HEAD")
    old_range, new_range = probe(old_location, "GET"), probe(location, "GET")
    if old_head["status"] != new_head["status"]:
        raise AssertionError("baseline/Rust CDN HEAD behavior differs")
    if old_range != new_range or new_range["status"] not in {200, 206} or new_range["bytes_read"] != 1:
        raise AssertionError(f"CDN range differs: baseline={old_range['status']}, rust={new_range['status']}")
    print(json.dumps({"provider":case["provider"], "listing_json_equal":True,
        "signed_url_query_not_compared":normalize_signed_fields(old) != old,
        "baseline_download":old_status, "rust_download":new_status,
        "cdn_head":new_head["status"], "cdn_range":new_range["status"],
        "file_bytes_read":1, "range_equal":True, "private_values_redacted":True}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True)
    args = parser.parse_args()
    cases = json.loads((PRIVATE / "live-cases.json").read_text())
    if not cases:
        raise RuntimeError("no authorized private sample recorded")
    environment = os.environ.copy()
    environment.update(JXPAN_LISTEN_ADDR="127.0.0.1:0", JXPAN_CREDENTIALS_FILE=str(PRIVATE / "credentials.json"))
    process = subprocess.Popen([str(ROOT / "target/debug/jxpan-rust")], cwd=ROOT,
        env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            if not selector.select(timeout=10):
                raise TimeoutError("local Rust server startup timed out")
        line = process.stdout.readline().strip()
        prefix = "jxpan-rust listening on "
        if not line.startswith(prefix):
            raise RuntimeError("local Rust server failed to start")
        local = "http://" + line.removeprefix(prefix)
        for case in cases:
            verify_case(args.baseline.rstrip("/"), local, case)
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == "__main__":
    main()
