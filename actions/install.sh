#!/usr/bin/env bash
# ---
# relationships:
#   implements: github-release-executor
# ---
#
# Install a released intentional binary onto the runner PATH after verifying
# its published checksum. Credential-free Intentional Actions source their
# executable through this one verified path.

set -euo pipefail

REQUESTED_VERSION="${1:-latest}"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "::error::Intentional actions support Linux runners."
  exit 1
fi

case "$(uname -m)" in
  x86_64)
    ASSET_NAME="intentional-linux-x86_64.tar.gz"
    ;;
  aarch64 | arm64)
    ASSET_NAME="intentional-linux-arm64.tar.gz"
    ;;
  *)
    echo "::error::No released intentional binary for architecture $(uname -m)."
    exit 1
    ;;
esac

if [[ "$REQUESTED_VERSION" == "latest" ]]; then
  DOWNLOAD_URL="https://github.com/wyrd-company/intentional/releases/latest/download/$ASSET_NAME"
elif [[ "$REQUESTED_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  DOWNLOAD_URL="https://github.com/wyrd-company/intentional/releases/download/$REQUESTED_VERSION/$ASSET_NAME"
else
  echo "::error::intentional-version must be latest or a plain SemVer version."
  exit 1
fi

INSTALL_DIR="$RUNNER_TEMP/intentional/bin"
ARCHIVE_DIR="$(mktemp -d)"
ARCHIVE_PATH="$ARCHIVE_DIR/$ASSET_NAME"
CHECKSUM_PATH="$ARCHIVE_DIR/SHA256SUMS"
trap 'rm -rf "$ARCHIVE_DIR"' EXIT

RELEASE_BASE="${DOWNLOAD_URL%/"$ASSET_NAME"}"
curl --fail --silent --show-error --location --max-redirs 5 \
  --proto '=https' --tlsv1.2 \
  --output "$CHECKSUM_PATH" "$RELEASE_BASE/SHA256SUMS"
curl --fail --silent --show-error --location --max-redirs 5 \
  --proto '=https' --tlsv1.2 \
  --output "$ARCHIVE_PATH" "$DOWNLOAD_URL"

EXPECTED_CHECKSUM="$(awk -v asset="$ASSET_NAME" '$2 == asset { print $1 }' "$CHECKSUM_PATH")"
if [[ ! "$EXPECTED_CHECKSUM" =~ ^[a-fA-F0-9]{64}$ ]] ||
  [[ "$(awk -v asset="$ASSET_NAME" '$2 == asset { count++ } END { print count + 0 }' "$CHECKSUM_PATH")" != "1" ]]; then
  echo "::error::SHA256SUMS must contain exactly one checksum for $ASSET_NAME."
  exit 1
fi
ACTUAL_CHECKSUM="$(sha256sum "$ARCHIVE_PATH" | cut -d' ' -f1)"
if [[ "${ACTUAL_CHECKSUM,,}" != "${EXPECTED_CHECKSUM,,}" ]]; then
  echo "::error::Checksum verification failed for $ASSET_NAME."
  exit 1
fi

mkdir -p "$INSTALL_DIR"
tar -xOzf "$ARCHIVE_PATH" "${ASSET_NAME%.tar.gz}/intentional" \
  > "$INSTALL_DIR/intentional.partial"
test -s "$INSTALL_DIR/intentional.partial"
mv "$INSTALL_DIR/intentional.partial" "$INSTALL_DIR/intentional"
chmod +x "$INSTALL_DIR/intentional"
echo "$INSTALL_DIR" >> "$GITHUB_PATH"
