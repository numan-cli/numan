#!/usr/bin/env python3
"""Watch official Nushell releases on GitHub, dynamically extract changelogs,
and maintain docs/nushell-releases.md and .github/latest-nu-version.json.

Usage:
  python scripts/watch_nu_releases.py [--dry-run] [--write] [--force]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse

REPO_ROOT = Path(__file__).resolve().parent.parent
STATE_FILE = REPO_ROOT / ".github" / "latest-nu-version.json"
RELEASES_MD = REPO_ROOT / "docs" / "nushell-releases.md"
NU_RELEASES_API = "https://api.github.com/repos/nushell/nushell/releases/latest"
USER_AGENT = "numan-release-watcher/1.0"
HTTP_SCHEMES = frozenset({"https", "http"})


def ensure_http_url(url: str) -> None:
    """Raise ValueError unless *url* is an http(s) URL with a host."""
    if not isinstance(url, str):
        raise ValueError(f"URL must use http(s), got {url!r}")
    parsed = urlparse(url)
    try:
        has_host = parsed.hostname is not None
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
    """Return an opener whose redirects are also constrained to http(s)."""
    return urllib.request.build_opener(_HttpOnlyRedirectHandler())


def fetch_latest_release(url: str = NU_RELEASES_API) -> dict:
    """Fetch latest release metadata from GitHub API with scheme safety."""
    ensure_http_url(url)
    req = urllib.request.Request(
        url,
        headers={
            "User-Agent": USER_AGENT,
            "Accept": "application/vnd.github.v3+json",
        },
    )
    gh_token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if gh_token:
        req.add_header("Authorization", f"Bearer {gh_token}")

    with http_opener().open(req, timeout=30) as resp:
        return json.loads(resp.read().decode("utf-8"))


def parse_version_tuple(v_str: str) -> tuple[int, int, int]:
    """Parse version string into (major, minor, patch) integer tuple."""
    clean = v_str.lstrip("v").split()[0].split("-")[0].split("+")[0]
    parts = clean.split(".")
    try:
        return (int(parts[0]), int(parts[1]), int(parts[2]) if len(parts) > 2 else 0)
    except (ValueError, IndexError):
        return (0, 0, 0)


def extract_blog_url(body: str) -> str | None:
    """Extract official Nushell blog post URL from release body text."""
    match = re.search(r"https?://(?:www\.)?nushell\.sh/blog/([a-zA-Z0-9_\-\.]+)(?:\.html)?", body)
    if match:
        url = match.group(0)
        if not url.endswith(".html"):
            url += ".html"
        return url
    return None


def generate_release_entry(
    version: str,
    date_str: str,
    github_url: str,
    blog_url: str | None,
    major: int,
    minor: int,
) -> str:
    """Generate Markdown entry for the stewardship releases log."""
    blog_link_text = f"[{blog_url}]({blog_url})" if blog_url else "https://www.nushell.sh/blog/ (see release notes)"
    next_minor = minor + 1
    abi_constraint = f">={major}.{minor}.0 <{major}.{next_minor}.0"

    return f"""## Nushell {version} ({date_str})

- **Release Notes**: [{github_url}]({github_url})
- **Official Blog & Detailed Changelog**: {blog_link_text}
- **Plugin Protocol / ABI Band**: `nu-plugin` `{major}.{minor}.x` (`nu_version: "{abi_constraint}"`)

### Stewardship & Ecosystem Checklist
- [ ] Review breaking engine/protocol changes affecting plugins
- [ ] Audit `numan-maintained/*` fork dependencies (`Cargo.toml`)
- [ ] Update `manifest.json` active plugin versions in `numan-plugins`
- [ ] Rebuild & test active plugins for `{major}.{minor}.x` band via `build.yml`
- [ ] Update `numan` CI acceptance matrix (`.github/workflows/ci.yml`)

---
"""


def load_current_state(state_file: Path = STATE_FILE) -> dict:
    """Load JSON state dictionary from disk if available."""
    if not state_file.exists():
        return {}
    try:
        return json.loads(state_file.read_text(encoding="utf-8"))
    except Exception:
        return {}


def write_state(state_file: Path, new_state: dict) -> None:
    """Persist new state dictionary as formatted JSON."""
    state_file.parent.mkdir(parents=True, exist_ok=True)
    state_file.write_text(json.dumps(new_state, indent=2) + "\n", encoding="utf-8")
    print(f"Updated {state_file}")


def update_releases_markdown(releases_md: Path, new_entry: str, tag_name: str) -> bool:
    """Prepend or insert release entry into releases markdown document."""
    existing_content = ""
    if releases_md.exists():
        existing_content = releases_md.read_text(encoding="utf-8")

    if f"## Nushell {tag_name}" in existing_content:
        return False

    header = (
        "# Official Nushell Release & Stewardship Log\n\n"
        "Chronological ledger of official Nushell releases, changelogs, ABI bands, and ecosystem impact checklists.\n\n"
        "---\n\n"
    )
    if not existing_content.startswith("# Official Nushell Release & Stewardship Log"):
        updated_content = header + new_entry + existing_content
    else:
        parts = existing_content.split("---\n\n", 1)
        if len(parts) == 2:
            updated_content = parts[0] + "---\n\n" + new_entry + parts[1]
        else:
            updated_content = existing_content + "\n" + new_entry

    releases_md.parent.mkdir(parents=True, exist_ok=True)
    releases_md.write_text(updated_content, encoding="utf-8")
    print(f"Updated {releases_md}")
    return True


def write_github_actions_outputs(
    is_new: bool,
    tag_name: str,
    github_url: str = "",
    blog_url: str | None = None,
) -> None:
    """Write outputs to GITHUB_OUTPUT environment file if defined."""
    gh_output = os.environ.get("GITHUB_OUTPUT")
    if not gh_output:
        return
    with open(gh_output, "a", encoding="utf-8") as f:
        f.write(f"is_new={'true' if is_new else 'false'}\n")
        f.write(f"version={tag_name}\n")
        if is_new:
            f.write(f"blog_url={blog_url or ''}\n")
            f.write(f"github_url={github_url}\n")


def write_github_step_summary(
    tag_name: str,
    date_str: str,
    github_url: str,
    blog_url: str | None,
    new_entry: str,
) -> None:
    """Append release summary to GITHUB_STEP_SUMMARY environment file."""
    gh_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if not gh_summary:
        return
    with open(gh_summary, "a", encoding="utf-8") as f:
        f.write(f"### 🚀 New Nushell Release Detected: {tag_name}\n\n")
        f.write(f"- **Release Date**: {date_str}\n")
        f.write(f"- **GitHub Release**: [{github_url}]({github_url})\n")
        if blog_url:
            f.write(f"- **Blog Post**: [{blog_url}]({blog_url})\n")
        f.write("\n" + new_entry + "\n")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parse command line arguments."""
    parser = argparse.ArgumentParser(description="Watch official Nushell releases.")
    parser.add_argument("--dry-run", action="store_true", help="Print actions without modifying files.")
    parser.add_argument("--write", action="store_true", help="Write changes to state file and releases doc.")
    parser.add_argument("--force", action="store_true", help="Force regenerate release entry even if version unchanged.")
    return parser.parse_args(argv)


def process_release(
    release: dict,
    write: bool,
    dry_run: bool,
    force: bool,
    state_file: Path = STATE_FILE,
    releases_md: Path = RELEASES_MD,
) -> int:
    """Process fetched release against tracked state and update files/outputs."""
    tag_name = release.get("tag_name", "").lstrip("v").strip()
    github_url = release.get("html_url", f"https://github.com/nushell/nushell/releases/tag/{tag_name}")
    published_at = release.get("published_at", datetime.now(timezone.utc).isoformat())
    date_str = published_at[:10] if published_at else datetime.now(timezone.utc).strftime("%Y-%m-%d")
    body = release.get("body", "")

    blog_url = extract_blog_url(body)
    major, minor, _patch = parse_version_tuple(tag_name)

    print(f"Detected latest release: {tag_name} (released {date_str})")
    print(f"Extracted blog URL: {blog_url or 'None found'}")

    current_state = load_current_state(state_file)
    tracked_version = current_state.get("latest_version")
    is_new = tracked_version != tag_name

    print(f"Tracked version in state: {tracked_version or 'None'}")
    print(f"New release detected: {is_new}")

    if not is_new and not force:
        print("No new version detected. Everything up to date.")
        write_github_actions_outputs(is_new=False, tag_name=tag_name)
        return 0

    new_entry = generate_release_entry(tag_name, date_str, github_url, blog_url, major, minor)

    if dry_run or not write:
        print("\n--- Proposed new release entry ---")
        print(new_entry)
        print(f"[dry-run] Would update {state_file} to latest_version: {tag_name}")
        return 0

    new_state = {
        "latest_version": tag_name,
        "checked_at": datetime.now(timezone.utc).isoformat(),
        "published_at": published_at,
        "github_url": github_url,
        "blog_url": blog_url,
    }
    write_state(state_file, new_state)
    update_releases_markdown(releases_md, new_entry, tag_name)
    write_github_actions_outputs(is_new=True, tag_name=tag_name, github_url=github_url, blog_url=blog_url)
    write_github_step_summary(tag_name, date_str, github_url, blog_url, new_entry)
    return 0


def main(argv: list[str] | None = None) -> int:
    """CLI entry point for Nushell release watcher."""
    args = parse_args(argv)
    print("Fetching latest Nushell release from GitHub...")
    try:
        release = fetch_latest_release()
    except Exception as e:
        print(f"Error fetching release: {e}", file=sys.stderr)
        return 1

    return process_release(
        release=release,
        write=args.write,
        dry_run=args.dry_run,
        force=args.force,
    )


if __name__ == "__main__":
    sys.exit(main())

