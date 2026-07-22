#!/usr/bin/env python3
# ---
# relationships:
#   validates: intent-driven-polyglot-release
# ---

import argparse
import json
from pathlib import Path
import sys
import tomllib


def fail(message: str) -> None:
    print(f"release version check failed: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expected")
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[2]
    workspace = tomllib.loads((root / "Cargo.toml").read_text())
    cli = tomllib.loads((root / "crates/cli/Cargo.toml").read_text())
    npm = json.loads((root / "npm/package.json").read_text())

    cargo_version = workspace["workspace"]["package"]["version"]
    core_dependency = cli["dependencies"]["intentional-core"]["version"]
    npm_version = npm["version"]
    expected = args.expected or cargo_version

    versions = {
        "Cargo workspace": cargo_version,
        "intentional-core CLI dependency": core_dependency,
        "npm package": npm_version,
    }
    for source, version in versions.items():
        if version != expected:
            fail(f"{source} is {version}, expected {expected}")

    if npm["name"] != "@wyrd-company/intentional":
        fail("npm package name is not @wyrd-company/intentional")
    if npm["publishConfig"] != {
        "access": "public",
        "registry": "https://registry.npmjs.org",
    }:
        fail("npm publishConfig must bind public npmjs publication")
    if npm["bin"] != {"intentional": "bin/intentional.js"}:
        fail("npm executable mapping is not exact")

    print(f"release versions agree at {expected}")


if __name__ == "__main__":
    main()
