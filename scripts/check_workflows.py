#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import yaml


class UniqueKeyLoader(yaml.BaseLoader):
    pass


def _construct_mapping(loader, node, deep=False):
    mapping = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=True)
        try:
            duplicate = key in mapping
        except TypeError:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                "found unhashable key",
                key_node.start_mark,
            )
        if duplicate:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                "found duplicate key {!r}".format(key),
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


UniqueKeyLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
    _construct_mapping,
)


def workflow_files(root: Path) -> list[Path]:
    result = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", "--", ".github/workflows"],
        check=True,
        capture_output=True,
    )
    return [
        root / rel
        for rel in result.stdout.decode("utf-8").split("\0")
        if rel and Path(rel).suffix in (".yml", ".yaml")
    ]


def validate_workflows(root: Path) -> list[str]:
    files = workflow_files(root)
    if not files:
        return [f"{root}: no tracked workflow files under .github/workflows"]
    errors = []
    for path in files:
        try:
            with path.open(encoding="utf-8") as fh:
                yaml.load(fh, Loader=UniqueKeyLoader)
        except (OSError, yaml.YAMLError) as exc:
            errors.append(f"{path}: {exc}")
    return errors


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    errors = validate_workflows(root)
    for err in errors:
        print(err, file=sys.stderr)
    if errors:
        return 1
    print(f"validated {len(workflow_files(root))} workflow files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
