#!/usr/bin/env python3
"""Unit checks for scripts/render_homebrew_formula.py."""

from __future__ import annotations

import importlib.util
import io
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "render_homebrew_formula.py"


def load_mod():
    spec = importlib.util.spec_from_file_location("render_homebrew_formula", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


class RenderHomebrewFormulaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_mod()

    def test_parse_and_render(self):
        sums = """
4d8fa065b5bc7fcce30af3ca7d5c3cd943701bee168d78fae6120a12689738b8  numan-0.1.5-aarch64-apple-darwin.tar.gz
2d855b3b8a9bb3c568051024b8aae9771a63c427cb3edfd3ac3aa3aa6d78468d  numan-0.1.5-x86_64-pc-windows-msvc.zip
0b113361c189a2062ef6e1fca36795d2347b925c1862b42c4ddeb54773e00ae3  numan-0.1.5-x86_64-unknown-linux-gnu.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  numan-0.1.5-aarch64-unknown-linux-gnu.tar.gz
""".strip()
        digests = self.mod.parse_sha256sums(sums, "0.1.5")
        self.assertEqual(
            digests["aarch64-apple-darwin"],
            "4d8fa065b5bc7fcce30af3ca7d5c3cd943701bee168d78fae6120a12689738b8",
        )
        self.assertEqual(
            digests["aarch64-unknown-linux-gnu"],
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        text = self.mod.render_formula("0.1.5", digests)
        self.assertIn('version "0.1.5"', text)
        self.assertIn("numan-#{version}-aarch64-apple-darwin.tar.gz", text)
        self.assertIn("numan-#{version}-aarch64-unknown-linux-gnu.tar.gz", text)
        self.assertIn(
            "4d8fa065b5bc7fcce30af3ca7d5c3cd943701bee168d78fae6120a12689738b8", text
        )
        self.assertIn(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", text
        )
        self.assertIn('bin.install "numan"', text)
        self.assertNotIn("arch_dir", text)
        self.assertNotIn("expected numan-* directory", text)
        self.assertIn("on_intel do", text)
        self.assertIn("no longer ships Intel Mac", text)
        self.assertNotIn("x86_64-apple-darwin.tar.gz", text)
        self.assertNotIn("windows-msvc", text)

    def test_parse_prerelease_version(self):
        sums = """
4D8FA065B5BC7FCCE30AF3CA7D5C3CD943701BEE168D78FAE6120A12689738B8  numan-0.2.0-beta.1-aarch64-apple-darwin.tar.gz
2d855b3b8a9bb3c568051024b8aae9771a63c427cb3edfd3ac3aa3aa6d78468d  numan-0.2.0-beta.1-x86_64-unknown-linux-gnu.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  numan-0.2.0-beta.1-aarch64-unknown-linux-gnu.tar.gz
""".strip()
        digests = self.mod.parse_sha256sums(sums, "0.2.0-beta.1")
        self.assertEqual(
            digests["aarch64-apple-darwin"],
            "4d8fa065b5bc7fcce30af3ca7d5c3cd943701bee168d78fae6120a12689738b8",
        )
        self.assertEqual(
            digests["aarch64-unknown-linux-gnu"],
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )

    def test_missing_asset_fails(self):
        sums = "abcd  numan-0.1.5-aarch64-apple-darwin.tar.gz\n"
        with self.assertRaises(SystemExit):
            self.mod.parse_sha256sums(sums, "0.1.5")

    def test_write_roundtrip(self):
        sums = """
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  numan-0.2.0-aarch64-apple-darwin.tar.gz
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  numan-0.2.0-x86_64-unknown-linux-gnu.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  numan-0.2.0-aarch64-unknown-linux-gnu.tar.gz
""".strip()
        with tempfile.TemporaryDirectory() as tmp:
            sums_path = Path(tmp) / "SHA256SUMS"
            out = Path(tmp) / "numan.rb"
            sums_path.write_text(sums + "\n", encoding="utf-8")
            digests = self.mod.parse_sha256sums(sums_path.read_text(encoding="utf-8"), "0.2.0")
            text = self.mod.render_formula("0.2.0", digests)
            with out.open("w", encoding="utf-8", newline="\n") as handle:
                handle.write(text)
            body = out.read_text(encoding="utf-8")
            self.assertEqual(body, text)
            self.assertIn('version "0.2.0"', body)
            self.assertIn("aarch64-unknown-linux-gnu.tar.gz", body)

    def test_cli_check_url_layout(self):
        buf = io.StringIO()
        with redirect_stdout(buf):
            code = self.mod.main(["--check-url-layout"])
        self.assertEqual(0, code)
        stdout = buf.getvalue()
        self.assertIn("numan-<version>-aarch64-apple-darwin.tar.gz", stdout)
        self.assertIn("numan-<version>-x86_64-unknown-linux-gnu.tar.gz", stdout)
        self.assertIn("numan-<version>-aarch64-unknown-linux-gnu.tar.gz", stdout)
        self.assertNotIn("x86_64-apple-darwin", stdout)

    def test_cli_full_write(self):
        sums = """
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  numan-0.2.0-aarch64-apple-darwin.tar.gz
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  numan-0.2.0-x86_64-unknown-linux-gnu.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  numan-0.2.0-aarch64-unknown-linux-gnu.tar.gz
""".strip()
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            sums_path = tmp_path / "SHA256SUMS"
            out_path = tmp_path / "numan.rb"
            sums_path.write_text(sums + "\n", encoding="utf-8")
            buf = io.StringIO()
            with redirect_stdout(buf):
                code = self.mod.main(
                    [
                        "--version",
                        "0.2.0",
                        "--sha256sums",
                        str(sums_path),
                        "--write",
                        "--out",
                        str(out_path),
                    ]
                )
            self.assertEqual(0, code)
            self.assertTrue(out_path.is_file())
            text = out_path.read_text(encoding="utf-8")
            self.assertIn('version "0.2.0"', text)
            self.assertIn("aarch64-unknown-linux-gnu.tar.gz", text)
            self.assertIn("Wrote ", buf.getvalue())

    def test_cli_missing_required_args_fails(self):
        err = io.StringIO()
        with redirect_stderr(err), self.assertRaises(SystemExit) as cm:
            self.mod.main([])
        self.assertNotEqual(0, cm.exception.code)
        self.assertIn("--version", err.getvalue())


if __name__ == "__main__":
    unittest.main()
