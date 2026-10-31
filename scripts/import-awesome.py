#!/usr/bin/env python3
"""Convert the awesome-terminal and awesome-tui lists into a catalog draft.

Part11 §11.2.1's conversion script. Run on demand:

    python scripts/import-awesome.py > catalog-draft.toml

It fetches the two lists, pulls each linked repository's license from the
GitHub API (best effort; the API rate-limits anonymous requests, so a run
may leave licenses blank), and emits `[[app]]` entries in the seed
catalog's format. THE OUTPUT IS A DRAFT: category, exec, icon and the
install recipes are always empty/`app`, and every line is meant to be
reviewed and filled by hand before anything joins catalog/terminal-apps.toml.
License strings the API could not resolve are left out rather than guessed.
"""

import json
import re
import sys
import urllib.request

LISTS = [
    # cdleon/awesome-terminals — terminal emulators and multiplexers.
    "https://raw.githubusercontent.com/cdleon/awesome-terminals/master/README.md",
    # six-ddc/awesome-tui — terminal user interfaces of every kind.
    "https://raw.githubusercontent.com/six-ddc/awesome-tui/master/README.md",
]

UA = {"User-Agent": "agony-import-awesome/1.0"}
REPO_RE = re.compile(r"https://github\.com/([\w.-]+)/([\w.-]+)")
ENTRY_RE = re.compile(r"\[([^\]]+)\]\((https://[^\s)]+)\)\s*[-—:]?\s*(.*)")


def fetch(url: str) -> str:
    request = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read().decode("utf-8", errors="replace")


def github_license(owner: str, repo: str) -> str:
    """The repo's SPDX id, or an empty string when the API says nothing."""
    try:
        payload = json.loads(fetch(f"https://api.github.com/repos/{owner}/{repo}/license"))
        spdx = payload.get("license", {}).get("spdx_id", "")
        return "" if spdx in ("", "NOASSERTION", "OTHER") else spdx
    except Exception:
        return ""


def entries(markdown: str):
    for line in markdown.splitlines():
        match = ENTRY_RE.search(line)
        if not match:
            continue
        name, url, description = (group.strip() for group in match.groups())
        if not name or url.startswith("https://github.com/awesome-lists"):
            continue
        yield name, url, description.replace("|", "/")


def main() -> int:
    seen = set()
    print(
        "# Draft catalog entries from the awesome lists — REVIEW BY HAND.\n"
        "# Fill category (files|shell|text|dev|game|net|system), exec, icon,\n"
        "# and install recipes; drop entries you do not want. Then merge the\n"
        "# survivors into catalog/terminal-apps.toml.\n"
    )
    for list_url in LISTS:
        try:
            markdown = fetch(list_url)
        except Exception as error:  # noqa: BLE001 - a draft tool reports and goes on
            print(f"# fetch failed: {list_url}: {error}", file=sys.stderr)
            continue
        for name, url, description in entries(markdown):
            key = url.lower()
            if key in seen:
                continue
            seen.add(key)
            repo = REPO_RE.match(url)
            license_id = github_license(*repo.groups()) if repo else ""
            print("[[app]]")
            print(f'id = "{name.lower().replace(" ", "-")}"')
            print(f'name = "{name}"')
            print(f'description = "{description}"')
            print('exec = ""')
            print('category = ""')
            print('icon = "app"')
            print(f"source = \"{url}\"")
            if license_id:
                print(f'license = "{license_id}"')
            print("install = { }")
            print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
