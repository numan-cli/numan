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

    def test_ensure_http_url_rejects_non_http(self):
        self.mod.ensure_http_url("https://api.github.com/repos/a/b/releases")
        with self.assertRaises(ValueError):
            self.mod.ensure_http_url("file:///etc/passwd")
        with self.assertRaises(ValueError):
            self.mod.ensure_http_url("ssh://git@github.com/a/b")

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
        self.assertEqual(self.mod.parse_semver_tuple("0.1.0-main"), (0, 1, 0))
        self.assertTrue(self.mod.is_newer("v1.3.0", "1.2.9"))
        self.assertFalse(self.mod.is_newer("v1.2.9", "1.2.9"))
        self.assertFalse(self.mod.is_newer("v1.2.0", "1.3.0"))

    def test_nu_satisfies(self):
        self.assertTrue(self.mod.nu_satisfies("*", "0.114.1"))
        self.assertTrue(self.mod.nu_satisfies(">=0.113.0 <0.115.0", "0.114.1"))
        self.assertFalse(self.mod.nu_satisfies(">=0.113.0 <0.114.0", "0.114.1"))
        self.assertFalse(self.mod.nu_satisfies(">=0.115.0", "0.114.1"))
        self.assertTrue(self.mod.nu_satisfies(">=0.114.0", "0.114.0"))

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

    def test_fetch_latest_release_filters_prerelease(self):
        payload = json.dumps(
            [
                {"tag_name": "v2.0.0-rc.1", "draft": False, "prerelease": True},
                {"tag_name": "v1.9.0", "draft": False, "prerelease": False, "html_url": "https://github.com/a/b/releases/tag/v1.9.0"},
            ]
        ).encode("utf-8")

        class FakeResp:
            def read(self):
                return payload

            def __enter__(self):
                return self

            def __exit__(self, *a):
                pass

        with mock.patch.object(self.mod, "http_opener") as opener_fn:
            opener = mock.MagicMock()
            opener.open.return_value = FakeResp()
            opener_fn.return_value = opener
            rel = self.mod.fetch_latest_release("a", "b")
            self.assertEqual(rel["tag_name"], "v1.9.0")


if __name__ == "__main__":
    unittest.main()
