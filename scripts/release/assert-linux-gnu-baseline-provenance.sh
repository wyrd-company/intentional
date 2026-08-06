#!/usr/bin/env bash
# ---
# relationships:
#   validates: intent-driven-polyglot-release
#   references: build-linux-gnu-release-on-pinned-cross-baseline
# ---

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
baseline_env="$root/scripts/release/linux-gnu-baseline.env"
technical_design="$root/docs/technical-designs/intent-driven-polyglot-release.yml"
decision_record="$root/docs/decisions/build-linux-gnu-release-on-pinned-cross-baseline.yml"
install_doc="$root/docs/install.md"

required_env_vars=(
  CROSS_SOURCE_TAG_OBJECT
  CROSS_SOURCE_COMMIT
  CROSS_OCI_REVISION
  CROSS_OCI_VERSION
  CROSS_OCI_SOURCE
  X86_64_GNU_DOCKERFILE
  AARCH64_GNU_DOCKERFILE
  GNU_BUILD_BASE_DISTRO
  GNU_BUILD_BASE_SUPPORT_STATUS
  GNU_IMAGE_HOST_ARCH
  GNU_SYSROOT_GLIBC_VERSION
  GNU_BUILD_GIT_VERSION
  LINUX_GNU_MAX_GLIBC_SYMBOL
  X86_64_GNU_CROSS_IMAGE
  AARCH64_GNU_CROSS_IMAGE
)

# shellcheck disable=SC1090
source "$baseline_env"

for variable in "${required_env_vars[@]}"; do
  if [[ -z "${!variable:-}" ]]; then
    echo "linux-gnu-baseline.env is missing $variable." >&2
    exit 1
  fi
done

if ! grep -Fq "minimum supported Git is $GNU_BUILD_GIT_VERSION" "$root/README.md"; then
  echo "README.md must record minimum supported Git $GNU_BUILD_GIT_VERSION." >&2
  exit 1
fi

for file in "$technical_design" "$decision_record" "$install_doc"; do
  if ! grep -Fq "$CROSS_SOURCE_COMMIT" "$file"; then
    echo "$file must record cross source commit $CROSS_SOURCE_COMMIT." >&2
    exit 1
  fi
  if ! grep -Fq "$CROSS_SOURCE_TAG_OBJECT" "$file"; then
    echo "$file must record cross tag object $CROSS_SOURCE_TAG_OBJECT." >&2
    exit 1
  fi
  if ! grep -Fq 'Ubuntu 16.04' "$file"; then
    echo "$file must record the Ubuntu 16.04 build base." >&2
    exit 1
  fi
  if ! grep -Fq 'glibc 2.23' "$file"; then
    echo "$file must distinguish sysroot glibc 2.23 from the symbol ceiling." >&2
    exit 1
  fi
  if ! grep -Fq 'GLIBC_2.18' "$file"; then
    echo "$file must record the GLIBC_2.18 symbol ceiling." >&2
    exit 1
  fi
done

for dockerfile in "$X86_64_GNU_DOCKERFILE" "$AARCH64_GNU_DOCKERFILE"; do
  if ! grep -Fq "$dockerfile" "$technical_design"; then
    echo "technical design must reference Dockerfile $dockerfile." >&2
    exit 1
  fi
done

echo "Linux GNU baseline provenance fields are present and consistent."
