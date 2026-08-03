#!/usr/bin/env bash
# Project verified `key: value` identity lines onto GitHub step outputs.
#
# Usage: project-identities.sh <output-file> <key>...
#
# The command output is read from standard input. Each requested key must be
# present exactly once and must match the shape its contract requires, so a
# malformed or repeated line can never reach a privileged step as an output.
set -euo pipefail

OUTPUT_FILE="${1:?GitHub output file}"
shift

INPUT="$(cat)"
printf '%s\n' "$INPUT"

for KEY in "$@"; do
  COUNT="$(printf '%s\n' "$INPUT" | grep -c "^$KEY: " || true)"
  if [[ "$COUNT" != "1" ]]; then
    echo "::error::intentional reported $KEY $COUNT times; exactly one is required."
    exit 1
  fi
  VALUE="$(printf '%s\n' "$INPUT" | sed -n "s/^$KEY: //p")"
  case "$KEY" in
    source-sha | release-sha)
      if [[ ! "$VALUE" =~ ^[0-9a-f]{40}$ && ! "$VALUE" =~ ^[0-9a-f]{64}$ ]]; then
        echo "::error::$KEY is not a complete Git object identity."
        exit 1
      fi
      ;;
    plan-digest)
      if [[ ! "$VALUE" =~ ^sha256:[0-9a-f]{64}$ ]]; then
        echo "::error::$KEY is not a canonical sha256 digest."
        exit 1
      fi
      ;;
    global-tag)
      if [[ -z "$VALUE" || "$VALUE" =~ [[:space:]] ]]; then
        echo "::error::$KEY is not a usable Git tag name."
        exit 1
      fi
      ;;
    *)
      if [[ -z "$VALUE" ]]; then
        echo "::error::$KEY is empty."
        exit 1
      fi
      ;;
  esac
  printf '%s=%s\n' "$KEY" "$VALUE" >> "$OUTPUT_FILE"
done
