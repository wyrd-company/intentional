# ---
# relationships:
#   validates: github-release-executor
# ---

# Apple's gzip is BSD-derived and assigns the current time to piped standard
# input unless -n clears it. Build the portable FreeBSD 13 implementation so
# the macOS archive replay exercises that header behavior instead of Debian's
# GNU gzip. The source archive and build base are both content pinned.
FROM alpine:3.22.1@sha256:4bcff63911fcb4448bd4fdacec207030997caf25e9bea4045fa6c8c44de311d1 AS bsdgzip

ADD --checksum=sha256:e7e7c39263a8d97a388f0f971ea0f09812aee6209ea28d3a60b19dbf8771c950 \
    https://github.com/chimera-linux/bsdgzip/archive/58b42c173df7a55697acb6dadb4a216fc70b62ad.tar.gz \
    /tmp/bsdgzip.tar.gz

RUN apk add --no-cache \
        build-base=0.5-r3 \
        musl-fts-dev=1.2.7-r6 \
        zlib-dev=1.3.2-r0 \
        zlib-static=1.3.2-r0 \
    && mkdir /tmp/bsdgzip \
    && tar -xzf /tmp/bsdgzip.tar.gz --strip-components=1 -C /tmp/bsdgzip \
    && cd /tmp/bsdgzip \
    && cc -std=c99 -D_GNU_SOURCE -Dlint \
        -DNO_BZIP2_SUPPORT -DNO_COMPRESS_SUPPORT -DNO_PACK_SUPPORT \
        -DNO_XZ_SUPPORT -DNO_LZ_SUPPORT \
        -I. -O2 -static gzip.c -lz -lfts -o /usr/local/bin/gzip \
    && test "$(/usr/local/bin/gzip --version 2>&1)" = "FreeBSD gzip 20190107"

# libarchive 3.4.3 predates the bsdtar shipped by the macOS 14 runner. The
# digest fixes the provisioning base; the package version makes repository
# drift fail the build instead of silently changing the compatibility claim.
FROM debian:11.11-slim@sha256:f313b4bd62667092a59b3a664d7d3ab8b5e65f41675f48e81455a15dc5abe792

COPY --from=bsdgzip /usr/local/bin/gzip /usr/local/bin/gzip

RUN apt-get update \
    && apt-get install --yes --no-install-recommends libarchive-tools=3.4.3-2+deb11u4 \
    && test "$(bsdtar --version | sed -n '1s/bsdtar \([^ ]*\).*/\1/p')" = 3.4.3 \
    && test "$(gzip --version 2>&1)" = "FreeBSD gzip 20190107" \
    && rm -rf /var/lib/apt/lists/*
