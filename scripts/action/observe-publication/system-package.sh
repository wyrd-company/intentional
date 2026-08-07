#!/usr/bin/env bash
# ---

# Assignments feed helpers defined by the sourcing dispatcher.
# shellcheck disable=SC2034
# relationships:
#   implements: github-release-executor
# ---

rm -rf "$INTENTIONAL_WORK"
mkdir -p "$INTENTIONAL_WORK"
package=$(find "$INTENTIONAL_SUBJECT" -maxdepth 1 -type f -print -quit)
test -n "$package"
package_name=$INTENTIONAL_SUBJECT_IDENTITY
package_version=$INTENTIONAL_VERSION
package_sha256=${INTENTIONAL_SUBJECT_DIGEST#sha256:}
curl --fail --silent --show-error --location "$INPUT_PUBLIC_KEY_URL" \
  --output "$INTENTIONAL_WORK/key"
gpg --batch --yes --dearmor --output "$INTENTIONAL_WORK/keyring.gpg" \
  "$INTENTIONAL_WORK/key"

observe_apt() {
  mkdir -p "$INTENTIONAL_WORK/state/lists/partial" \
    "$INTENTIONAL_WORK/cache/archives/partial" "$INTENTIONAL_WORK/etc/apt"
  local package_architecture index package_directory packages_download packages
  package_architecture=$(dpkg-deb -f "$package" Architecture)
  index="$INTENTIONAL_WORK/InRelease"
  package_directory="$INPUT_APT_COMPONENT/binary-$package_architecture"
  packages_download="$INTENTIONAL_WORK/Packages.download"
  packages="$INTENTIONAL_WORK/Packages"
  INTENTIONAL_ELAPSED=0
  while :; do
    if curl --fail --silent --show-error --location \
      "${INTENTIONAL_DESTINATION%/}/dists/$INPUT_APT_SUITE/InRelease" \
      --output "$index"; then
      gpgv --keyring "$INTENTIONAL_WORK/keyring.gpg" "$index"
      local advertised_members index_member expected relative indexed named downloaded
      advertised_members=$(awk -v directory="$package_directory" \
        '$1 == "SHA256:" { section=1; next } section && NF == 3 && $3 ~ ("^" directory "/Packages\\.") { print $3 }' \
        "$index")
      index_member=$(awk -v directory="$package_directory" '
        $1 == "SHA256:" { section=1; next }
        section && NF == 3 && ($3 == directory "/Packages.xz" || $3 == directory "/Packages.gz" || $3 == directory "/Packages") { print $3; exit }
      ' "$index")
      if [[ -z "$index_member" && -n "$advertised_members" ]]; then
        printf 'APT index advertises unsupported package index form(s): %s\n' \
          "$(printf '%s' "$advertised_members" | tr '\n' ' ')" >&2
        exit 1
      fi
      if [[ -n "$index_member" ]]; then
        expected=$(awk -v wanted="$index_member" \
          '$1 == "SHA256:" { section=1; next } section && NF == 3 && $3 == wanted { print $1; exit }' \
          "$index")
        test -n "$expected"
        relative=$index_member
        if awk '$1 == "Acquire-By-Hash:" && $2 == "yes" { found=1 } END { exit !found }' "$index"; then
          relative="$package_directory/by-hash/SHA256/$expected"
        fi
        downloaded=yes
        if ! curl --fail --silent --show-error --location \
          "${INTENTIONAL_DESTINATION%/}/dists/$INPUT_APT_SUITE/$relative" \
          --output "$packages_download"; then
          downloaded=no
          if [[ "$relative" != "$index_member" ]] \
            && curl --fail --silent --show-error --location \
              "${INTENTIONAL_DESTINATION%/}/dists/$INPUT_APT_SUITE/$index_member" \
              --output "$packages_download"; then
            downloaded=yes
          fi
        fi
        if [[ "$downloaded" == yes ]] \
          && [[ "$(sha256sum "$packages_download" | cut -d' ' -f1)" == "$expected" ]]; then
          case "$index_member" in
            *.gz) gzip -dc "$packages_download" > "$packages" ;;
            *.xz) xz -dc "$packages_download" > "$packages" ;;
            *) cp "$packages_download" "$packages" ;;
          esac
          indexed=$(awk -v RS='' -v name="$package_name" -v version="$package_version" \
            -v architecture="$package_architecture" -v digest="$package_sha256" \
            '$0 ~ "(^|\\n)Package: " name "(\\n|$)" && $0 ~ "(^|\\n)Version: " version "(\\n|$)" && $0 ~ "(^|\\n)Architecture: " architecture "(\\n|$)" && $0 ~ "(^|\\n)SHA256: " digest "(\\n|$)" { print "yes"; exit }' \
            "$packages")
          named=$(awk -v RS='' -v name="$package_name" \
            '$0 ~ "(^|\\n)Package: " name "(\\n|$)" { print "yes"; exit }' "$packages")
          if [[ "$indexed" == yes ]]; then break; fi
          if [[ "$named" == yes ]]; then
            printf '%s is indexed with facts that disagree with the sealed package\n' "$package_name" >&2
            exit 1
          fi
        fi
      fi
    fi
    if ! wait_again; then observe_state pending; exit 0; fi
  done
  printf 'deb [signed-by=%s] %s %s %s\n' "$INTENTIONAL_WORK/keyring.gpg" \
    "$INTENTIONAL_DESTINATION" "$INPUT_APT_SUITE" "$INPUT_APT_COMPONENT" \
    > "$INTENTIONAL_WORK/etc/apt/sources.list"
  apt-get -o Dir::Etc="$INTENTIONAL_WORK/etc/apt" \
    -o Dir::State="$INTENTIONAL_WORK/state" -o Dir::Cache="$INTENTIONAL_WORK/cache" \
    -o APT::Get::List-Cleanup=0 update
  (cd "$INTENTIONAL_WORK" && apt-get -o Dir::Etc="$INTENTIONAL_WORK/etc/apt" \
    -o Dir::State="$INTENTIONAL_WORK/state" -o Dir::Cache="$INTENTIONAL_WORK/cache" \
    download "$package_name=$package_version")
  local retrieved
  retrieved=$(find "$INTENTIONAL_WORK" -maxdepth 1 -type f -name '*.deb' -print -quit)
  INTENTIONAL_DESTINATION_DIGEST=$INTENTIONAL_SUBJECT_DIGEST
  INTENTIONAL_RETRIEVED_DIGEST="sha256:$(sha256sum "$retrieved" | cut -d' ' -f1)"
  test "$INTENTIONAL_RETRIEVED_DIGEST" = "$INTENTIONAL_SUBJECT_DIGEST"
  INTENTIONAL_RETRIEVAL_VERSION=$(apt --version 2>&1 | head -n1)
}

observe_rpm() {
  mkdir -p "$INTENTIONAL_WORK/state" "$INTENTIONAL_WORK/cache" \
    "$INTENTIONAL_WORK/etc/yum.repos.d" "$INTENTIONAL_WORK/retrieved"
  local package_architecture index primary entries
  package_architecture=$(rpm -qp --qf '%{ARCH}' "$package")
  index="$INTENTIONAL_WORK/repomd.xml"
  primary="$INTENTIONAL_WORK/primary"
  entries="$INTENTIONAL_WORK/primary.entries"
  INTENTIONAL_ELAPSED=0
  while :; do
    if curl --fail --silent --show-error --location \
      "${INTENTIONAL_DESTINATION%/}/$INPUT_RPM_CHANNEL/repodata/repomd.xml" \
      --output "$index" \
      && curl --fail --silent --show-error --location \
        "${INTENTIONAL_DESTINATION%/}/$INPUT_RPM_CHANNEL/repodata/repomd.xml.asc" \
        --output "$index.asc"; then
      gpgv --keyring "$INTENTIONAL_WORK/keyring.gpg" "$index.asc" "$index"
      local expected relative indexed named
      read -r expected relative < <(python3 - "$index" 2>/dev/null <<'PY'
import sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
data = next(node for node in root if node.tag.endswith('data') and node.attrib.get('type') == 'primary')
checksum = next(node.text for node in data if node.tag.endswith('checksum'))
location = next(node.attrib['href'] for node in data if node.tag.endswith('location'))
print(checksum, location)
PY
      ) || true
      if [[ -n "$expected" && -n "$relative" ]] \
        && curl --fail --silent --show-error --location \
          "${INTENTIONAL_DESTINATION%/}/$INPUT_RPM_CHANNEL/$relative" --output "$primary" \
        && [[ "$(sha256sum "$primary" | cut -d' ' -f1)" == "$expected" ]]; then
        if ! python3 - "$primary" > "$entries" 2>/dev/null <<'PY'
import bz2, gzip, lzma, pathlib, sys, xml.etree.ElementTree as ET
raw = pathlib.Path(sys.argv[1]).read_bytes()
for opener in (gzip.decompress, bz2.decompress, lzma.decompress):
    try:
        raw = opener(raw)
        break
    except Exception:
        pass
root = ET.fromstring(raw)
for package in root:
    fields = {node.tag.rsplit('}', 1)[-1]: node for node in package}
    checksum, version, name, architecture = (fields.get(key) for key in ('checksum', 'version', 'name', 'arch'))
    if all(field is not None for field in (checksum, version, name, architecture)):
        print(name.text, version.attrib.get('ver'), architecture.text, checksum.text, sep='\t')
PY
        then
          if ! wait_again; then observe_state pending; exit 0; fi
          continue
        fi
        indexed=$(awk -F '\t' -v name="$package_name" -v version="$package_version" \
          -v architecture="$package_architecture" -v digest="$package_sha256" \
          '$1 == name && $2 == version && $3 == architecture && $4 == digest { print "yes"; exit }' "$entries")
        named=$(awk -F '\t' -v name="$package_name" '$1 == name { print "yes"; exit }' "$entries")
        if [[ "$indexed" == yes ]]; then break; fi
        if [[ "$named" == yes ]]; then
          printf '%s is indexed with facts that disagree with the sealed package\n' "$package_name" >&2
          exit 1
        fi
      fi
    fi
    if ! wait_again; then observe_state pending; exit 0; fi
  done
  printf '[intentional]\nname=Intentional scratch\nbaseurl=%s/%s\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=1\ngpgkey=file://%s\n' \
    "${INTENTIONAL_DESTINATION%/}" "$INPUT_RPM_CHANNEL" "$INTENTIONAL_WORK/key" \
    > "$INTENTIONAL_WORK/etc/yum.repos.d/intentional.repo"
  dnf --config /dev/null --setopt=reposdir="$INTENTIONAL_WORK/etc/yum.repos.d" \
    --setopt=cachedir="$INTENTIONAL_WORK/cache" --setopt=persistdir="$INTENTIONAL_WORK/state" \
    --assumeyes --downloadonly --downloaddir="$INTENTIONAL_WORK/retrieved" \
    install "$package_name-$package_version.$package_architecture"
  local retrieved
  retrieved=$(find "$INTENTIONAL_WORK/retrieved" -type f -name '*.rpm' -print -quit)
  INTENTIONAL_DESTINATION_DIGEST=$INTENTIONAL_SUBJECT_DIGEST
  INTENTIONAL_RETRIEVED_DIGEST="sha256:$(sha256sum "$retrieved" | cut -d' ' -f1)"
  test "$INTENTIONAL_RETRIEVED_DIGEST" = "$INTENTIONAL_SUBJECT_DIGEST"
  INTENTIONAL_RETRIEVAL_VERSION=$(dnf --version 2>&1 | head -n1)
}

"observe_$INTENTIONAL_PUBLISHER"
case "$INTENTIONAL_PACKAGER_ID" in
  goreleaser) INTENTIONAL_PACKAGER_VERSION=$(goreleaser --version | head -n1) ;;
  cargo-archive) INTENTIONAL_PACKAGER_VERSION=$(intentional --version | awk '{print $2}') ;;
  *) printf 'unsupported system-package packager: %s\n' "$INTENTIONAL_PACKAGER_ID" >&2; exit 1 ;;
esac
observe_present
