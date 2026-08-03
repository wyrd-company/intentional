#!/usr/bin/env python3
# ---
# relationships:
#   validates: github-release-executor
# ---
"""Lint the reusable composite Actions this repository publishes.

`actionlint` reads workflows, not action documents, so nothing else inspects
the composite steps that ship to consumers. Three properties matter enough to
gate:

* A `${{ ... }}` expansion inside a `run:` script is textual substitution into
  shell source. Any input carrying a quote, a newline, or a `$(...)` becomes
  executable text. Routing the same value through `env:` and reading it as a
  shell variable keeps it data.
* A `run:` step without `shell:` is rejected by the runner, and a shell this
  repository does not otherwise rely on is not covered by `shellcheck` or by
  anything a reviewer reads.
* An external `uses:` reference resolved by tag or branch is mutable, so the
  code a consumer executes is whatever that name points at on the day it runs.
  A container reference carries no commit identity, so its immutable name is the
  manifest digest instead.
* A `runs.using` runtime the gate does not recognise leaves every step in that
  document unread, so it is reported rather than skipped. A recognised runtime
  is inspected: a container action's `runs.image` is held to the same digest
  rule a container step is, because naming a runtime as known and then reading
  nothing is the same clean-pass-over-nothing this gate exists to stop.

Run with no arguments to lint every action document in the repository. Explicit
paths are for this gate's own tests.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path, PurePosixPath

import yaml

# Every `run:` step in this repository uses bash, and `shellcheck` covers the
# scripts those steps invoke. Widening this set means widening that coverage
# first.
SUPPORTED_SHELLS = ("bash",)

EXPANSION = re.compile(r"\$\{\{")

# A complete commit identity is the full 40-character object name. An
# abbreviation is not a commit identity: it names whatever object currently
# shares that prefix.
PINNED_REFERENCE = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")

# A container reference carries no commit identity at all. Its immutable name is
# the manifest digest, so it is held to that rule and told so in those terms.
CONTAINER_SCHEME = "docker://"
PINNED_IMAGE = re.compile(r"^[^@\s]+@sha256:[0-9a-f]{64}$")

COMPOSITE_RUNTIME = "composite"
CONTAINER_RUNTIME = "docker"
# GitHub retires JavaScript runtimes on its own schedule. Naming the ones this
# repository is willing to publish means a runtime that ages out is reported
# rather than waved through as "something node-shaped".
NODE_RUNTIMES = ("node20", "node24")
KNOWN_RUNTIMES = (COMPOSITE_RUNTIME, CONTAINER_RUNTIME, *NODE_RUNTIMES)

ROOT = Path(__file__).resolve().parents[2]

# GitHub accepts either spelling, and resolves an Action from `.github/actions/`
# exactly as it does from anywhere else in the tree. A gate that reads one
# spelling in one directory reports clean over files it never opened.
ACTION_FILENAMES = ("action.yml", "action.yaml")
SEARCH_ROOTS = ("actions", ".github/actions")

# Build output and vendored dependencies carry action documents this repository
# does not publish and cannot fix.
EXCLUDED_DIRECTORIES = frozenset({"target", "node_modules"})


class LineLoader(yaml.SafeLoader):
    """A loader that records where each mapping started."""

    def construct_mapping(self, node, deep=False):  # type: ignore[override]
        mapping = super().construct_mapping(node, deep=deep)
        mapping["__line__"] = node.start_mark.line + 1
        return mapping


def is_excluded(path: Path) -> bool:
    return bool(EXCLUDED_DIRECTORIES.intersection(path.relative_to(ROOT).parts))


def discover() -> list[Path]:
    """Return every action document in the repository, root action first."""

    documents = [
        candidate
        for filename in ACTION_FILENAMES
        if (candidate := ROOT / filename).is_file()
    ]

    nested: set[Path] = set()
    for relative in SEARCH_ROOTS:
        base = ROOT / relative
        if not base.is_dir():
            continue
        for filename in ACTION_FILENAMES:
            nested.update(path for path in base.rglob(filename) if not is_excluded(path))

    documents.extend(sorted(nested))
    return documents


def unreachable_links() -> list[str]:
    """Report symlinked directories under the search roots.

    `rglob` does not descend a symlinked directory, so an action document
    reachable only through one is never opened. Following it would invite a
    cycle. Reporting it keeps the gate's field of view equal to its claim: the
    reader is told the tree holds a door this gate will not walk through.
    """

    findings = []
    for relative in SEARCH_ROOTS:
        base = ROOT / relative
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*")):
            if is_excluded(path) or not path.is_symlink() or not path.is_dir():
                continue
            findings.append(
                f"{describe(path)}: is a symlinked directory; discovery does not "
                "descend one, so any action document beneath it would go "
                "unchecked. Replace it with a real directory."
            )
    return findings


def describe(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def step_name(step: dict, position: int) -> str:
    name = step.get("name")
    if isinstance(name, str) and name:
        return f'step "{name}"'
    return f"step {position}"


def check_step(step: dict, position: int, location: str) -> list[str]:
    findings = []
    label = step_name(step, position)
    line = step.get("__line__", 0)
    where = f"{location}:{line}: {label}"

    script = step.get("run")
    if script is not None:
        if not isinstance(script, str):
            findings.append(f"{where} has a run: that is not a script")
            return findings

        if EXPANSION.search(script):
            findings.append(
                f"{where} interpolates a ${{{{ ... }}}} expansion into its run: "
                "script; pass the value through env: and read it as a shell "
                "variable instead"
            )

        shell = step.get("shell")
        if shell is None:
            findings.append(f"{where} has a run: without a shell:")
        elif shell not in SUPPORTED_SHELLS:
            supported = ", ".join(SUPPORTED_SHELLS)
            findings.append(
                f"{where} runs under shell {shell!r}; this repository supports "
                f"only {supported}"
            )

    reference = step.get("uses")
    if reference is not None:
        findings.extend(check_reference(reference, where))

    return findings


def container_target(reference: str) -> str | None:
    """Return the reference past its `docker://` scheme, or None if it has none.

    The scheme is matched case-insensitively. Reading it any other way sends
    `DOCKER://alpine:3.20` to the commit-identity branch, which tells the reader
    to pin an image to something images do not have.
    """

    if reference[: len(CONTAINER_SCHEME)].lower() == CONTAINER_SCHEME:
        return reference[len(CONTAINER_SCHEME) :]
    return None


def digest_finding(reference: str, where: str) -> str:
    return (
        f"{where} uses {reference}, which is not pinned to an image digest; "
        f"reference the image as {CONTAINER_SCHEME}<image>@sha256:<64 "
        "hexadecimal characters>"
    )


def check_reference(reference: object, where: str) -> list[str]:
    if not isinstance(reference, str):
        return [f"{where} has a uses: that is not a reference"]

    if reference.startswith("./") or reference.startswith("../"):
        return []

    target = container_target(reference)
    if target is not None:
        if PINNED_IMAGE.match(target):
            return []
        return [digest_finding(reference, where)]

    if not PINNED_REFERENCE.match(reference):
        return [
            f"{where} uses {reference}, which is not pinned to a complete "
            "40-character commit identity"
        ]

    return []


def check_image(image: object, where: str) -> list[str]:
    """Hold a container runtime's image to the same digest rule as a step.

    GitHub accepts a registry reference with or without the `docker://` scheme,
    so `alpine:latest` is as mutable here as `docker://alpine:latest` is in a
    step. A Dockerfile is repository content, versioned with the action itself,
    and is the one form that needs no digest.
    """

    if not isinstance(image, str) or not image:
        return [f"{where} is not an image reference"]

    target = container_target(image)
    if target is None:
        if PurePosixPath(image).name.startswith("Dockerfile"):
            return []
        return [digest_finding(image, where)]

    if PINNED_IMAGE.match(target):
        return []
    return [digest_finding(image, where)]


def check(path: Path) -> list[str]:
    location = describe(path)

    try:
        document = yaml.load(path.read_text(), Loader=LineLoader)
    except yaml.YAMLError as error:
        return [f"{location}: is not parseable YAML: {error}"]

    if not isinstance(document, dict):
        return [f"{location}: is not an action document"]

    runs = document.get("runs")
    if not isinstance(runs, dict):
        return [f"{location}: has no runs: block"]

    using = runs.get("using")
    if not using:
        return [f"{location}: does not declare runs.using"]

    if not isinstance(using, str):
        return [f"{location}: declares a runs.using that is not a runtime name"]

    # GitHub resolves the runtime name case-insensitively. An unrecognised name
    # is a finding rather than a skip: the alternative is a document whose steps
    # are never read reporting a clean pass.
    runtime = using.strip().lower()
    if runtime not in KNOWN_RUNTIMES:
        recognised = ", ".join(KNOWN_RUNTIMES)
        return [
            f"{location}: declares runs.using {using!r}, which this gate does "
            f"not recognise; it recognises {recognised}"
        ]

    if runtime == CONTAINER_RUNTIME:
        if "image" not in runs:
            return [f"{location}: runs with the docker runtime but declares no image:"]
        return check_image(runs["image"], f"{location}: runs.image")

    if runtime != COMPOSITE_RUNTIME:
        return []

    steps = runs.get("steps")
    if not isinstance(steps, list) or not steps:
        return [f"{location}: is composite but declares no steps"]

    findings = []
    for position, step in enumerate(steps, start=1):
        if not isinstance(step, dict):
            findings.append(f"{location}: step {position} is not a mapping")
            continue
        findings.extend(check_step(step, position, location))
    return findings


def main(argv: list[str]) -> int:
    explicit = [Path(argument) for argument in argv]
    documents = explicit or discover()

    if not documents:
        print("No action document was linted; the gate is vacuous.", file=sys.stderr)
        return 1

    # Only discovery can leave a document unread. Explicit paths were named by
    # the caller, so there is no field of view to report on.
    findings = [] if explicit else unreachable_links()
    for path in documents:
        if not path.is_file():
            findings.append(f"{describe(path)}: does not exist")
            continue
        findings.extend(check(path))

    for finding in findings:
        print(finding, file=sys.stderr)

    if findings:
        return 1

    print(f"Action documents pass composite linting ({len(documents)} checked).")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
