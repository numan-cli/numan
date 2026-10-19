#!/usr/bin/env python3

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

import yaml


class UniqueKeyLoader(yaml.SafeLoader):
    pass


# GitHub Actions treats on/off/yes/no as plain strings; restrict implicit
# boolean resolution to true/false so `on:` stays a literal string key while
# other scalars still get resolver-aware duplicate-key checks.
UniqueKeyLoader.yaml_implicit_resolvers = {}
for _ch, _resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items():
    _filtered = [
        (tag, regexp)
        for tag, regexp in _resolvers
        if tag != "tag:yaml.org,2002:bool"
    ]
    if _filtered:
        UniqueKeyLoader.yaml_implicit_resolvers[_ch] = _filtered
UniqueKeyLoader.add_implicit_resolver(
    "tag:yaml.org,2002:bool",
    re.compile(r"^(?:true|True|TRUE|false|False|FALSE)$"),
    list("tTfF"),
)


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


def load_workflow(stream):
    loader = UniqueKeyLoader(stream)
    try:
        return loader.get_single_data()
    finally:
        loader.dispose()


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


def validate_workflows(root: Path, files: list[Path] | None = None) -> list[str]:
    if files is None:
        files = workflow_files(root)
    if not files:
        return [f"{root}: no tracked workflow files under .github/workflows"]
    errors = []
    for path in files:
        try:
            with path.open("rb") as fh:
                load_workflow(fh)
        except (OSError, yaml.YAMLError) as exc:
            errors.append(f"{path}: {exc}")
    return errors


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    files = workflow_files(root)
    errors = validate_workflows(root, files=files)
    for err in errors:
        print(err, file=sys.stderr)
    if errors:
        return 1
    print(f"validated {len(files)} workflow files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
