#!/usr/bin/env python3
"""Private live differential: JSON, redirects, then at most one CDN byte per endpoint.

NOT read-only: parse/download requests can trigger mandatory provider credential
renewal on both the baseline and the Rust child. A temporary local DB does not
isolate remote account/session changes. Explicit --allow-provider-side-effects is
required before private inputs are read, a child is started or requests are sent.
Every recorded case is checked unchanged; private values are never emitted.
"""
import argparse
import copy
import ipaddress
import json
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request

from baseline import PRIVATE, ROOT, get


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


class CaseFailure(AssertionError):
    def __init__(self, phase, reason, **fields):
        super().__init__(reason)
        self.phase = phase
        self.reason = reason
        self.fields = fields


class VerificationError(RuntimeError):
    """Fixed non-private startup/shutdown/sample diagnostics."""


def safe_provider(case):
    provider = case.get("provider") if isinstance(case, dict) else None
    return provider if isinstance(provider, str) and provider in {"mcloud", "cloud189"} else "unknown"


def safe_code(value):
    if not isinstance(value, dict):
        return None
    code = value.get("code")
    return code if type(code) is int else None


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
    request = urllib.request.Request(base + query, headers={"Accept-Encoding": "identity"})
    try:
        response = urllib.request.build_opener(NoRedirect).open(request, timeout=40)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        status, location = response.status, response.headers.get("Location")
        if status == 302:
            parts = urllib.parse.urlsplit(location or "")
            if parts.scheme not in {"http", "https"} or not parts.hostname:
                raise CaseFailure("download", "invalid download redirect")
        return status, location


def probe(url, method):
    request = urllib.request.Request(url, method=method,
        headers={"User-Agent": "Mozilla/5.0", "Range": "bytes=0-0"})
    try:
        response = urllib.request.urlopen(request, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        status = response.status
        count = len(response.read(1)) if method == "GET" and status in {200, 206} else 0
        return {"status": status, "bytes_read": count, "content_range": response.headers.get("Content-Range")}


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
    # Preserve all recorded URLs/passwords/file IDs; never replace or skip a failing sample.
    if not isinstance(case, dict) or safe_provider(case) == "unknown":
        raise CaseFailure("sample", "unsupported or malformed recorded case")
    query = "/?" + urllib.parse.urlencode({"url": case["url"], "pwd": case["pwd"], "type": "json"})
    old_status, _, old_data = get(baseline, query)
    new_status, _, new_data = get(local, query)
    old, new = json.loads(old_data), json.loads(new_data)
    if (old_status != 200 or new_status != 200 or not isinstance(old, dict)
            or not isinstance(new, dict) or old.get("success") is not True or new.get("success") is not True):
        raise CaseFailure("listing", "listing failed", baseline_status=old_status, rust_status=new_status,
                          baseline_code=safe_code(old), rust_code=safe_code(new))
    if normalize_signed_fields(old) != normalize_signed_fields(new):
        raise CaseFailure("listing", "listing JSON differs", baseline_code=safe_code(old), rust_code=safe_code(new))
    file_id = first_leaf(old.get("data", {}).get("tree", {})) or case.get("file_id")
    if not file_id:
        raise CaseFailure("sample", "sample contains no downloadable file")
    sid = old.get("data", {}).get("sid", "") or case.get("sid", "")
    play = "/?" + urllib.parse.urlencode({"url": case["url"], "pwd": case["pwd"], "id": file_id, "sid": sid, "type": "down"})
    old_status, old_location = down(baseline, play)
    new_status, location = down(local, play)
    if old_status != 302 or new_status != 302:
        raise CaseFailure("download", "download redirect differs", baseline_status=old_status, rust_status=new_status)
    old_head, new_head = probe(old_location, "HEAD"), probe(location, "HEAD")
    old_range, new_range = probe(old_location, "GET"), probe(location, "GET")
    if old_head["status"] != new_head["status"]:
        raise CaseFailure("cdn-head", "baseline/Rust CDN HEAD behavior differs",
                          baseline_status=old_head["status"], rust_status=new_head["status"])
    if old_range != new_range or new_range["status"] not in {200, 206} or new_range["bytes_read"] != 1:
        raise CaseFailure("cdn-range", "CDN one-byte range differs",
                          baseline_status=old_range["status"], rust_status=new_range["status"])
    return {"provider": safe_provider(case), "listing_json_equal": True,
        "signed_url_query_not_compared": normalize_signed_fields(old) != old,
        "baseline_download": old_status, "rust_download": new_status,
        "cdn_head": new_head["status"], "cdn_range": new_range["status"],
        "file_bytes_read": 1, "range_equal": True, "private_values_redacted": True}


def local_base(process):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout=10):
            raise VerificationError("local Rust server startup timed out")
    line = process.stdout.readline().strip()
    prefix = "jxpan-rust listening on "
    if not line.startswith(prefix):
        raise VerificationError("local Rust server failed to announce its port")
    base = "http://" + line.removeprefix(prefix)
    parts = urllib.parse.urlsplit(base)
    if not (ipaddress.ip_address(parts.hostname).is_loopback and 0 < parts.port < 65536
            and parts.username is None and parts.password is None
            and not parts.path and not parts.query and not parts.fragment):
        raise VerificationError("test server must announce a loopback origin")
    return base


def stop_owned_server(process):
    forced = False
    try:
        if process.poll() is None:
            try:
                process.terminate()  # SIGTERM for the child spawned here, never a real service.
            except ProcessLookupError:
                pass
        try:
            status = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            forced = True
            process.kill()  # Kill only after the graceful shutdown deadline.
            status = process.wait()
        return status, forced
    finally:
        process.stdout.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument("--baseline", required=True, help="authorized baseline URL (provider requests are not read-only)")
    parser.add_argument(
        "--allow-provider-side-effects", action="store_true",
        help="authorize reading real private inputs and live requests to both services and CDN; "
             "mandatory renewal can change remote provider tokens/sessions despite the temporary DB",
    )
    args = parser.parse_args(argv)
    if not args.allow_provider_side_effects:
        parser.error("live verification is NOT read-only; --allow-provider-side-effects is required "
                     "before reading private inputs or starting live verification")
    print("WARNING: live verification is NOT read-only. Mandatory auth renewal on baseline/Rust "
          "parse/download requests may change remote provider tokens/sessions. "
          "The temporary local DB does not isolate remote side effects.", file=sys.stderr, flush=True)
    # Only after explicit authorization: read private inputs without emitting or rewriting them.
    cases = json.loads((PRIVATE / "live-cases.json").read_text(encoding="utf-8"))
    if not isinstance(cases, list) or not cases:
        raise VerificationError("no authorized private sample recorded")
    failures = 0
    with tempfile.TemporaryDirectory(prefix="jxpan-verify-live-") as directory:
        # Do not inherit any legacy credential seeds, service DB/admin or proxy settings.
        environment = {
            "JXPAN_LISTEN_ADDR": "127.0.0.1:0",
            "JXPAN_DB_PATH": str(Path(directory) / "state.sqlite"),
            "JXPAN_CREDENTIALS_FILE": str(PRIVATE / "credentials.json"),
        }
        # Raw child logs may contain secrets; the mandatory-renewal warning above stays visible.
        process = subprocess.Popen([str(ROOT / "target/debug/jxpan-rust")], cwd=directory,
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        try:
            local = local_base(process)
            for index, case in enumerate(cases, start=1):
                try:
                    result = verify_case(args.baseline.rstrip("/"), local, case)
                    result.update(case=index, result="PASS")
                except CaseFailure as error:
                    failures += 1
                    result = {"case": index, "provider": safe_provider(case), "result": "FAIL",
                              "phase": error.phase, "reason": error.reason, **error.fields,
                              "private_values_redacted": True}
                except Exception as error:
                    failures += 1
                    # Raw exceptions may contain a sample URL, signed redirect or secret body.
                    result = {"case": index, "provider": safe_provider(case), "result": "FAIL",
                              "reason": "case verification raised a sanitized exception",
                              "error_type": type(error).__name__, "private_values_redacted": True}
                print(json.dumps(result))
        finally:
            status, forced = stop_owned_server(process)
            if status != 0 or forced:
                raise VerificationError("owned local server did not shut down gracefully")
    print(json.dumps({"cases": len(cases), "passed": len(cases) - failures, "failed": failures,
                      "isolated_database": True, "auth_refresh": True,
                      "provider_side_effects_authorized": True, "private_values_redacted": True}))
    return 1 if failures else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except VerificationError as error:
        print(f"FAIL {error}", file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        print(f"FAIL live verification ({type(error).__name__}; private values redacted)", file=sys.stderr)
        sys.exit(1)
