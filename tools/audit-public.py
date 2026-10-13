#!/usr/bin/env python3
"""Reject accidental inclusion of live credentials in Git's staged content."""
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
PRIVATE = ROOT / ".scratch/baseline/credentials.json"


def strings(value):
    if isinstance(value, dict):
        for nested in value.values():
            yield from strings(nested)
    elif isinstance(value, list):
        for nested in value:
            yield from strings(nested)
    elif isinstance(value, str) and len(value) >= 8:
        yield value.encode()


def main():
    sensitive = list(strings(json.loads(PRIVATE.read_text()))) if PRIVATE.exists() else []
    names = subprocess.check_output(["git", "diff", "--cached", "--name-only", "-z"], cwd=ROOT)
    checked = 0
    for name in names.split(b"\0"):
        if not name:
            continue
        path = name.decode()
        if path.startswith((".scratch/", "tests/private/", "data/")) or path.startswith(".env"):
            raise SystemExit("Private path staged; audit rejected it.")
        data = subprocess.check_output(["git", "show", f":{path}"], cwd=ROOT)
        if any(value in data for value in sensitive):
            raise SystemExit(f"Live credential content detected in {path}; values not emitted.")
        checked += 1
    print(f"Staged-content credential audit passed: {checked} files; no values emitted.")


if __name__ == "__main__":
    main()
