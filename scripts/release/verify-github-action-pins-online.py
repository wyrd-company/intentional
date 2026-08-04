#!/usr/bin/env python3
# ---
# relationships:
#   implements: github-release-executor
#   verifies: github-action-pins
# ---
"""Resolve each declared GitHub Action tag and verify its pinned commit."""

from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
DECLARATION = ROOT / "github-action-pins.yml"
FIELD = re.compile(r"^    (repository|tag|commit): (\S+)$")


def declarations():
    current = {}
    for line in DECLARATION.read_text(encoding="utf-8").splitlines():
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


def main():
    pins = list(declarations())
    if len(pins) != 9:
        raise SystemExit(f"expected 9 Action declarations, found {len(pins)}")
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
    raise SystemExit(main())
