#!/usr/bin/env python3
# ---
# relationships:
#   implements: github-release-executor
#   verifies: github-action-pins
# ---
"""Resolve each declared GitHub Action tag and verify its pinned commit.

The declaration to read defaults to this repository's. A caller may name
another so this seam can be held to declarations that are deliberately
malformed, because a check only ever run against a well-formed file cannot
distinguish "every entry was read" from "every entry that was read agreed".
"""

from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
DECLARATION = ROOT / "github-action-pins.yml"
FIELD = re.compile(r"^    (repository|tag|commit): (\S+)$")


def declarations(text):
    current = {}
    for line in text.splitlines():
        match = FIELD.match(line)
        if not match:
            continue
        field, value = match.groups()
        current[field] = value
        if current.keys() >= {"repository", "tag", "commit"}:
            yield current
            current = {}


def resolve(repository, tag):
    reference = f"refs/tags/{tag}"
    result = subprocess.run(
        ["git", "ls-remote", "--tags", f"https://github.com/{repository}.git", reference, f"{reference}^{{}}"],
        check=True,
        capture_output=True,
        text=True,
    )
    resolved = {
        name: commit for commit, name in (line.split() for line in result.stdout.splitlines())
    }
    return resolved.get(f"{reference}^{{}}", resolved.get(reference))


def main(argv):
    declaration = Path(argv[1]) if len(argv) > 1 else DECLARATION
    text = declaration.read_text(encoding="utf-8")
    pins = list(declarations(text))
    if not pins:
        raise SystemExit("expected at least one Action declaration")
    disagreements = []
    for pin in pins:
        actual = resolve(pin["repository"], pin["tag"])
        if actual != pin["commit"]:
            disagreements.append(
                f'{pin["repository"]}@{pin["tag"]}: declared {pin["commit"]}, resolved {actual}'
            )
    if disagreements:
        print("GitHub Action tag resolution disagrees with declared pins:", file=sys.stderr)
        print("\n".join(disagreements), file=sys.stderr)
        return 1
    print(f"Verified {len(pins)} GitHub Action tags against their declared commits.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
