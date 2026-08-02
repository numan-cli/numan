#!/usr/bin/env python3
"""
check-roadmap-drift.py

Detect roadmap drift between the consolidated cross-repo plan and shipped code,
so a contradiction like PR 67 (where `numan use` shipped but the roadmap still
called it post-1.0) cannot recur without blocking CI.

What this checks
================

1. The consolidated roadmap at `docs/plans/consolidated-multi-repo-roadmap.md`
   exists and explicitly tells readers that repo-local roadmaps link back
   here, naming `numan-plugins/docs/roadmap.md` and
   `numan-registry/docs/roadmap.md` (no silent fence-sitting).

2. No "shipped-feature bullet" in the consolidated roadmap contains the
   forbidden phrases `stub`, `reserved`, or `post-1.0 reserved`. Bullets under
   deferral / post-1.0 / shelfware sections are exempt because deferral
   language is appropriate there. Bullets under "Current Baseline",
   "Ship 1.0 when …", `[x]` checkboxes, or any line that quotes an actual
   CLI command (`numan use …`, `numan setup nu …`) count as shipped.

3. If a repo-local `docs/roadmap.md` exists, it links (relative or absolute)
   back to the consolidated roadmap so a future maintainer knows where to
   track drift.

Modes
=====

Default (run from each repo's repo root):

    python scripts/check-roadmap-drift.py

Override paths are rare; if you need them, env vars override:

    CONSOLIDATED_ROADMAP=path MD LOCAL_ROADMAP=path

The script exits 1 if any rule fails; warnings don't fail CI.
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import Iterable

# --- Configuration ---------------------------------------------------------

CONSOLIDATED_PATH = Path(
    os.environ.get(
        "CONSOLIDATED_ROADMAP",
        "docs/plans/consolidated-multi-repo-roadmap.md",
    )
)
LOCAL_PATH = Path(
    os.environ.get("LOCAL_ROADMAP", "docs/roadmap.md")
)

# Repo-local roadmap name pattern; the consolidated roadmap should reference
# these by GitHub URL so we can fan the audit out across the three repos.
REMOTE_ROADMAP_LINKS = (
    "numan-plugins/docs/roadmap.md",
    "numan-registry/docs/roadmap.md",
)

# Forbidden phrases in shipped-feature bullets.
# Case-insensitive; word-boundary anchored; appropriate only in deferral
# contexts ("we'll defer this until …").
FORBIDDEN_PHRASES: tuple[str, ...] = (
    r"\bstub\b",
    r"\breserved\b",
    r"\bpost-1\.0 reserved\b",
    r"\bexists as a stub\b",
    r"\bis a reserved\b",
)

# Section / line patterns ---------------------------------------------------

# A heading whose presence starts a deferral context, so the forbidden
# phrases inside are not flagged. Lower-case match.
DEFERRED_HEADING_RE = re.compile(
    r"^#{1,4}\s+(?P<name>"
    r"post[-\s]1\.0\s+features"
    r"|explicitly\s+deferred"
    r"|explicitly\s+not\s+in\s+scope"
    r"|explicitly\s+closed\s+in\s+this\s+pr"
    r"|post[-\s]1\.0\s+v\s*\d+"
    r"|"
    r"shelfware"
    r"|"
    r"future features"
    r"|"
    r"deferred\s+plugin\s+candidates"
    r")\b",
    re.IGNORECASE,
)

# A bullet whose body describes a shipped feature.
# Markers (any one of these is enough):
#   - `[x]` checkbox
#   - `Wave N closed`, `Phase N` markers with `shipped|complete|merged|done`
#   - CLI command pattern (`numan <verb>`) — concrete executable behavior
#   - "shipped", "merged", "closed" keywords in a baseline/baseline-like line
SHIPPED_MARKERS = (
    re.compile(r"\[\s*[xX]\s*\]"),
    re.compile(r"\bwave\s+\d+\s+(closed|merged|shipped)\b", re.IGNORECASE),
    re.compile(
        r"\b(phase\s+\d+(?:\.\d+)?\s*(?:\()?.*?(shipped|complete|merged|done)\)?)\b",
        re.IGNORECASE,
    ),
    re.compile(r"`numan\s+[a-z][a-z0-9_]*"),
    re.compile(r"`numan-\w+\s+[a-z][a-z0-9_]*"),
    re.compile(r"`scripts/[a-z_]+\.py"),
)

BULLET_RE = re.compile(r"^\s*[-*]\s+", re.IGNORECASE)
HEAD_RE = re.compile(r"^(#{1,6})\s+", re.IGNORECASE)


def _is_deferred_context(stack: list[str]) -> bool:
    """True when any heading in scope is a deferral heading."""
    return any(DEFERRED_HEADING_RE.match(h) for h in stack)


def audit_consolidated(path: Path) -> tuple[list[str], list[str]]:
    """Return (errors, warnings) for the consolidated roadmap."""
    errors: list[str] = []
    warnings: list[str] = []

    if not path.exists():
        errors.append(f"missing consolidated roadmap: {path}")
        return errors, warnings

    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()

    # Preamble must name all remote roadmap links ("repo-local roadmaps keep
    # operational detail and should link here: …").
    if "repo-local" not in text.lower():
        errors.append(
            f"{path}: preamble must declare the repo-local roadmap rule "
            "(phrase: 'repo-local roadmaps keep operational detail')."
        )
    for link in REMOTE_ROADMAP_LINKS:
        if link not in text:
            errors.append(
                f"{path}: preamble must mention `{link}` so each repo's "
                "docs/roadmap.md points back here."
            )

    # Forbidden phrases in shipped-feel bullets outside deferral scope.
    heading_stack: list[str] = []
    for n, raw in enumerate(lines, start=1):
        line = raw.rstrip()
        if HEAD_RE.match(line):
            heading_stack.append(line)
            continue

        # Pop headings as we leave their scope. The markdown is single-level
        # for our purposes: if we see an H2, pop everything above H2; an H3
        # pops H3+ above; H1 resets everything.
        # Treat consecutive headings as a stack.
        # Cheap implementation: drop trailing headings shorter (lower
        # depth) than current heading level. We re-derive from raw line.
        head_match = HEAD_RE.match(line)
        if head_match:
            depth = len(head_match.group(1))
            heading_stack[:] = [
                h for h in heading_stack if len(HEAD_RE.match(h).group(1)) < depth
            ]
            heading_stack.append(line)
            continue

        if not BULLET_RE.match(line):
            continue

        # Shipped-feel? Look for known shipped markers.
        shipped = any(p.search(line) for p in SHIPPED_MARKERS)

        # Deferral context exempt.
        if _is_deferred_context(heading_stack):
            continue

        if not shipped:
            continue

        # Bullet both feels shipped AND is not under a deferral header.
        for pat in FORBIDDEN_PHRASES:
            if re.search(pat, line, re.IGNORECASE):
                errors.append(
                    f"{path}:{n}: shipped-feel bullet contains forbidden "
                    f"phrase matching `{pat}`: {line.strip()[:120]!r}"
                )

    # Soft warning: if ALL bullets are deferred we'd flag the doc as
    # stale; we don't fail but we do log so the author notices.
    shipped_visible = sum(
        1
        for raw in lines
        if BULLET_RE.match(raw) and any(p.search(raw) for p in SHIPPED_MARKERS)
    )
    if shipped_visible == 0:
        warnings.append(
            f"{path}: no shipped-feel bullets found — the consolidated "
            "roadmap may be in deferral-review-only mode."
        )

    return errors, warnings


def audit_local(path: Path, name: str) -> tuple[list[str], list[str]]:
    """Return (errors, warnings) for a repo-local roadmap file."""
    errors: list[str] = []
    warnings: list[str] = []
    if not path.exists():
        # Repo-local roadmap is optional for the source-of-truth repo
        # (`numan`); expected for downstream repos (`numan-plugins`,
        # `numan-registry`). We can't tell which we're in without env
        # cooperation, so log a warning.
        warnings.append(
            f"missing repo-local roadmap: {path} — add one if this repo "
            "is numan-plugins or numan-registry."
        )
        return errors, warnings

    text = path.read_text(encoding="utf-8")
    if CONSOLIDATED_PATH.name not in text and "consolidated-multi-repo-roadmap" not in text:
        errors.append(
            f"{path}: must link to the consolidated roadmap "
            f"(reference `{CONSOLIDATED_PATH.name}` or its full GitHub URL)."
        )
    if name == "consolidated":
        # The `numan` repo itself hosts the consolidated roadmap; its
        # docs/roadmap.md should be a thin pointer or explicitly say
        # "this repo owns the consolidated roadmap".
        if "consolidated" not in text.lower():
            warnings.append(
                f"{path}: in `numan`, the local roadmap should mention "
                "'consolidated' so contributors know the cross-repo plan lives here."
            )
    return errors, warnings


def main(argv: list[str]) -> int:
    consolidated_path = (
        Path(argv[1]) if len(argv) > 1 else CONSOLIDATED_PATH
    )

    errors, warnings = [], []
    e, w = audit_consolidated(consolidated_path)
    errors += e
    warnings += w

    e, w = audit_local(LOCAL_PATH, name="consolidated")
    errors += e
    warnings += w

    for w in warnings:
        print(f"warning: {w}")
    for e in errors:
        print(f"error: {e}")
    print(
        f"roadmap-drift check: {len(errors)} error(s), {len(warnings)} warning(s)"
    )
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
