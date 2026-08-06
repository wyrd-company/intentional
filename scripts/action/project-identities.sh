#!/usr/bin/env bash
# Project verified `key: value` identity lines onto GitHub step outputs.
#
# Usage: project-identities.sh <output-file> <key>...
#
# The command output is read from standard input. Each requested key must be
# present exactly once and must match its declared shape, so a malformed,
# repeated, or unowned identity can never reach a privileged step as an output.
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
    source-sha | release-sha | global-tag-object)
      if [[ ! "$VALUE" =~ ^[0-9a-f]{40}$ && ! "$VALUE" =~ ^[0-9a-f]{64}$ ]]; then
        echo "::error::$KEY is not a complete Git object identity."
        exit 1
      fi
      ;;
    plan-digest | digest)
      if [[ ! "$VALUE" =~ ^sha256:[0-9a-f]{64}$ ]]; then
        echo "::error::$KEY is not a canonical sha256 digest."
        exit 1
      fi
      ;;
    version)
      if [[ ! "$VALUE" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]]; then
        echo "::error::$KEY is not a canonical Semantic Versioning 2.0.0 version."
        exit 1
      fi
      WITHOUT_BUILD="${VALUE%%+*}"
      if [[ "$WITHOUT_BUILD" == *-* ]]; then
        PRERELEASE="${WITHOUT_BUILD#*-}"
        IFS=. read -r -a IDENTIFIERS <<< "$PRERELEASE"
        for IDENTIFIER in "${IDENTIFIERS[@]}"; do
          if [[ "$IDENTIFIER" =~ ^[0-9]+$ && "$IDENTIFIER" != "0" && "$IDENTIFIER" == 0* ]]; then
            echo "::error::$KEY has a numeric prerelease identifier with a leading zero."
            exit 1
          fi
        done
      fi
      ;;
    global-tag)
      if [[ -z "$VALUE" || "$VALUE" =~ [[:space:]] ]]; then
        echo "::error::$KEY is not a usable Git tag name."
        exit 1
      fi
      ;;
    evidence-path | built-subject-path | sealed-phase-evidence)
      # The projected path is the authoritative fragment, so a value that does
      # not name a file the command actually wrote must never reach a consumer.
      if [[ ! -f "$VALUE" ]]; then
        echo "::error::$KEY does not name a file intentional wrote."
        exit 1
      fi
      ;;
    candidate-path)
      # A release candidate is a directory contract rather than one fragment.
      if [[ ! -d "$VALUE" ]]; then
        echo "::error::$KEY does not name a directory intentional wrote."
        exit 1
      fi
      ;;
    *)
      echo "::error::$KEY has no declared projection contract."
      exit 1
      ;;
  esac
  printf '%s=%s\n' "$KEY" "$VALUE" >> "$OUTPUT_FILE"
done
