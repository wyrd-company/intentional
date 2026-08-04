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
import os
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
DECLARATION = ROOT / "github-action-pins.yml"
FIELD = re.compile(r"^    (repository|tag|commit): (\S+)$")
ENTRY = re.compile(r"^  - constant: (\S+)$", re.MULTILINE)

#: Attempts one repository's resolution is given before the run fails.
#:
#: The same bounded-attempt shape the publication scripts use. Nine
#: repositories are resolved per run, and each is bounded independently, so one
#: unreachable repository cannot make the other eight resolve again.
ATTEMPTS = 18

#: Seconds between attempts. The workflow does not set the override; it exists
#: so the ceiling above can be exercised without waiting it out.
DELAY_SECONDS = float(os.environ.get("INTENTIONAL_ACTION_PIN_RETRY_DELAY", "10"))


def declared_constants(text):
    """Every entry the declaration opens, counted straight off the raw text.

    This never consults the field parse, the entries it yielded, or anything
    the field pattern produced. That independence is the whole point: an
    equality between this count and the parsed count fails when the parser
    skips an entry, and it is the only shape that does. A floor -- including
    non-emptiness -- is satisfied by any partial sweep, which is how a run that
    read eight of nine entries reported success.
    """
    return ENTRY.findall(text)


def unreadable_constants(text):
    """The entries whose field block the parser could not read completely.

    Diagnosis only. The equality above decides whether the run fails; this
    decides what the failure is allowed to say, so an operator is told which
    entry to look at rather than only that a count was wrong.
    """
    unreadable = []
    for block in re.split(r"^(?=  - constant: )", text, flags=re.MULTILINE):
        header = ENTRY.search(block)
        if not header:
            continue
        fields = {
            match.group(1) for match in map(FIELD.match, block.splitlines()) if match
        }
        if fields != {"repository", "tag", "commit"}:
            unreadable.append(header.group(1))
    return unreadable


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
    """Resolve one tag upstream, retrying only a failed resolution.

    Retry covers process failure -- a rate limit, a DNS failure, a transient
    GitHub outage -- because this workflow runs on every push and a blip would
    otherwise redden a required check on an unrelated commit.

    Retry stops at the first attempt that completes, whatever it resolved. A
    resolved commit that disagrees with the declared pin is this seam's only
    real finding, and it is a resolution rather than a failure, so it is
    returned here and judged by the caller. Re-running it would convert the
    finding into silence.
    """
    reference = f"refs/tags/{tag}"
    command = [
        "git",
        "ls-remote",
        "--tags",
        f"https://github.com/{repository}.git",
        reference,
        f"{reference}^{{}}",
    ]
    for attempt in range(1, ATTEMPTS + 1):
        result = subprocess.run(command, check=False, capture_output=True, text=True)
        if result.returncode == 0:
            resolved = {
                name: commit
                for commit, name in (
                    line.split() for line in result.stdout.splitlines()
                )
            }
            return resolved.get(f"{reference}^{{}}", resolved.get(reference))
        if attempt == ATTEMPTS:
            raise SystemExit(
                f"resolving {repository}@{tag} failed on all {ATTEMPTS} attempts: "
                f"{result.stderr.strip()}"
            )
        time.sleep(DELAY_SECONDS)


def main(argv):
    declaration = Path(argv[1]) if len(argv) > 1 else DECLARATION
    text = declaration.read_text(encoding="utf-8")
    pins = list(declarations(text))
    constants = declared_constants(text)
    if not constants:
        raise SystemExit("expected at least one Action declaration")
    # An equality, never a floor. The two counts come from the same text by
    # different routes, so a parse that skipped an entry disagrees with the
    # text that declared it.
    if len(pins) != len(constants):
        unreadable = unreadable_constants(text)
        raise SystemExit(
            f"read {len(pins)} of the {len(constants)} declared Action entries; "
            f"could not read: {', '.join(unreadable) or 'unidentified entries'}"
        )
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
