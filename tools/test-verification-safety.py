#!/usr/bin/env python3
"""Offline safety regressions: python3 -B tools/test-verification-safety.py.

All filesystem reads, network access, processes and temporary directories are
blocked during tests. Only in-memory synthetic inputs and mocked children run.
"""

import builtins
from contextlib import ExitStack, redirect_stderr, redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest
from unittest import mock


def load_script(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


live = load_script("verify-live")
pages = load_script("verify-pages")
BASELINE = "https://baseline.invalid/synthetic-baseline-secret"
LOCAL = "http://127.0.0.1:12345"
TEMP = "/synthetic-offline-temporary-directory"
CASES = [
    {"provider": "mcloud", "url": "https://share.invalid/synthetic-share-secret", "pwd": "synthetic-password"},
    {"provider": "cloud189", "url": "https://share.invalid/another-synthetic-secret", "pwd": "other-password"},
    {"provider": "mcloud", "url": "https://share.invalid/last-synthetic-secret", "pwd": "last-password"},
]


class VerificationSafetyTests(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.guards = []
        for owner, name in (
            (builtins, "open"), (io, "open"), (Path, "read_text"), (Path, "read_bytes"),
            (socket, "socket"), (socket, "create_connection"), (socket, "getaddrinfo"),
            (subprocess, "Popen"), (subprocess, "run"), (tempfile, "TemporaryDirectory"),
        ):
            self.guards.append(self.stack.enter_context(mock.patch.object(
                owner, name, side_effect=AssertionError("external access forbidden in offline tests"),
            )))
        self.stack.enter_context(mock.patch.dict(os.environ, {
            "JXPAN_CREDENTIALS_FILE": "/synthetic-credential-file-never-read",
            "MCLOUD_AUTHORIZATION": "synthetic-inherited-secret",
            "JXPAN_AUTH_REFRESH": "false",
            "JXPAN_DB_PATH": "/synthetic-service-database-never-used",
            "http_proxy": "https://proxy.invalid",
        }, clear=True))
        for module in (live, pages):
            self.stack.enter_context(mock.patch.object(module, "PRIVATE", Path("/synthetic-private-never-read")))
        self.stdout, self.stderr = io.StringIO(), io.StringIO()
        self.stack.enter_context(redirect_stdout(self.stdout))
        self.stack.enter_context(redirect_stderr(self.stderr))

    def tearDown(self):
        for guard in self.guards:
            guard.assert_not_called()

    def fake_child(self, module):
        directory = self.stack.enter_context(mock.patch.object(module.tempfile, "TemporaryDirectory"))
        directory.return_value.__enter__.return_value = TEMP
        spawn = self.stack.enter_context(mock.patch.object(module.subprocess, "Popen"))
        location = self.stack.enter_context(mock.patch.object(module, "local_base", return_value=LOCAL))
        stop = self.stack.enter_context(mock.patch.object(module, "stop_owned_server", return_value=(0, False)))
        return spawn, location, stop

    def test_live_requires_opt_in_even_with_legacy_refresh_false(self):
        with self.assertRaises(SystemExit) as error:
            live.main(["--baseline", BASELINE])
        self.assertEqual(error.exception.code, 2)
        self.assertIn("--allow-provider-side-effects is required", self.stderr.getvalue())
        self.assertNotIn("synthetic-baseline-secret", self.stderr.getvalue())
        self.assertNotIn("synthetic-inherited-secret", self.stderr.getvalue())
        self.assertEqual(self.stdout.getvalue(), "")

    def test_live_rejects_abbreviated_opt_in(self):
        with self.assertRaises(SystemExit) as error:
            live.main(["--baseline", BASELINE, "--allow-provider-side-effect"])
        self.assertEqual(error.exception.code, 2)
        self.assertNotIn("synthetic-baseline-secret", self.stderr.getvalue())

    def test_live_opt_in_still_requires_baseline(self):
        with self.assertRaises(SystemExit) as error:
            live.main(["--allow-provider-side-effects"])
        self.assertEqual(error.exception.code, 2)

    def test_both_help_paths_are_side_effect_free(self):
        for module in (live, pages):
            with self.subTest(script=module.__name__), self.assertRaises(SystemExit) as error:
                module.main(["--help"])
            self.assertEqual(error.exception.code, 0)
        output = self.stdout.getvalue()
        self.assertIn("--allow-provider-side-effects", output)
        self.assertIn("NOT read-only", output)
        self.assertIn("JXPAN_AUTH_REFRESH is ignored", output)
        self.assertIn("no provider credentials", output)

    def test_authorized_live_warns_before_inputs_and_preserves_all_cases(self):
        spawn, location, stop = self.fake_child(live)

        def synthetic_inputs(*args, **kwargs):
            self.assertIn("WARNING:", self.stderr.getvalue())
            self.assertIn("does not isolate remote side effects", self.stderr.getvalue())
            spawn.assert_not_called()
            return json.dumps(CASES)

        read = self.stack.enter_context(mock.patch.object(Path, "read_text", side_effect=synthetic_inputs))
        verify = self.stack.enter_context(mock.patch.object(live, "verify_case", side_effect=[
            live.CaseFailure("listing", "listing failed"),
            RuntimeError("synthetic-share-secret must never be printed"),
            {"provider": "mcloud", "listing_json_equal": True},
        ]))
        status = live.main(["--baseline", BASELINE, "--allow-provider-side-effects"])
        self.assertEqual(status, 1)
        read.assert_called_once_with(encoding="utf-8")
        verify.assert_has_calls([mock.call(BASELINE, LOCAL, case) for case in CASES])
        self.assertEqual(verify.call_count, len(CASES))
        location.assert_called_once_with(spawn.return_value)
        stop.assert_called_once_with(spawn.return_value)
        environment = spawn.call_args.kwargs["env"]
        self.assertEqual(set(environment), {"JXPAN_LISTEN_ADDR", "JXPAN_DB_PATH", "JXPAN_CREDENTIALS_FILE"})
        self.assertEqual(environment["JXPAN_DB_PATH"], TEMP + "/state.sqlite")
        self.assertEqual(spawn.call_args.kwargs["cwd"], TEMP)
        output = self.stdout.getvalue()
        records = [json.loads(line) for line in output.splitlines()]
        self.assertEqual([record["result"] for record in records[:-1]], ["FAIL", "FAIL", "PASS"])
        self.assertEqual(records[-1]["cases"], 3)
        self.assertEqual(records[-1]["failed"], 2)
        self.assertIs(records[-1]["auth_refresh"], True)
        self.assertIs(records[-1]["provider_side_effects_authorized"], True)
        self.assertNotIn("synthetic-share-secret", output + self.stderr.getvalue())
        self.assertNotIn(BASELINE, output + self.stderr.getvalue())

    def test_live_differential_still_checks_both_services_and_cdn(self):
        case = dict(CASES[0], file_id="synthetic-file-id", sid="synthetic-sid")
        listing = {"success": True, "code": 200, "data": {"tree": {}}}
        get = self.stack.enter_context(mock.patch.object(live, "get", return_value=(200, "", json.dumps(listing))))
        down = self.stack.enter_context(mock.patch.object(live, "down", side_effect=[
            (302, "https://cdn.invalid/old"), (302, "https://cdn.invalid/new"),
        ]))
        probe = self.stack.enter_context(mock.patch.object(live, "probe", side_effect=lambda url, method: {
            "status": 200 if method == "HEAD" else 206,
            "bytes_read": 0 if method == "HEAD" else 1, "content_range": "bytes 0-0/2",
        }))
        result = live.verify_case(BASELINE, LOCAL, case)
        self.assertIs(result["listing_json_equal"], True)
        self.assertIs(result["range_equal"], True)
        self.assertEqual(result["file_bytes_read"], 1)
        self.assertEqual([call.args[0] for call in get.call_args_list], [BASELINE, LOCAL])
        self.assertEqual(get.call_args_list[0].args[1], get.call_args_list[1].args[1])
        self.assertEqual([call.args[0] for call in down.call_args_list], [BASELINE, LOCAL])
        self.assertEqual(down.call_args_list[0].args[1], down.call_args_list[1].args[1])
        self.assertEqual(probe.call_args_list, [
            mock.call("https://cdn.invalid/old", "HEAD"), mock.call("https://cdn.invalid/new", "HEAD"),
            mock.call("https://cdn.invalid/old", "GET"), mock.call("https://cdn.invalid/new", "GET"),
        ])

    def test_live_listing_mismatch_still_fails_before_download(self):
        get = self.stack.enter_context(mock.patch.object(live, "get", side_effect=[
            (200, "", json.dumps({"success": True, "code": 200, "data": {"name": "old"}})),
            (200, "", json.dumps({"success": True, "code": 200, "data": {"name": "new"}})),
        ]))
        down = self.stack.enter_context(mock.patch.object(live, "down"))
        with self.assertRaises(live.CaseFailure) as error:
            live.verify_case(BASELINE, LOCAL, CASES[0])
        self.assertEqual(error.exception.phase, "listing")
        self.assertEqual(get.call_count, 2)
        down.assert_not_called()

    def test_pages_default_needs_no_provider_opt_in_or_inherited_credentials(self):
        spawn, location, stop = self.fake_child(pages)
        frozen = self.stack.enter_context(mock.patch.object(pages, "verify_frozen"))
        compare = self.stack.enter_context(mock.patch.object(pages, "compare"))
        verify = self.stack.enter_context(mock.patch.object(pages, "verify_local"))
        pages.main([])
        frozen.assert_called_once_with()
        compare.assert_not_called()
        verify.assert_called_once_with(LOCAL)
        location.assert_called_once_with(spawn.return_value)
        stop.assert_called_once_with(spawn.return_value)
        self.assertEqual(spawn.call_args.kwargs["cwd"], TEMP)
        self.assertEqual(spawn.call_args.kwargs["env"], {
            "JXPAN_LISTEN_ADDR": "127.0.0.1:0", "JXPAN_DB_PATH": TEMP + "/state.sqlite",
            "JXPAN_ADMIN_USER": pages.ADMIN_USER, "JXPAN_ADMIN_PASSWORD": pages.ADMIN_PASSWORD,
        })

    def test_pages_explicit_baseline_comparison_is_preserved(self):
        self.fake_child(pages)
        self.stack.enter_context(mock.patch.object(pages, "verify_frozen"))
        compare = self.stack.enter_context(mock.patch.object(pages, "compare"))
        self.stack.enter_context(mock.patch.object(pages, "verify_local"))
        pages.main(["--baseline", BASELINE])
        compare.assert_called_once_with(BASELINE)
        self.assertIn("two original pages only (GET, no login or write API)", self.stdout.getvalue())


if __name__ == "__main__":
    unittest.main()
