#!/usr/bin/env python3
"""Check frozen original provenance separately from isolated, security-patched pages.

By default the child receives no provider credentials or inherited environment;
admin login/cookies and DB writes use only synthetic data in its temporary DB.
This safety comes from having no real accounts or provider parse/download calls,
NOT from disabling mandatory auth renewal (JXPAN_AUTH_REFRESH is ignored).
Optional --baseline authorizes GET comparisons of the two original public pages
only; it never receives our synthetic admin login/cookie or provider requests.
"""

import argparse
import ipaddress
import json
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

from baseline import MAX_BYTES, PRIVATE, ROOT, compare, digest

BINARY = ROOT / "target" / "debug" / "jxpan-rust"
SOURCE_SHA256 = "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
ORIGINAL_PAGES = {
    "index": {
        "path": "/", "status": 200, "content_type": "text/html; charset=utf-8",
        "bytes": 275007,
        "sha256": "867d387b5fb80da772d8e3627d5de76cfad4f387e92ce1cb3bea2039e3d6d2a6",
    },
    "admin-login": {
        "path": "/admin", "status": 200, "content_type": "text/html; charset=utf-8",
        "bytes": 3699,
        "sha256": "7218b85e4e40e50f23de3de47376f319f32fadfe54845412e58c1cd44735a646",
    },
}
ORIGINAL_ADMIN_SHA256 = "82898b569bddaa909dcc935359bca3fdc1e2e9cedb8d2f60049353a1c64c08f5"
QRCODE_SHA256 = "79ec86f82856005b1c887905cfccfcfbec3821ca61c7fd5a952faa5f778f791c"
ADMIN_USER = "page-verification-admin"
ADMIN_PASSWORD = "synthetic-page-verification-password"


class VerificationError(RuntimeError):
    """Only fixed, non-private diagnostic messages are emitted."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        return None


def require(condition, message):
    if not condition:
        raise VerificationError(message)


def verify_frozen():
    # Never update the private manifest or rebase hashes onto the patched web files.
    manifest = json.loads((PRIVATE / "manifest.json").read_text(encoding="utf-8"))
    require(manifest.get("pages") == ORIGINAL_PAGES, "frozen original page manifest changed")
    source = (PRIVATE / "_worker.js").read_bytes()
    require(
        manifest.get("source") == {"sha256": SOURCE_SHA256, "bytes": len(source)}
        and digest(source) == SOURCE_SHA256,
        "frozen original source hash changed",
    )
    for name, expected in ORIGINAL_PAGES.items():
        original = (PRIVATE / f"{name}.html").read_bytes()
        require(
            len(original) == expected["bytes"] and digest(original) == expected["sha256"],
            f"frozen original HTML changed: {name}",
        )
    print("PASS frozen original source/manifest and two original HTML hashes (read-only)")


def local_base(process):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout=10):
            raise VerificationError("local Rust server startup timed out")
    line = process.stdout.readline().strip()
    prefix = "jxpan-rust listening on "
    require(line.startswith(prefix), "local Rust server failed to announce its port")
    base = "http://" + line.removeprefix(prefix)
    parts = urllib.parse.urlsplit(base)
    require(
        ipaddress.ip_address(parts.hostname).is_loopback and 0 < parts.port < 65536
        and parts.username is None and parts.password is None
        and not parts.path and not parts.query and not parts.fragment,
        "test server must announce a loopback origin",
    )
    return base


def stop_owned_server(process):
    forced = False
    try:
        if process.poll() is None:
            try:
                process.terminate()  # SIGTERM for this owned child only.
            except ProcessLookupError:
                pass
        try:
            status = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            forced = True
            process.kill()  # Only after graceful shutdown times out.
            status = process.wait()
        return status, forced
    finally:
        process.stdout.close()


def request(base, path, method="GET", data=None, headers=None):
    # No inherited proxy, redirects or cookie jar can send synthetic admin data away.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect)
    fields = {"Accept-Encoding": "identity"}
    fields.update(headers or {})
    message = urllib.request.Request(base + path, data=data, headers=fields, method=method)
    try:
        response = opener.open(message, timeout=10)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        body = response.read(MAX_BYTES + 1)
        require(len(body) <= MAX_BYTES, "local response exceeded the size limit")
        return response.status, response.headers, body


def private_headers(headers, referrer_policy="no-referrer"):
    require(headers.get("Cache-Control") == "no-store", "admin response must be no-store")
    require(headers.get("Referrer-Policy") == referrer_policy, "admin referrer policy differs")
    require(headers.get("X-Frame-Options") == "DENY", "admin frame protection differs")
    require("Access-Control-Allow-Origin" not in headers, "admin response must not expose CORS")


def exact_page(base, path, expected, cookie=None):
    status, headers, body = request(base, path, headers={"Cookie": cookie} if cookie else None)
    require(status == 200, f"local page status differs: {path}")
    require(headers.get("Content-Type") == "text/html; charset=utf-8", "local HTML MIME differs")
    require(body == expected, f"local page is not exact current web bytes: {path}")
    if path.startswith("/admin"):
        private_headers(headers, "same-origin" if cookie is None else "no-referrer")
        if cookie is None:
            require("Set-Cookie" not in headers, "anonymous admin page must not set a session")
    return headers


def verify_local(base):
    home = (ROOT / "web/index.html").read_bytes()
    login = (ROOT / "web/admin-login.html").read_bytes()
    admin = (ROOT / "web/admin.html").read_bytes()
    vendor = (ROOT / "web/vendor/qrcode.js").read_bytes()
    require(digest(home) != ORIGINAL_PAGES["index"]["sha256"], "current home lost its security patches")
    require(digest(admin) != ORIGINAL_ADMIN_SHA256, "current admin lost its security patches")
    require(digest(vendor) == QRCODE_SHA256, "vendored QR encoder hash changed")
    for page in (home, admin):
        require(b'src="/assets/qrcode.js"' in page, "current page must reference same-origin QR vendor")

    exact_page(base, "/", home)
    for path in ("/admin", "/admin/"):
        exact_page(base, path, login)
    print("PASS local home/login: exact current web bytes, not frozen original page hashes")

    status, headers, body = request(base, "/assets/qrcode.js")
    require(status == 200 and body == vendor and digest(body) == QRCODE_SHA256, "local QR asset bytes/hash differ")
    require(headers.get("Content-Type") == "application/javascript; charset=utf-8", "QR asset MIME differs")
    require(headers.get("X-Content-Type-Options") == "nosniff", "QR asset must be nosniff")
    require(headers.get("Cache-Control") == "public, max-age=3600", "QR asset cache policy differs")
    print("PASS local QR vendor: exact bytes/pinned hash, JavaScript MIME and nosniff")

    # This cookie never leaves the validated loopback child; the baseline is never logged into.
    form = urllib.parse.urlencode({
        "action": "login", "username": ADMIN_USER, "password": ADMIN_PASSWORD,
    }).encode("ascii")
    status, headers, body = request(base, "/admin/", method="POST", data=form, headers={
        "Content-Type": "application/x-www-form-urlencoded", "Origin": "null",
        "Sec-Fetch-Site": "same-origin", "Sec-Fetch-Mode": "navigate", "Sec-Fetch-Dest": "document",
    })
    require(status == 200 and body == admin, "local login did not return exact current admin HTML")
    private_headers(headers)
    cookie_header = headers.get("Set-Cookie", "")
    require(
        cookie_header.startswith("admin_token=")
        and all(attribute in cookie_header for attribute in ("Path=/", "HttpOnly", "SameSite=Lax", "Max-Age=604800")),
        "local login session cookie protection differs",
    )
    cookie = cookie_header.split(";", 1)[0]
    require(len(cookie.removeprefix("admin_token=")) == 44, "local login session token missing")
    require(cookie.removeprefix("admin_token=").encode("ascii") not in body, "session token leaked in admin HTML")
    for path in ("/admin", "/admin/"):
        exact_page(base, path, admin, cookie)
    print("PASS local authenticated admin: exact current patched web bytes and private headers")

    for path, fields in (
        ("/", None), ("/admin", None), ("/admin/", {"Cookie": cookie}),
        ("/assets/qrcode.js", None),
    ):
        status, headers, body = request(base, path, method="HEAD", headers=fields)
        require(status == 200 and body == b"", "local HEAD must preserve status and omit body")
        if path.startswith("/admin"):
            private_headers(headers, "no-referrer" if fields else "same-origin")
        if path == "/assets/qrcode.js":
            require(headers.get("X-Content-Type-Options") == "nosniff", "QR HEAD must retain nosniff")
    for path in ("/", "/admin", "/admin/", "/api/create-short"):
        status, headers, body = request(base, path, method="OPTIONS")
        methods = {method.strip() for method in headers.get("Access-Control-Allow-Methods", "").split(",")}
        require(status == 204 and body == b"" and {"GET", "POST", "HEAD", "OPTIONS"} <= methods,
                "local OPTIONS status/body/methods differ")
    status, _, _ = request(base, "/api/create-short")
    require(status == 405, "GET short-link creation must be method-not-allowed")
    print("PASS local HEAD/OPTIONS and GET short-link method restriction")

    status, headers, body = request(base, "/?action=get_stats")
    require(status == 200 and headers.get("Content-Type", "").startswith("application/json"),
            "anonymous public get_stats must return JSON successfully")
    value = json.loads(body)
    require(value.get("success") is True and value.get("code") == 200, "public get_stats returned failure")
    require(value.get("data") == {"total": 0, "success": 0, "failed": 0, "cached": 0},
            "fresh isolated public counters must be zero and contain no private fields")
    print("PASS anonymous public get_stats: fresh isolated lifetime counters, no private fields")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", help="authorized original two-page comparison URL (public GET only)")
    args = parser.parse_args(argv)
    verify_frozen()
    if args.baseline:
        try:
            compare(args.baseline)
        except AssertionError:
            raise VerificationError("authorized baseline pages differ from frozen originals") from None
        print("PASS authorized baseline: two original pages only (GET, no login or write API)")

    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="jxpan-verify-pages-") as directory:
        # No inherited environment, credential file or provider parse/download requests.
        # Mandatory auth renewal is not disabled; there is no real account to renew.
        environment = {
            "JXPAN_LISTEN_ADDR": "127.0.0.1:0",
            "JXPAN_DB_PATH": str(Path(directory) / "state.sqlite"),
            "JXPAN_ADMIN_USER": ADMIN_USER,
            "JXPAN_ADMIN_PASSWORD": ADMIN_PASSWORD,
        }
        process = subprocess.Popen(
            [str(BINARY)], cwd=directory, env=environment,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        )
        try:
            verify_local(local_base(process))
        finally:
            status, forced = stop_owned_server(process)
            require(status == 0 and not forced, "owned local server did not shut down gracefully")
    print(f"PASS isolated security-patched Rust HTTP pages ({time.monotonic() - started:.3f}s)")


if __name__ == "__main__":
    try:
        main()
    except VerificationError as error:
        print(f"FAIL {error}", file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        # Do not echo URLs, response bodies, private paths or raw exception details.
        print(f"FAIL page verification ({type(error).__name__}; private values redacted)", file=sys.stderr)
        sys.exit(1)
