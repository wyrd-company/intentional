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

Run with no arguments to lint every action document in the repository. Explicit
paths are for this gate's own tests.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

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

ROOT = Path(__file__).resolve().parents[2]


class LineLoader(yaml.SafeLoader):
    """A loader that records where each mapping started."""

    def construct_mapping(self, node, deep=False):  # type: ignore[override]
        mapping = super().construct_mapping(node, deep=deep)
        mapping["__line__"] = node.start_mark.line + 1
        return mapping


def discover() -> list[Path]:
    """Return every action document in the repository, root action first."""

    documents = []
    root_action = ROOT / "action.yml"
    if root_action.is_file():
        documents.append(root_action)
    documents.extend(sorted((ROOT / "actions").rglob("action.yml")))
    return documents


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
        if not isinstance(reference, str):
            findings.append(f"{where} has a uses: that is not a reference")
        elif reference.startswith("./") or reference.startswith("../"):
            pass
        elif not PINNED_REFERENCE.match(reference):
            findings.append(
                f"{where} uses {reference}, which is not pinned to a complete "
                "40-character commit identity"
            )

    return findings


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

    if using != "composite":
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
    documents = [Path(argument) for argument in argv] or discover()

    if not documents:
        print("No action document was linted; the gate is vacuous.", file=sys.stderr)
        return 1

    findings = []
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
