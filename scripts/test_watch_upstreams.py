#!/usr/bin/env python3
"""Unit checks for scripts/watch_upstreams.py."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "watch_upstreams.py"


def load_mod():
    spec = importlib.util.spec_from_file_location("watch_upstreams", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


class WatchUpstreamsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_mod()

    def setUp(self) -> None:
        os.environ.pop("GITHUB_OUTPUT", None)
        os.environ.pop("GITHUB_STEP_SUMMARY", None)
        os.environ.pop("GITHUB_TOKEN", None)
        os.environ.pop("GH_TOKEN", None)

    def test_ensure_http_url_rejects_non_http(self):
        self.mod.ensure_http_url("https://api.github.com/repos/a/b/releases")
        with self.assertRaises(ValueError):
            self.mod.ensure_http_url("file:///etc/passwd")
        with self.assertRaises(ValueError):
            self.mod.ensure_http_url("ssh://git@github.com/a/b")
        with self.assertRaises(TypeError):
            self.mod.ensure_http_url(12345)  # type: ignore[arg-type]

    def test_parse_github_repo(self):
        self.assertEqual(
            self.mod.parse_github_repo("https://github.com/SuaveIV/nu_script_wttr"),
            ("SuaveIV", "nu_script_wttr"),
        )
        self.assertEqual(
            self.mod.parse_github_repo("https://github.com/owner/repo.git"),
            ("owner", "repo"),
        )
        self.assertIsNone(self.mod.parse_github_repo("https://example.com/owner/repo"))
        self.assertIsNone(self.mod.parse_github_repo("not-a-url"))

    def test_parse_semver_and_is_newer(self):
        self.assertEqual(self.mod.parse_semver_tuple("v1.2.3"), (1, 2, 3))
        self.assertEqual(self.mod.parse_semver_tuple("0.114.1"), (0, 114, 1))
        self.assertEqual(self.mod.parse_semver_tuple("0.1.0-main")[:3], (0, 1, 0))
        self.assertTrue(self.mod.is_newer("v1.3.0", "1.2.9"))
        self.assertFalse(self.mod.is_newer("v1.2.9", "1.2.9"))
        self.assertFalse(self.mod.is_newer("v1.2.0", "1.3.0"))

    def test_prerelease_ordering(self):
        # Stable is newer than same-version prerelease
        self.assertTrue(self.mod.is_newer("v1.2.0", "1.2.0-rc.1"))
        self.assertFalse(self.mod.is_newer("1.2.0-rc.1", "v1.2.0"))
        # Prerelease comparisons
        self.assertTrue(self.mod.is_newer("1.2.0-rc.2", "1.2.0-rc.1"))
        self.assertFalse(self.mod.is_newer("1.2.0-rc.1", "1.2.0-rc.2"))
        self.assertTrue(self.mod.is_newer("1.2.0-beta.1", "1.2.0-alpha.1"))
        # latest_indexed_version prefers stable over prerelease
        pkg = {
            "versions": [
                {"version": "1.2.0-rc.1"},
                {"version": "1.2.0"},
            ]
        }
        latest = self.mod.latest_indexed_version(pkg)
        assert latest is not None
        self.assertEqual(latest["version"], "1.2.0")

    def test_nu_satisfies(self):
        self.assertTrue(self.mod.nu_satisfies("*", "0.114.1"))
        self.assertTrue(self.mod.nu_satisfies(">=0.113.0 <0.115.0", "0.114.1"))
        self.assertFalse(self.mod.nu_satisfies(">=0.113.0 <0.114.0", "0.114.1"))
        self.assertFalse(self.mod.nu_satisfies(">=0.115.0", "0.114.1"))
        self.assertTrue(self.mod.nu_satisfies(">=0.114.0", "0.114.0"))
        # Legacy minor wildcard constraints
        self.assertTrue(self.mod.nu_satisfies("=0.113.x", "0.113.1"))
        self.assertTrue(self.mod.nu_satisfies("=0.113.x", "0.113.0"))
        self.assertFalse(self.mod.nu_satisfies("=0.113.x", "0.114.0"))
        self.assertTrue(self.mod.nu_satisfies("0.113.x", "0.113.2"))
        self.assertFalse(self.mod.nu_satisfies("0.113.x", "0.114.1"))
        # Wildcard with comparison operators fails closed
        self.assertFalse(self.mod.nu_satisfies(">=0.113.x", "0.114.1"))
        self.assertFalse(self.mod.nu_satisfies("<0.113.x", "0.112.0"))

    def test_detect_gaps_upstream_and_nu(self):
        packages = [
            {
                "id": {"owner": "a", "name": "mod"},
                "type": "module",
                "repo": "https://github.com/a/mod",
                "tags": ["module"],
                "versions": [
                    {
                        "version": "1.0.0",
                        "nu_version": ">=0.113.0 <0.114.0",
                        "verified_with": ["0.113.1"],
                    }
                ],
            },
            {
                "id": {"owner": "b", "name": "plug"},
                "type": "plugin",
                "repo": "https://github.com/b/plug",
                "tags": ["plugin"],
                "versions": [
                    {
                        "version": "2.0.0",
                        "nu_version": ">=0.114.0 <0.115.0",
                        "verified_with": ["0.114.1"],
                    }
                ],
            },
        ]
        upstream = {
            "a/mod": {"tag_name": "v1.1.0", "html_url": "https://github.com/a/mod/releases/tag/v1.1.0"},
            "b/plug": {"tag_name": "v2.0.0", "html_url": "https://github.com/b/plug/releases/tag/v2.0.0"},
        }
        gaps = self.mod.detect_gaps(packages, upstream, "0.114.1")
        self.assertEqual(len(gaps["upstream_updates"]), 1)
        self.assertEqual(gaps["upstream_updates"][0]["package"], "a/mod")
        self.assertEqual(len(gaps["nu_gaps"]), 1)
        self.assertEqual(gaps["nu_gaps"][0]["package"], "a/mod")

    def test_detect_gaps_skips_watch_off(self):
        packages = [
            {
                "id": {"owner": "a", "name": "mod"},
                "type": "module",
                "repo": "https://github.com/a/mod",
                "tags": ["watch-off"],
                "versions": [
                    {"version": "1.0.0", "nu_version": "*", "verified_with": []}
                ],
            }
        ]
        upstream = {"a/mod": {"tag_name": "v9.9.9", "html_url": "https://example.com"}}
        gaps = self.mod.detect_gaps(packages, upstream, "0.114.1")
        self.assertEqual(gaps["upstream_updates"], [])
        self.assertEqual(gaps["nu_gaps"], [])

    def test_format_report_markdown_contains_checklist(self):
        gaps = {
            "upstream_updates": [
                {
                    "package": "a/mod",
                    "indexed": "1.0.0",
                    "upstream": "v1.1.0",
                    "url": "https://github.com/a/mod/releases/tag/v1.1.0",
                    "type": "module",
                }
            ],
            "nu_gaps": [
                {
                    "package": "a/mod",
                    "indexed": "1.0.0",
                    "constraint": ">=0.113.0 <0.114.0",
                    "target_nu": "0.114.1",
                    "type": "module",
                }
            ],
        }
        md = self.mod.format_report_markdown(gaps, "0.114.1")
        self.assertIn("a/mod", md)
        self.assertIn("lifecycle-prove", md)
        self.assertIn("verified_with", md)

    def test_process_dry_run_writes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            index_path = Path(tmp) / "index.json"
            report_path = Path(tmp) / "report.md"
            index_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "updated_at": "2026-10-19T00:00:00Z",
                        "packages": [
                            {
                                "id": {"owner": "a", "name": "mod"},
                                "type": "module",
                                "repo": "https://github.com/a/mod",
                                "tags": [],
                                "versions": [
                                    {
                                        "version": "1.0.0",
                                        "nu_version": "*",
                                        "verified_with": ["0.114.1"],
                                    }
                                ],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            with mock.patch.object(
                self.mod,
                "fetch_latest_release",
                return_value={"tag_name": "v1.0.0", "html_url": "https://github.com/a/mod/releases/tag/v1.0.0"},
            ):
                buf = io.StringIO()
                with redirect_stdout(buf):
                    code = self.mod.process(
                        index_path=str(index_path),
                        target_nu="0.114.1",
                        write=False,
                        report_path=report_path,
                    )
                self.assertEqual(code, 0)
                self.assertFalse(report_path.exists())
                self.assertIn("dry-run", buf.getvalue().lower())

    def test_fetch_latest_release_filters_prerelease_and_paginates(self):
        # Page 1 has 30 items: one backport stable v1.8.0 and drafts/prereleases
        page1 = [
            {"tag_name": "v2.0.0-rc.1", "draft": False, "prerelease": True},
            {"tag_name": "v1.8.0", "draft": False, "prerelease": False, "html_url": "https://example.com/1.8"},
        ] + [{"tag_name": f"v0.{i}.0", "draft": True, "prerelease": False} for i in range(28)]

        # Page 2 has the actual latest release v1.9.0
        page2 = [
            {"tag_name": "v1.9.0", "draft": False, "prerelease": False, "html_url": "https://example.com/1.9"},
        ]

        class FakeResp:
            def __init__(self, data: bytes):
                self._data = data

            def read(self):
                return self._data

            def __enter__(self):
                return self

            def __exit__(self, *a):
                pass

        calls = []

        def fake_open(req, timeout=30):
            calls.append(req.full_url)
            if "page=1" in req.full_url:
                return FakeResp(json.dumps(page1).encode("utf-8"))
            return FakeResp(json.dumps(page2).encode("utf-8"))

        with mock.patch.object(self.mod, "http_opener") as opener_fn:
            opener = mock.MagicMock()
            opener.open.side_effect = fake_open
            opener_fn.return_value = opener
            rel = self.mod.fetch_latest_release("a", "b")
            assert rel is not None
            # Highest semver across pages must be chosen (v1.9.0, not v1.8.0)
            self.assertEqual(rel["tag_name"], "v1.9.0")
            self.assertEqual(len(calls), 2)

    def test_fetch_json_url_does_not_leak_auth_to_non_github(self):
        os.environ["GITHUB_TOKEN"] = "secret-token-12345"

        captured_headers = {}

        class FakeResp:
            def read(self):
                return b"{}"

            def __enter__(self):
                return self

            def __exit__(self, *a):
                pass

        def fake_open(req, timeout=30):
            captured_headers.update(req.headers)
            return FakeResp()

        with mock.patch.object(self.mod, "http_opener") as opener_fn:
            opener = mock.MagicMock()
            opener.open.side_effect = fake_open
            opener_fn.return_value = opener

            # Custom / registry host must not receive Authorization
            self.mod.fetch_json_url("https://custom-registry.example.com/index.json")
            self.assertNotIn("Authorization", captured_headers)
            self.assertNotIn("authorization", captured_headers)

            # api.github.com host must receive Authorization
            captured_headers.clear()
            self.mod.fetch_json_url("https://api.github.com/repos/a/b")
            self.assertIn("Authorization", captured_headers)
            self.assertEqual(captured_headers["Authorization"], "Bearer secret-token-12345")

    def test_resolve_target_nu_fail_closed(self):
        # Explicit input works
        self.assertEqual(self.mod.resolve_target_nu("v0.115.0"), "0.115.0")
        # Missing state file raises RuntimeError
        with tempfile.TemporaryDirectory() as tmp:
            missing = Path(tmp) / "does-not-exist.json"
            with mock.patch.object(self.mod, "NU_STATE_FILE", missing), self.assertRaises(RuntimeError):
                self.mod.resolve_target_nu(None)

    def test_lookup_errors_recorded_and_output(self):
        packages = [
            {
                "id": {"owner": "fail", "name": "pkg"},
                "type": "module",
                "repo": "https://github.com/fail/pkg",
                "tags": [],
                "versions": [{"version": "1.0.0", "nu_version": "*"}],
            }
        ]
        with tempfile.TemporaryDirectory() as tmp:
            index_path = Path(tmp) / "index.json"
            report_path = Path(tmp) / "report.md"
            output_path = Path(tmp) / "github_output.txt"
            os.environ["GITHUB_OUTPUT"] = str(output_path)
            index_path.write_text(json.dumps({"packages": packages}), encoding="utf-8")

            with mock.patch.object(
                self.mod,
                "fetch_latest_release",
                side_effect=RuntimeError("rate limit exceeded"),
            ):
                code = self.mod.process(
                    index_path=str(index_path),
                    target_nu="0.114.1\n",  # Test newline sanitization too
                    write=True,
                    report_path=report_path,
                )
            self.assertEqual(code, 0)
            report_text = report_path.read_text(encoding="utf-8")
            self.assertIn("## Lookup errors", report_text)
            self.assertIn("rate limit exceeded", report_text)

            output_text = output_path.read_text(encoding="utf-8")
            self.assertIn("error_count=1", output_text)
            self.assertIn("target_nu=0.114.1\n", output_text)  # newline stripped inside value


if __name__ == "__main__":
    unittest.main()
