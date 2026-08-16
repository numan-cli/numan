#!/usr/bin/env python3
"""Unit checks for scripts/watch_nu_releases.py."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "watch_nu_releases.py"


def load_mod():
    spec = importlib.util.spec_from_file_location("watch_nu_releases", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


class FakeResponse:
    def __init__(self, data: bytes) -> None:
        self._data = data

    def read(self) -> bytes:
        return self._data

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        pass


class WatchNuReleasesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_mod()

    def test_ensure_http_url_valid(self):
        self.mod.ensure_http_url("https://api.github.com/repos/nushell/nushell/releases/latest")
        self.mod.ensure_http_url("http://example.com/test")
        self.mod.ensure_http_url("HTTPS://WWW.NUSHELL.SH/BLOG/TEST.HTML")

    def test_ensure_http_url_invalid_schemes_and_forms(self):
        invalid_urls = [
            "file:///etc/passwd",
            "ftp://example.com",
            "ssh://git@github.com",
            "custom://api",
            "https:",
            "https://",
            "https:evil.com",
            "javascript:alert(1)",
            None,
            123,
        ]
        for url in invalid_urls:
            with self.subTest(url=url):
                with self.assertRaises(ValueError):
                    self.mod.ensure_http_url(url)

    def test_http_redirect_handler_guards_redirect(self):
        handler = self.mod._HttpOnlyRedirectHandler()
        req = mock.Mock()
        with self.assertRaises(ValueError):
            handler.redirect_request(req, None, 302, "Found", {}, "file:///etc/passwd")

    def test_fetch_latest_release_success(self):
        payload = json.dumps({"tag_name": "v0.115.0", "html_url": "https://github.com/nushell/nushell/releases/tag/0.115.0"}).encode("utf-8")
        fake_resp = FakeResponse(payload)

        with mock.patch.object(self.mod, "http_opener") as mock_opener_fn, \
             mock.patch.dict(os.environ, {"GITHUB_TOKEN": "secret_token"}):
            mock_opener = mock.MagicMock()
            mock_opener.open.return_value = fake_resp
            mock_opener_fn.return_value = mock_opener

            res = self.mod.fetch_latest_release("https://api.github.com/test")
            self.assertEqual(res["tag_name"], "v0.115.0")
            mock_opener.open.assert_called_once()
            req = mock_opener.open.call_args[0][0]
            self.assertEqual(req.get_header("Authorization"), "Bearer secret_token")
            self.assertEqual(req.get_header("User-agent"), self.mod.USER_AGENT)

    def test_parse_version_tuple(self):
        self.assertEqual(self.mod.parse_version_tuple("0.115.0"), (0, 115, 0))
        self.assertEqual(self.mod.parse_version_tuple("v0.115.1"), (0, 115, 1))
        self.assertEqual(self.mod.parse_version_tuple("v1.2.3 extra"), (1, 2, 3))
        self.assertEqual(self.mod.parse_version_tuple("0.115.0-beta.1"), (0, 115, 0))
        self.assertEqual(self.mod.parse_version_tuple("not-a-version"), (0, 0, 0))

    def test_extract_blog_url(self):
        body_with_html = "See the blog at https://www.nushell.sh/blog/2026-08-15-nushell_0_115_0.html for details."
        self.assertEqual(
            self.mod.extract_blog_url(body_with_html),
            "https://www.nushell.sh/blog/2026-08-15-nushell_0_115_0.html",
        )

        body_without_html = "Release blog: https://nushell.sh/blog/2026-08-15-nushell_0_115_0"
        self.assertEqual(
            self.mod.extract_blog_url(body_without_html),
            "https://nushell.sh/blog/2026-08-15-nushell_0_115_0.html",
        )

        body_without_link = "No blog link in this release description."
        self.assertIsNone(self.mod.extract_blog_url(body_without_link))

    def test_generate_release_entry(self):
        entry = self.mod.generate_release_entry(
            version="0.115.0",
            date_str="2026-08-15",
            github_url="https://github.com/nushell/nushell/releases/tag/0.115.0",
            blog_url="https://www.nushell.sh/blog/2026-08-15-nushell_0_115_0.html",
            major=0,
            minor=115,
        )
        self.assertIn("## Nushell 0.115.0 (2026-08-15)", entry)
        self.assertIn("nu-plugin` `0.115.x`", entry)
        self.assertIn('nu_version: ">=0.115.0 <0.116.0"', entry)
        self.assertIn("https://www.nushell.sh/blog/2026-08-15-nushell_0_115_0.html", entry)

    def test_state_load_and_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            state_file = Path(tmp) / "latest-nu-version.json"
            self.assertEqual(self.mod.load_current_state(state_file), {})

            state_file.write_text("invalid json", encoding="utf-8")
            self.assertEqual(self.mod.load_current_state(state_file), {})

            buf = io.StringIO()
            with redirect_stdout(buf):
                self.mod.write_state(state_file, {"latest_version": "0.115.0"})
            loaded = self.mod.load_current_state(state_file)
            self.assertEqual(loaded.get("latest_version"), "0.115.0")

    def test_update_releases_markdown(self):
        with tempfile.TemporaryDirectory() as tmp:
            md_file = Path(tmp) / "nushell-releases.md"
            entry_115 = self.mod.generate_release_entry("0.115.0", "2026-08-15", "https://github.com/...", None, 0, 115)
            entry_116 = self.mod.generate_release_entry("0.116.0", "2026-09-15", "https://github.com/...", None, 0, 116)

            buf = io.StringIO()
            with redirect_stdout(buf):
                # Case 1: Fresh file
                updated = self.mod.update_releases_markdown(md_file, entry_115, "0.115.0")
                self.assertTrue(updated)
                content = md_file.read_text(encoding="utf-8")
                self.assertIn("# Official Nushell Release & Stewardship Log", content)
                self.assertIn("## Nushell 0.115.0", content)

                # Case 2: Prepend newer release
                updated = self.mod.update_releases_markdown(md_file, entry_116, "0.116.0")
                self.assertTrue(updated)
                content = md_file.read_text(encoding="utf-8")
                self.assertTrue(content.index("## Nushell 0.116.0") < content.index("## Nushell 0.115.0"))

                # Case 3: Version already present -> idempotent
                updated = self.mod.update_releases_markdown(md_file, entry_116, "0.116.0")
                self.assertFalse(updated)

    def test_github_actions_outputs_and_summary(self):
        with tempfile.TemporaryDirectory() as tmp:
            gh_output = Path(tmp) / "github_output"
            gh_summary = Path(tmp) / "github_summary"

            with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(gh_output), "GITHUB_STEP_SUMMARY": str(gh_summary)}):
                self.mod.write_github_actions_outputs(
                    is_new=True,
                    tag_name="0.115.0",
                    github_url="https://github.com/nushell/nushell/releases/tag/0.115.0",
                    blog_url="https://www.nushell.sh/blog/0.115.0.html",
                )
                self.mod.write_github_step_summary(
                    tag_name="0.115.0",
                    date_str="2026-08-15",
                    github_url="https://github.com/nushell/nushell/releases/tag/0.115.0",
                    blog_url="https://www.nushell.sh/blog/0.115.0.html",
                    new_entry="## Nushell 0.115.0",
                )

            out_text = gh_output.read_text(encoding="utf-8")
            self.assertIn("is_new=true\n", out_text)
            self.assertIn("version=0.115.0\n", out_text)
            self.assertIn("blog_url=https://www.nushell.sh/blog/0.115.0.html\n", out_text)

            sum_text = gh_summary.read_text(encoding="utf-8")
            self.assertIn("### 🚀 New Nushell Release Detected: 0.115.0", sum_text)

    def test_process_release_workflow(self):
        with tempfile.TemporaryDirectory() as tmp:
            state_file = Path(tmp) / "latest-nu-version.json"
            md_file = Path(tmp) / "nushell-releases.md"

            release = {
                "tag_name": "v0.115.0",
                "html_url": "https://github.com/nushell/nushell/releases/tag/0.115.0",
                "published_at": "2026-08-15T12:00:00Z",
                "body": "See https://www.nushell.sh/blog/2026-08-15-nushell_0_115_0.html",
            }

            buf = io.StringIO()
            with redirect_stdout(buf):
                # 1. Dry run should not create files
                code = self.mod.process_release(release, write=False, dry_run=True, force=False, state_file=state_file, releases_md=md_file)
                self.assertEqual(code, 0)
                self.assertFalse(state_file.exists())
                self.assertFalse(md_file.exists())

                # 2. Write updates files
                code = self.mod.process_release(release, write=True, dry_run=False, force=False, state_file=state_file, releases_md=md_file)
                self.assertEqual(code, 0)
                self.assertTrue(state_file.exists())
                self.assertTrue(md_file.exists())
                self.assertEqual(self.mod.load_current_state(state_file).get("latest_version"), "0.115.0")

                # 3. Subsequent run without new version returns 0 without re-writing
                code = self.mod.process_release(release, write=True, dry_run=False, force=False, state_file=state_file, releases_md=md_file)
                self.assertEqual(code, 0)

                # 4. Force run succeeds
                code = self.mod.process_release(release, write=True, dry_run=False, force=True, state_file=state_file, releases_md=md_file)
                self.assertEqual(code, 0)

    def test_main_cli_execution(self):
        fake_release = {
            "tag_name": "v0.115.0",
            "html_url": "https://github.com/nushell/nushell/releases/tag/0.115.0",
            "published_at": "2026-08-15T12:00:00Z",
            "body": "Nushell release",
        }
        with mock.patch.object(self.mod, "fetch_latest_release", return_value=fake_release):
            buf = io.StringIO()
            with redirect_stdout(buf):
                code = self.mod.main(["--dry-run"])
            self.assertEqual(code, 0)
            self.assertIn("Detected latest release: 0.115.0", buf.getvalue())

    def test_main_cli_fetch_error_handled(self):
        with mock.patch.object(self.mod, "fetch_latest_release", side_effect=OSError("network failure")):
            err = io.StringIO()
            with redirect_stderr(err):
                code = self.mod.main(["--dry-run"])
            self.assertEqual(code, 1)
            self.assertIn("Error fetching release: network failure", err.getvalue())


if __name__ == "__main__":
    unittest.main()
