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

The declaration is read exhaustively rather than recognised selectively: the
reader classifies every line and refuses the file on the first line it cannot
classify. So the guarantee is that no line of the declaration went unread, and
it holds against any edit whatsoever, including edits that preserve the
document's YAML meaning. What it does not do is accept YAML this grammar does
not spell -- a flow mapping, a quoted scalar, an alias -- and the cost of that
is a red check asking for the declaration to be written in the one shape,
never a run that verified part of the table and reported success.

The exhaustive-read claim quantifies over documents the `serde_yaml` consumer
reads. Its measured search covered hand-built and position-fuzzed documents
accepted by PyYAML; constructs on which PyYAML and `serde_yaml` disagree are
outside that search. Within the claim's input set, closure is at line
granularity: every line is classified or refused. It does not assert that this
reader and a YAML parser assign every field value the same scalar type.

The guarantee is about lines, so it can only be as good as the agreement on
what a line is. Where readers of this file disagree about that, the file is
refused rather than read one way; see `AMBIGUOUS_BREAKS`.

The reader is deliberately stdlib-only. This script runs on a hosted runner in
`.github/workflows/github-action-pins.yml` with no dependency installation
step, so a third-party parser here would trade a completeness hole for an
`ImportError` on every push.
"""

from pathlib import Path
import os
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
DECLARATION = ROOT / "github-action-pins.yml"

#: The complete grammar of the declaration, line by line. Every line of the
#: file must match one of these, and a line matching none of them fails the
#: run. There is no second recogniser to agree with, so there is no shared
#: assumption for an edit to slip between.
IGNORED = re.compile(r"[ \t]*(#.*)?")
SEQUENCE_KEY = re.compile(r"actions:")
HEADER = re.compile(r"  - constant: ([A-Z][A-Z0-9_]*)")
FIELD = re.compile(r"    (repository|tag|commit): (\S+)")

#: What each field's value is allowed to be, spelled out rather than left as
#: "anything without a space".
#:
#: These patterns close syntax that changes the literal value this reader sees.
#: For example, `tag: "v1.0.0"` would otherwise include quotation marks in the
#: reference sent to Git. They do not close every plain scalar a YAML parser may
#: type differently. That residual is bounded by the consumers: the Rust checks
#: read only `repository` and `commit`, the slash keeps a repository scalar a
#: string, and a non-string commit fails their `as_str` expectation. This online
#: check sends a tag's exact spelling to Git and reports a resolved disagreement.
VALUES = {
    "repository": re.compile(r"[A-Za-z0-9._-]+/[A-Za-z0-9._-]+"),
    "tag": re.compile(r"[A-Za-z0-9._+-]+"),
    # A complete commit identity, matching the rule the offline Action lint
    # holds every pinned reference to. An abbreviation names whatever object
    # currently shares that prefix.
    "commit": re.compile(r"[0-9a-f]{40}"),
}

REQUIRED_FIELDS = ("repository", "tag", "commit")

#: Characters that are a line break to some readers of this file and an
#: ordinary character to others.
#:
#: Measured, not assumed: Python's ``splitlines`` ends a line on every one of
#: these; PyYAML ends a line on U+0085, U+2028 and U+2029 and rejects the rest
#: of them outright; the YAML 1.2 grammar ends a line on none of them. So a
#: declaration containing one is read as
#: different documents by different tools, and picking a side would put this
#: reader's line model back into disagreement with somebody's parser -- the
#: same class of divergence, one layer down, that made a pair of counts agree
#: about a document neither had read. It is refused instead. Nothing legitimate
#: in this declaration needs one.
AMBIGUOUS_BREAKS = "\v\f\x1c\x1d\x1e\x85\u2028\u2029"

#: Attempts one repository's resolution is given before the run fails.
#:
#: The same bounded-attempt shape the publication scripts use. Nine
#: repositories are resolved per run, and each is bounded independently, so one
#: unreachable repository cannot make the other eight resolve again.
ATTEMPTS = 18

#: Seconds between attempts. The workflow does not set the override; it exists
#: so the ceiling above can be exercised without waiting it out.
DELAY_SECONDS = float(os.environ.get("INTENTIONAL_ACTION_PIN_RETRY_DELAY", "10"))


class Unreadable(SystemExit):
    """A line of the declaration this reader will not claim to have understood.

    The entry under construction is named when there is one, because an
    operator handed a line number and a line of YAML still has to work out
    which Action stopped the run.
    """

    def __init__(self, number, line, reason, entry=None):
        within = f" (within {entry})" if entry else ""
        super().__init__(f"github-action-pins line {number}{within}: {reason}: {line!r}")


def declarations(text):
    """Read the whole declaration, or refuse naming the line that stopped it.

    Completeness here is exhaustiveness rather than agreement. Every line is
    classified by exactly one rule and an unclassified line raises, so there is
    no count to compare against a second count and no lexical assumption two
    recognisers can share. An edit that moves an entry out of the shape below
    does not become invisible; it becomes the failure.

    The text arrives newline-normalised -- `main` reads it in text mode, so a
    CRLF checkout is already ``\\n`` here -- and is split on ``\\n`` alone. A line
    carrying any other character some reader treats as a break is refused
    rather than resolved one way. Matching is whole-line, so nothing trails off
    the end of a rule unexamined.
    """
    entries = []
    current = None
    constants = set()
    sequence_seen = False
    for number, line in enumerate(text.split("\n"), start=1):
        found = [character for character in line if character in AMBIGUOUS_BREAKS]
        if found:
            raise Unreadable(
                number,
                line,
                f"this line carries {found[0]!r}, which some readers of this file "
                "treat as a line break and others do not",
                current and current["constant"],
            )
        if IGNORED.fullmatch(line):
            continue
        if SEQUENCE_KEY.fullmatch(line):
            if sequence_seen:
                raise Unreadable(number, line, "the entry sequence reopens")
            sequence_seen = True
            continue
        header = HEADER.fullmatch(line)
        if header:
            current = finish(current, entries)
            constant = header.group(1)
            if constant in constants:
                raise Unreadable(number, line, "this entry is declared twice")
            constants.add(constant)
            current = {"constant": constant, "line": number}
            continue
        field = FIELD.fullmatch(line)
        if field:
            if current is None:
                raise Unreadable(number, line, "this field opens no entry")
            name, value = field.groups()
            if name in current:
                raise Unreadable(number, line, f"{name} is declared twice")
            if not VALUES[name].fullmatch(value):
                raise Unreadable(
                    number,
                    line,
                    f"this is not a {name} this reader recognises",
                    current["constant"],
                )
            current[name] = value
            continue
        raise Unreadable(
            number,
            line,
            "this line is not a declaration this reader knows",
            current and current["constant"],
        )
    finish(current, entries)
    if not sequence_seen:
        raise SystemExit("the declaration carries no actions sequence key")
    return entries


def finish(current, entries):
    """Close the entry under construction, refusing one that lacks a field."""
    if current is None:
        return None
    missing = [field for field in REQUIRED_FIELDS if field not in current]
    if missing:
        raise Unreadable(
            current["line"],
            current["constant"],
            f"this entry declares no {', '.join(missing)}",
        )
    entries.append(current)
    return None


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
    # Text mode, so universal newlines fold a CRLF checkout to the same text a
    # LF one produces. `declarations` splits on "\n" and would otherwise refuse
    # a file nobody edited.
    text = declaration.read_text(encoding="utf-8")
    # Completeness is discharged here, by the read itself: `declarations`
    # returns only when every line of the file was accounted for, so what
    # follows cannot be a partial sweep.
    pins = declarations(text)
    # Not a completeness check. Exhaustiveness already rules out a partial
    # read, so the only file this can still reject is one that genuinely
    # declares nothing -- a table emptied rather than a table misread -- and
    # the message says so.
    if not pins:
        raise SystemExit("the declaration is readable but declares no Action")
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
