#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_workflows

VALID = """\
name: ok
on:
  push:
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
"""

MALFORMED = "name: bad\non: [push\n"

DUPLICATE_KEY = """\
name: dup
on: push
on: pull_request
"""


def init_repo(root: Path) -> None:
    subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)


def add_workflow(root: Path, name: str, text: str, tracked: bool = True) -> Path:
    path = root / ".github" / "workflows" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    if tracked:
        subprocess.run(
            ["git", "-C", str(root), "add", ".github/workflows/" + name],
            check=True,
        )
    return path


class CheckWorkflowsTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        init_repo(self.root)

    def tearDown(self):
        self._tmp.cleanup()

    def test_valid_workflow_passes(self):
        add_workflow(self.root, "ci.yml", VALID)
        self.assertEqual(check_workflows.validate_workflows(self.root), [])

    def test_malformed_yaml_fails(self):
        add_workflow(self.root, "ci.yml", MALFORMED)
        errors = check_workflows.validate_workflows(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("ci.yml", errors[0])

    def test_duplicate_mapping_key_fails(self):
        add_workflow(self.root, "ci.yml", DUPLICATE_KEY)
        errors = check_workflows.validate_workflows(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("duplicate", errors[0].lower())
        self.assertIn("ci.yml", errors[0])

    def test_on_key_stays_literal(self):
        path = add_workflow(self.root, "ci.yml", VALID)
        doc = check_workflows.yaml.load(
            path.read_text(encoding="utf-8"),
            Loader=check_workflows.UniqueKeyLoader,
        )
        self.assertIn("on", doc)
        self.assertNotIn(True, doc)

    def test_both_extensions_checked(self):
        add_workflow(self.root, "a.yml", VALID)
        add_workflow(self.root, "b.yaml", MALFORMED)
        errors = check_workflows.validate_workflows(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("b.yaml", errors[0])

    def test_untracked_malformed_workflow_ignored(self):
        add_workflow(self.root, "ci.yml", VALID)
        add_workflow(self.root, "draft.yml", MALFORMED, tracked=False)
        self.assertEqual(check_workflows.validate_workflows(self.root), [])

    def test_no_tracked_workflows_fails(self):
        errors = check_workflows.validate_workflows(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("no tracked workflow", errors[0].lower())

    def test_unhashable_key_and_other_errors_collected(self):
        add_workflow(self.root, "complex.yml", "name: x\n? [a, b]\n: value\n")
        add_workflow(self.root, "bad.yml", MALFORMED)
        errors = check_workflows.validate_workflows(self.root)
        self.assertEqual(len(errors), 2)
        joined = "\n".join(errors)
        self.assertIn("complex.yml", joined)
        self.assertIn("bad.yml", joined)

    def test_invalid_non_first_workflow_still_found(self):
        add_workflow(self.root, "a.yml", VALID)
        add_workflow(self.root, "b.yml", VALID)
        add_workflow(self.root, "c.yml", MALFORMED)
        errors = check_workflows.validate_workflows(self.root)
        self.assertEqual(len(errors), 1)
        self.assertIn("c.yml", errors[0])


if __name__ == "__main__":
    unittest.main()
