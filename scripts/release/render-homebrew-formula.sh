#!/usr/bin/env bash
# ---
# relationships:
#   publishes: intent-driven-polyglot-release
# ---

set -euo pipefail

version="${1:?version is required}"
artifact_directory="${2:?artifact directory is required}"
output="${3:?output path is required}"
repository="wyrd-company/intentional"

linux_x86_64_archive="$artifact_directory/intentional-linux-x86_64.tar.gz"
linux_arm64_archive="$artifact_directory/intentional-linux-arm64.tar.gz"
macos_arm64_archive="$artifact_directory/intentional-macos-arm64.tar.gz"

for archive in "$linux_x86_64_archive" "$linux_arm64_archive" "$macos_arm64_archive"; do
  if [[ ! -f "$archive" ]]; then
    echo "Missing expected Homebrew release asset: $archive" >&2
    exit 1
  fi
done

linux_x86_64_sha256="$(sha256sum "$linux_x86_64_archive" | cut -d' ' -f1)"
linux_arm64_sha256="$(sha256sum "$linux_arm64_archive" | cut -d' ' -f1)"
macos_arm64_sha256="$(sha256sum "$macos_arm64_archive" | cut -d' ' -f1)"

cat > "$output" <<FORMULA
class Intentional < Formula
  desc "Intent-driven polyglot release and versioning tool"
  homepage "https://github.com/$repository"
  license "Apache-2.0"
  version "$version"

  on_macos do
    on_arm do
      url "https://github.com/$repository/releases/download/$version/intentional-macos-arm64.tar.gz"
      sha256 "$macos_arm64_sha256"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/$repository/releases/download/$version/intentional-linux-arm64.tar.gz"
      sha256 "$linux_arm64_sha256"
    end

    on_intel do
      url "https://github.com/$repository/releases/download/$version/intentional-linux-x86_64.tar.gz"
      sha256 "$linux_x86_64_sha256"
    end
  end

  def install
    bin.install "intentional"
    prefix.install_metafiles
  end

  test do
    assert_match "Intent-driven", shell_output("#{bin}/intentional --help")
  end
end
FORMULA
