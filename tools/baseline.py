#!/usr/bin/env python3
"""Record/compare an explicitly configured JxPan baseline; never send write actions."""

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
PRIVATE = ROOT / ".scratch" / "baseline"
PAGES = {"index": "/", "admin-login": "/admin"}
MAX_BYTES = 16 * 1024 * 1024


def private_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(path.parent, 0o700)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False)
        stream.write("\n")
    os.chmod(path, 0o600)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def get(base, path):
    request = urllib.request.Request(
        base.rstrip("/") + path,
        headers={"Accept-Encoding": "identity", "Cache-Control": "no-cache"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        data = response.read(MAX_BYTES + 1)
        if len(data) > MAX_BYTES:
            raise ValueError("baseline response exceeds limit")
        if response.headers.get("Content-Encoding") == "gzip":
            with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
                data = compressed.read(MAX_BYTES + 1)
        if len(data) > MAX_BYTES:
            raise ValueError("decoded baseline response exceeds limit")
        return response.status, response.headers.get("Content-Type", ""), data


def record(base, ssh):
    PRIVATE.mkdir(parents=True, exist_ok=True)
    manifest = {"pages": {}}
    for name, path in PAGES.items():
        status, content_type, data = get(base, path)
        if status != 200 or "text/html" not in content_type:
            raise ValueError(f"unexpected baseline page response: {name}")
        data.decode("utf-8")
        (PRIVATE / f"{name}.html").write_bytes(data)
        manifest["pages"][name] = {
            "path": path, "status": status, "content_type": content_type,
            "bytes": len(data), "sha256": digest(data),
        }
    if ssh:
        # Docker's read-only package export; no remote shell reads, writes or discovery.
        result = subprocess.run(
            ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
             "-o", "ConnectTimeout=10", ssh,
             "docker cp jxpan:/app/_worker.js -"],
            capture_output=True, check=True, timeout=45,
        )
        if len(result.stdout) > MAX_BYTES:
            raise ValueError("source package exceeds limit")
        with tarfile.open(fileobj=io.BytesIO(result.stdout), mode="r:*") as archive:
            files = [member for member in archive if member.isfile()]
            if len(files) != 1 or Path(files[0].name).name != "_worker.js":
                raise ValueError("unexpected source package entries")
            if files[0].size > MAX_BYTES:
                raise ValueError("source exceeds limit")
            source = archive.extractfile(files[0]).read()
        source.decode("utf-8")
        (PRIVATE / "_worker.js").write_bytes(source)
        manifest["source"] = {"bytes": len(source), "sha256": digest(source)}
    license_url = "https://raw.githubusercontent.com/ByLsPro/JxPan/main/LICENSE"
    with urllib.request.urlopen(license_url, timeout=30) as response:
        license_text = response.read(64 * 1024)
    if b"Apache License" not in license_text or b"Version 2.0" not in license_text:
        raise ValueError("unexpected upstream license")
    license_dir = ROOT / "LICENSES"
    license_dir.mkdir(exist_ok=True)
    (license_dir / "JxPan-Apache-2.0.txt").write_bytes(license_text)
    (PRIVATE / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(manifest, ensure_ascii=False, indent=2))


def compare(base):
    manifest = json.loads((PRIVATE / "manifest.json").read_text(encoding="utf-8"))
    for name, expected in manifest["pages"].items():
        status, content_type, data = get(base, expected["path"])
        if (status, content_type, digest(data)) != (
            expected["status"], expected["content_type"], expected["sha256"]
        ):
            raise AssertionError(f"page differs from frozen baseline: {name}")
        print(f"PASS {name}: status/content-type/SHA-256 ({len(data)} bytes)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["record", "compare"])
    parser.add_argument("--base", default=os.environ.get("JXPAN_BASELINE_URL"))
    parser.add_argument("--ssh", help="authorized SSH target for read-only source packaging")
    args = parser.parse_args()
    if not args.base:
        parser.error("--base or JXPAN_BASELINE_URL is required")
    if args.operation == "record":
        record(args.base, args.ssh)
    else:
        compare(args.base)


if __name__ == "__main__":
    main()
