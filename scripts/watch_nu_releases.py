#!/usr/bin/env python3
"""Watch official Nushell releases on GitHub, dynamically extract changelogs,
and maintain docs/nushell-releases.md and .github/latest-nu-version.json.

Usage:
  python scripts/watch_nu_releases.py [--dry-run] [--write]
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

REPO_ROOT = Path(__file__).resolve().parent.parent
STATE_FILE = REPO_ROOT / ".github" / "latest-nu-version.json"
RELEASES_MD = REPO_ROOT / "docs" / "nushell-releases.md"
NU_RELEASES_API = "https://api.github.com/repos/nushell/nushell/releases/latest"
USER_AGENT = "numan-release-watcher/1.0"


def fetch_latest_release() -> dict:
    req = urllib.request.Request(
        NU_RELEASES_API,
        headers={
            "User-Agent": USER_AGENT,
            "Accept": "application/vnd.github.v3+json",
        },
    )
    # Check for GITHUB_TOKEN if available
    gh_token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if gh_token:
        req.add_header("Authorization", f"Bearer {gh_token}")

    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode("utf-8"))


def parse_version_tuple(v_str: str) -> tuple[int, int, int]:
    clean = v_str.lstrip("v").split()[0]
    parts = clean.split(".")
    try:
        return (int(parts[0]), int(parts[1]), int(parts[2]) if len(parts) > 2 else 0)
    except (ValueError, IndexError):
        return (0, 0, 0)


def extract_blog_url(body: str) -> str | None:
    # Look for https://www.nushell.sh/blog/... or nushell.sh/blog/...
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
    blog_link_text = f"[{blog_url}]({blog_url})" if blog_url else f"https://www.nushell.sh/blog/ (see release notes)"
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


def main() -> int:
    parser = argparse.ArgumentParser(description="Watch official Nushell releases.")
    parser.add_argument("--dry-run", action="store_true", help="Print actions without modifying files.")
    parser.add_argument("--write", action="store_true", help="Write changes to state file and releases doc.")
    parser.add_argument("--force", action="store_true", help="Force regenerate release entry even if version unchanged.")
    args = parser.parse_args()

    print("Fetching latest Nushell release from GitHub...")
    try:
        release = fetch_latest_release()
    except Exception as e:
        print(f"Error fetching release: {e}", file=sys.stderr)
        return 1

    tag_name = release.get("tag_name", "").lstrip("v").strip()
    github_url = release.get("html_url", f"https://github.com/nushell/nushell/releases/tag/{tag_name}")
    published_at = release.get("published_at", datetime.now(timezone.utc).isoformat())
    date_str = published_at[:10] if published_at else datetime.now(timezone.utc).strftime("%Y-%m-%d")
    body = release.get("body", "")

    blog_url = extract_blog_url(body)
    major, minor, patch = parse_version_tuple(tag_name)

    print(f"Detected latest release: {tag_name} (released {date_str})")
    print(f"Extracted blog URL: {blog_url or 'None found'}")

    current_state = {}
    if STATE_FILE.exists():
        try:
            current_state = json.loads(STATE_FILE.read_text(encoding="utf-8"))
        except Exception:
            current_state = {}

    tracked_version = current_state.get("latest_version")
    is_new = tracked_version != tag_name

    print(f"Tracked version in state: {tracked_version or 'None'}")
    print(f"New release detected: {is_new}")

    if not is_new and not args.force:
        print("No new version detected. Everything up to date.")
        # Set GitHub Actions output if in CI
        gh_output = os.environ.get("GITHUB_OUTPUT")
        if gh_output:
            with open(gh_output, "a", encoding="utf-8") as f:
                f.write("is_new=false\n")
                f.write(f"version={tag_name}\n")
        return 0

    new_entry = generate_release_entry(tag_name, date_str, github_url, blog_url, major, minor)

    if args.dry_run or not args.write:
        print("\n--- Proposed new release entry ---")
        print(new_entry)
        print(f"[dry-run] Would update {STATE_FILE} to latest_version: {tag_name}")
        return 0

    # Update STATE_FILE
    new_state = {
        "latest_version": tag_name,
        "checked_at": datetime.now(timezone.utc).isoformat(),
        "published_at": published_at,
        "github_url": github_url,
        "blog_url": blog_url,
    }
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    STATE_FILE.write_text(json.dumps(new_state, indent=2) + "\n", encoding="utf-8")
    print(f"Updated {STATE_FILE}")

    # Update RELEASES_MD
    existing_content = ""
    if RELEASES_MD.exists():
        existing_content = RELEASES_MD.read_text(encoding="utf-8")

    header = "# Official Nushell Release & Stewardship Log\n\nChronological ledger of official Nushell releases, changelogs, ABI bands, and ecosystem impact checklists.\n\n---\n\n"
    if not existing_content.startswith("# Official Nushell Release & Stewardship Log"):
        updated_content = header + new_entry + existing_content
    else:
        # Check if version section already present
        if f"## Nushell {tag_name}" not in existing_content:
            parts = existing_content.split("---\n\n", 1)
            if len(parts) == 2:
                updated_content = parts[0] + "---\n\n" + new_entry + parts[1]
            else:
                updated_content = existing_content + "\n" + new_entry
        else:
            updated_content = existing_content

    RELEASES_MD.parent.mkdir(parents=True, exist_ok=True)
    RELEASES_MD.write_text(updated_content, encoding="utf-8")
    print(f"Updated {RELEASES_MD}")

    # Set GitHub Actions output & summary if in CI
    gh_output = os.environ.get("GITHUB_OUTPUT")
    if gh_output:
        with open(gh_output, "a", encoding="utf-8") as f:
            f.write("is_new=true\n")
            f.write(f"version={tag_name}\n")
            f.write(f"blog_url={blog_url or ''}\n")
            f.write(f"github_url={github_url}\n")

    gh_summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if gh_summary:
        with open(gh_summary, "a", encoding="utf-8") as f:
            f.write(f"### 🚀 New Nushell Release Detected: {tag_name}\n\n")
            f.write(f"- **Release Date**: {date_str}\n")
            f.write(f"- **GitHub Release**: [{github_url}]({github_url})\n")
            if blog_url:
                f.write(f"- **Blog Post**: [{blog_url}]({blog_url})\n")
            f.write("\n" + new_entry + "\n")

    return 0


if __name__ == "__main__":
    sys.exit(main())
