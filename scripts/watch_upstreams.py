#!/usr/bin/env python3
"""Watch indexed packages for upstream releases and Nu-version gaps.

Detection only. This script never mutates the registry index, never
signs, and never publishes. Its only write (with ``--write``) is a
Markdown report. Promotion stays PR-gated: a maintainer reviews the
report, runs candidate generation + lifecycle-prove in
``numan-registry``, and merges manually.

Usage:
  python scripts/watch_upstreams.py [--dry-run] [--write]
      [--index PATH|URL] [--nu-version X.Y.Z] [--report PATH]
      [--packages owner/name,...]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Self
from urllib.parse import urlencode, urlparse

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_REPORT = REPO_ROOT / "docs" / "upstream-watch.md"
NU_STATE_FILE = REPO_ROOT / ".github" / "latest-nu-version.json"
GITHUB_API = "https://api.github.com"
USER_AGENT = "numan-upstream-watcher/1.0"
HTTP_SCHEMES = frozenset({"https", "http"})
WATCH_OFF_TAG = "watch-off"
RELEASES_PER_PAGE = 30


def ensure_http_url(url: str) -> None:
    """Raise ValueError unless *url* is an http(s) URL with a host."""
    if not isinstance(url, str):
        raise TypeError(f"URL must be a string, got {type(url).__name__}")
    parsed = urlparse(url)
    try:
        has_host = bool(parsed.hostname)
    except ValueError:
        has_host = False
    if parsed.scheme.lower() not in HTTP_SCHEMES or not has_host:
        raise ValueError(f"URL must use http(s), got {url!r}")


class _HttpOnlyRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Reject any redirect whose target fails the http(s) scheme guard."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        ensure_http_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def http_opener() -> urllib.request.OpenerDirector:
    return urllib.request.build_opener(_HttpOnlyRedirectHandler())


def parse_github_repo(repo_url: str) -> tuple[str, str] | None:
    """Return (owner, repo) for a github.com URL, else None."""
    if not isinstance(repo_url, str):
        return None
    parsed = urlparse(repo_url.strip())
    if (parsed.hostname or "").lower() not in ("github.com", "www.github.com"):
        return None
    parts = [p for p in parsed.path.split("/") if p]
    if len(parts) < 2:
        return None
    owner, repo = parts[0], parts[1].removesuffix(".git")
    if not owner or not repo:
        return None
    return owner, repo


class SemverTuple(tuple):
    """SemVer 2.0.0-aware 3-tuple that preserves prerelease precedence."""

    def __new__(
        cls,
        major: int,
        minor: int,
        patch: int,
        prerelease: str | None = None,
    ) -> Self:
        inst = super().__new__(cls, (major, minor, patch))
        inst.major = major
        inst.minor = minor
        inst.patch = patch
        inst.prerelease = prerelease
        return inst

    def _key(self) -> tuple[tuple[int, int, int], tuple[int, tuple]]:
        core = (self.major, self.minor, self.patch)
        if self.prerelease is None:
            return (core, (1, ()))
        parts: list[tuple[int, int | str]] = []
        for p in self.prerelease.split("."):
            parts.append((0, int(p)) if p.isdigit() else (1, p))
        return (core, (0, tuple(parts)))

    def __lt__(self, other: object) -> bool:
        if isinstance(other, SemverTuple):
            return self._key() < other._key()
        if isinstance(other, tuple) and len(other) == 3:
            return self._key() < (other, (1, ()))
        return NotImplemented

    def __gt__(self, other: object) -> bool:
        if isinstance(other, SemverTuple):
            return self._key() > other._key()
        if isinstance(other, tuple) and len(other) == 3:
            return self._key() > (other, (1, ()))
        return NotImplemented

    def __eq__(self, other: object) -> bool:
        if isinstance(other, SemverTuple):
            return self._key() == other._key()
        if isinstance(other, tuple) and len(other) == 3:
            return self._key() == (other, (1, ()))
        return NotImplemented

    def __ge__(self, other: object) -> bool:
        lt = self.__lt__(other)
        if lt is NotImplemented:
            return NotImplemented
        return not lt

    def __le__(self, other: object) -> bool:
        gt = self.__gt__(other)
        if gt is NotImplemented:
            return NotImplemented
        return not gt


def parse_semver_tuple(v_str: str) -> SemverTuple:
    """Parse version string into a SemVer-aware tuple (major, minor, patch)."""
    tokens = str(v_str).lstrip("vV").split()
    if not tokens:
        return SemverTuple(0, 0, 0)
    token = tokens[0].split("+")[0]
    if "-" in token:
        clean, prerelease = token.split("-", 1)
    else:
        clean = token
        prerelease = None
    parts = clean.split(".")
    try:
        nums = [int(p) for p in parts[:3]]
    except ValueError:
        return SemverTuple(0, 0, 0, prerelease=prerelease)
    while len(nums) < 3:
        nums.append(0)
    return SemverTuple(nums[0], nums[1], nums[2], prerelease=prerelease)


def is_newer(upstream_tag: str, indexed_version: str) -> bool:
    return parse_semver_tuple(upstream_tag) > parse_semver_tuple(indexed_version)


_CMP = {
    ">=": lambda a, b: a >= b,
    "<=": lambda a, b: a <= b,
    ">": lambda a, b: a > b,
    "<": lambda a, b: a < b,
    "==": lambda a, b: a == b,
    "=": lambda a, b: a == b,
}


def nu_satisfies(constraint: str, nu_version: str) -> bool:
    """Check Nu version against a simple '>=x <y' style constraint. '*' always true."""
    c = (constraint or "*").strip()
    if c in ("", "*"):
        return True
    target = parse_semver_tuple(nu_version)
    for token in c.replace(",", " ").split():
        m_wildcard = re.fullmatch(r"=?(\d+)\.(\d+)\.[xX*]", token)
        if m_wildcard:
            major, minor = int(m_wildcard.group(1)), int(m_wildcard.group(2))
            if (target[0], target[1]) != (major, minor):
                return False
            continue
        m = re.fullmatch(r"(>=|<=|>|<|==|=)?(.+)", token)
        if not m:
            return False
        op, ver = m.group(1) or "==", m.group(2)
        if re.search(r"[xX*]", ver):
            return False
        if not _CMP[op](target, parse_semver_tuple(ver)):
            return False
    return True


def _github_headers() -> dict[str, str]:
    headers = {"User-Agent": USER_AGENT, "Accept": "application/vnd.github.v3+json"}
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return headers


def fetch_latest_release(owner: str, repo: str) -> dict | None:
    """Fetch the highest stable (non-draft, non-prerelease) release, or None."""
    base = f"{GITHUB_API}/repos/{owner}/{repo}/releases"
    opener = http_opener()
    stable: list[dict] = []
    page = 1
    while True:
        url = f"{base}?{urlencode({'per_page': RELEASES_PER_PAGE, 'page': page})}"
        ensure_http_url(url)
        req = urllib.request.Request(url, headers=_github_headers())
        with opener.open(req, timeout=30) as resp:
            releases = json.loads(resp.read().decode("utf-8"))
        if not isinstance(releases, list):
            return None
        stable.extend(
            rel
            for rel in releases
            if isinstance(rel, dict) and not rel.get("draft") and not rel.get("prerelease")
        )
        if len(releases) < RELEASES_PER_PAGE:
            break
        page += 1
    if not stable:
        return None
    return max(stable, key=lambda r: parse_semver_tuple(str(r.get("tag_name", "0.0.0"))))


def fetch_json_url(url: str) -> dict:
    ensure_http_url(url)
    headers = {"User-Agent": USER_AGENT, "Accept": "application/vnd.github.v3+json"}
    parsed = urlparse(url)
    if (parsed.hostname or "").lower() == "api.github.com":
        token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
        if token:
            headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(url, headers=headers)
    with http_opener().open(req, timeout=30) as resp:
        return json.loads(resp.read().decode("utf-8"))


def load_index(index_arg: str) -> dict:
    """Load registry index from a local path or an http(s) URL."""
    if index_arg.startswith(("http://", "https://")):
        return fetch_json_url(index_arg)
    path = Path(index_arg)
    return json.loads(path.read_text(encoding="utf-8"))


def resolve_target_nu(explicit: str | None) -> str:
    if explicit:
        return explicit.lstrip("vV")
    try:
        state = json.loads(NU_STATE_FILE.read_text(encoding="utf-8"))
        if not isinstance(state, dict):
            raise TypeError("expected a JSON object")
        raw = state.get("latest_version")
        if not isinstance(raw, str) or not raw.strip():
            raise ValueError("missing latest_version")
        return raw.strip().lstrip("vV")
    except (OSError, ValueError, TypeError) as exc:
        raise RuntimeError(
            f"Cannot determine target Nu version: {NU_STATE_FILE} is missing or invalid: {exc}. "
            "Pass --nu-version explicitly."
        ) from exc


def latest_indexed_version(package: dict) -> dict | None:
    versions = [v for v in package.get("versions", []) if isinstance(v, dict)]
    if not versions:
        return None
    return max(versions, key=lambda v: parse_semver_tuple(str(v.get("version", "0.0.0"))))


def detect_gaps(packages: list[dict], upstream: dict[str, dict], target_nu: str) -> dict:
    """Type A: new upstream tag. Type B: indexed version excludes target Nu."""
    upstream_updates: list[dict] = []
    nu_gaps: list[dict] = []
    for pkg in packages:
        tags = pkg.get("tags") or []
        if WATCH_OFF_TAG in tags:
            continue
        pid = pkg.get("id") or {}
        scoped = f"{pid.get('owner', '?')}/{pid.get('name', '?')}"
        latest = latest_indexed_version(pkg)
        if latest is None:
            continue
        indexed = str(latest.get("version", "0.0.0"))
        rel = upstream.get(scoped)
        if rel and rel.get("tag_name") and is_newer(str(rel["tag_name"]), indexed):
            upstream_updates.append(
                {
                    "package": scoped,
                    "indexed": indexed,
                    "upstream": str(rel["tag_name"]),
                    "url": str(rel.get("html_url", "")),
                    "type": str(pkg.get("type", "")),
                }
            )
        constraint = str(latest.get("nu_version", "*"))
        verified = [str(v) for v in (latest.get("verified_with") or [])]
        if not nu_satisfies(constraint, target_nu) and target_nu not in verified:
            nu_gaps.append(
                {
                    "package": scoped,
                    "indexed": indexed,
                    "constraint": constraint,
                    "target_nu": target_nu,
                    "type": str(pkg.get("type", "")),
                }
            )
    upstream_updates.sort(key=lambda r: r["package"])
    nu_gaps.sort(key=lambda r: r["package"])
    return {"upstream_updates": upstream_updates, "nu_gaps": nu_gaps}


def format_report_markdown(gaps: dict, target_nu: str) -> str:
    """Human report. Promotion checklist mirrors the registry intake loop."""
    checked = datetime.now(timezone.utc).isoformat()
    lines = [
        "# Upstream & Nu Watch Report",
        "",
        (
            f"Checked at {checked} against Nushell {target_nu}. "
            "Detection only: nothing here is published automatically."
        ),
        "",
        "## New upstream releases (Type A)",
        "",
    ]
    updates = gaps.get("upstream_updates", [])
    if not updates:
        lines.append("None.")
    for u in updates:
        lines.append(
            f"- `{u['package']}` [{u['type']}] indexed {u['indexed']} "
            f"-> upstream {u['upstream']} ({u['url']})"
        )
    lines += ["", f"## Nu {target_nu} gaps (Type B)", ""]
    ng = gaps.get("nu_gaps", [])
    if not ng:
        lines.append("None.")
    for g in ng:
        lines.append(
            f"- `{g['package']}` [{g['type']}] v{g['indexed']} "
            f"needs Nu {g['constraint']} (target {g['target_nu']})"
        )
    errors = gaps.get("lookup_errors", [])
    if errors:
        lines += ["", "## Lookup errors", ""]
        for e in errors:
            lines.append(f"- `{e['package']}`: release lookup failed: {e.get('error', 'unknown error')}")
    lines += [
        "",
        "## Promotion checklist (per package, in numan-registry)",
        "",
        "- [ ] Draft candidate spec with provenance (no hand-typed hashes)",
        "- [ ] Run validate + parser-check + manifest lint",
        "- [ ] Run lifecycle-prove on the target Nu; attach evidence",
        "- [ ] Append verified_with only if lifecycle-prove passed",
        "- [ ] Credential-gated: mark evidence_tier provisional with deferral reason",
        "- [ ] Open draft PR; production publish needs reviewer approval",
        "",
    ]
    return "\n".join(lines)


def write_outputs(gaps: dict, target_nu: str) -> None:
    has = bool(gaps.get("upstream_updates") or gaps.get("nu_gaps"))
    sanitized_target = target_nu.replace("\r", "").replace("\n", "")
    gh_output = os.environ.get("GITHUB_OUTPUT")
    if gh_output:
        with open(gh_output, "a", encoding="utf-8") as f:
            f.write(f"has_updates={'true' if has else 'false'}\n")
            f.write(f"upstream_count={len(gaps.get('upstream_updates', []))}\n")
            f.write(f"nu_gap_count={len(gaps.get('nu_gaps', []))}\n")
            f.write(f"error_count={len(gaps.get('lookup_errors', []))}\n")
            f.write(f"target_nu={sanitized_target}\n")
    gh_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if gh_summary:
        with open(gh_summary, "a", encoding="utf-8") as f:
            f.write(format_report_markdown(gaps, sanitized_target))
            f.write("\n")


def process(
    index_path: str,
    target_nu: str,
    write: bool,
    report_path: Path = DEFAULT_REPORT,
    package_filter: list[str] | None = None,
) -> int:
    index = load_index(index_path)
    packages = [p for p in index.get("packages", []) if isinstance(p, dict)]
    if package_filter:
        wanted = set(package_filter)
        packages = [
            p
            for p in packages
            if f"{(p.get('id') or {}).get('owner', '')}/{(p.get('id') or {}).get('name', '')}"
            in wanted
        ]
    upstream: dict[str, dict] = {}
    lookup_errors: list[dict] = []
    for pkg in packages:
        if WATCH_OFF_TAG in (pkg.get("tags") or []):
            continue
        gh = parse_github_repo(str(pkg.get("repo", "")))
        if gh is None:
            continue
        pid = pkg.get("id") or {}
        scoped = f"{pid.get('owner', '?')}/{pid.get('name', '?')}"
        try:
            rel = fetch_latest_release(gh[0], gh[1])
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError, OSError, ValueError, RuntimeError) as exc:
            print(f"warning: {scoped}: release lookup failed: {exc}", file=sys.stderr)
            lookup_errors.append({"package": scoped, "repo": str(pkg.get("repo", "")), "error": str(exc)})
            continue
        if rel is not None:
            upstream[scoped] = rel
    gaps = detect_gaps(packages, upstream, target_nu)
    if lookup_errors:
        gaps["lookup_errors"] = lookup_errors
    report = format_report_markdown(gaps, target_nu)
    print(report)
    if not write:
        print("[dry-run] report not written. Pass --write to save it.")
        write_outputs(gaps, target_nu)
        return 0
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(report, encoding="utf-8")
    print(f"Updated {report_path}")
    write_outputs(gaps, target_nu)
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Watch indexed packages for updates (detection only).")
    p.add_argument("--index", default="https://numan-cli.github.io/numan-registry/index.json",
                   help="Registry index.json path or URL.")
    p.add_argument("--nu-version", default=None, help="Target Nu version (default: latest-nu-version.json).")
    p.add_argument("--dry-run", action="store_true", help="Print report without writing (default).")
    p.add_argument("--write", action="store_true", help="Write report file.")
    p.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    p.add_argument("--packages", default=None, help="Comma-separated owner/name filter.")
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        target_nu = resolve_target_nu(args.nu_version)
        filt = [s.strip() for s in args.packages.split(",")] if args.packages else None
        return process(
            index_path=args.index,
            target_nu=target_nu,
            write=bool(args.write and not args.dry_run),
            report_path=args.report,
            package_filter=filt,
        )
    except (OSError, ValueError, RuntimeError) as exc:
        print(f"Error watching upstreams: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
