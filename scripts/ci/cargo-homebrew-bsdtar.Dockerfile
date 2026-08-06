# ---
# relationships:
#   validates: github-release-executor
# ---

# libarchive 3.4.3 predates the bsdtar shipped by the macOS 14 runner. The
# digest fixes the provisioning base; the package version makes repository
# drift fail the build instead of silently changing the compatibility claim.
FROM debian:11.11-slim@sha256:f313b4bd62667092a59b3a664d7d3ab8b5e65f41675f48e81455a15dc5abe792

RUN apt-get update \
    && apt-get install --yes --no-install-recommends libarchive-tools=3.4.3-2+deb11u4 \
    && test "$(bsdtar --version | sed -n '1s/bsdtar \([^ ]*\).*/\1/p')" = 3.4.3 \
    && rm -rf /var/lib/apt/lists/*
