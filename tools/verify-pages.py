#!/usr/bin/env python3
"""Verify imported pages against the frozen current deployment and own test server."""

from pathlib import Path
import selectors
import subprocess
import time

from baseline import compare

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target" / "debug" / "jxpan-rust"


def main():
    process = subprocess.Popen(
        [str(BINARY)],
        cwd=ROOT,
        env={"JXPAN_LISTEN_ADDR": "127.0.0.1:0"},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            if not selector.select(timeout=10):
                raise TimeoutError("Rust server did not announce its port")
        line = process.stdout.readline().strip()
        prefix = "jxpan-rust listening on "
        if not line.startswith(prefix):
            raise RuntimeError("Rust server failed to start")
        started = time.monotonic()
        compare("http://" + line.removeprefix(prefix))
        print(f"PASS live Rust HTTP page verification ({time.monotonic() - started:.3f}s)")
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == "__main__":
    main()
