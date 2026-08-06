#!/usr/bin/env bash
# ---

# Assignments feed helpers defined by the sourcing dispatcher.
# shellcheck disable=SC2034
# relationships:
#   implements: github-release-executor
# ---

repository="$INPUT_REGISTRY/$INTENTIONAL_DESTINATION"
mkdir -p "$INTENTIONAL_WORK"
clean_client="$INTENTIONAL_WORK/clean"
rm -rf "$clean_client"
mkdir -p "$clean_client"
export DOCKER_CONFIG=$clean_client
aliases_file="$INTENTIONAL_WORK/aliases.yml"
metadata_file="$INTENTIONAL_WORK/metadata.yml"
provenance_file="$INTENTIONAL_WORK/provenance.yml"
: > "$aliases_file"
: > "$metadata_file"
: > "$provenance_file"
if ! published=$(crane digest "$repository:$INTENTIONAL_VERSION" 2>/dev/null) \
  || ! index=$(crane manifest "$repository@$published" 2>/dev/null); then
  printf '%s carries version %s but no public client can retrieve it; an OCI package is observable to its consumers only while it is public, and a package is private when it is first pushed\n' \
    "$repository" "$INTENTIONAL_VERSION" >&2
  observe_state pending
  exit 0
fi

if [[ "$INTENTIONAL_PACKAGER_ID" == buildx ]]; then
  layout="$INTENTIONAL_WORK/layout"
  rm -rf "$layout"
  mkdir -p "$layout"
  tar -xf "$INTENTIONAL_SUBJECT/subject.oci.tar" -C "$layout"
  sealed_index=$(jq -r '.manifests[0].digest' "$layout/index.json")
  sealed_manifests=$(jq -S -r '[.manifests[].digest] | sort | .[]' \
    "$layout/blobs/sha256/${sealed_index#sha256:}")
  destination_manifests=$(printf '%s' "$index" | jq -S -r '[.manifests[].digest] | sort | .[]')
  if [[ "$destination_manifests" != "$sealed_manifests" ]]; then
    observe_state conflict \
      "$repository already holds $published under version $INTENTIONAL_VERSION, which is not the subject this release built"
    exit 0
  fi
  test "$(printf '%s' "$index" | jq -r '.annotations["org.opencontainers.image.version"] // ""')" \
    = "$INTENTIONAL_VERSION"
  test "$(printf '%s' "$index" | jq -r '.annotations["org.opencontainers.image.title"] // ""')" \
    = "$INTENTIONAL_SUBJECT_IDENTITY"

  aliases=""
  core=${INTENTIONAL_VERSION%%-*}
  major=${core%%.*}
  minor=${core%.*}
  minor_pattern=${minor//./\.}
  major_pattern=${major//./\.}
  published_tags=$(crane ls "$repository" | grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' || true)
  newest=$(printf '%s\n' "$published_tags" | sort -V | tail -n1)
  if [[ "$newest" == "$INTENTIONAL_VERSION" ]]; then aliases="latest"; fi
  newest_minor=$(printf '%s\n' "$published_tags" \
    | grep -E "^${minor_pattern}\.[0-9]+$" | sort -V | tail -n1 || true)
  if [[ "$newest_minor" == "$INTENTIONAL_VERSION" ]]; then aliases="$aliases $minor"; fi
  if [[ "$major" != 0 ]]; then
    newest_major=$(printf '%s\n' "$published_tags" \
      | grep -E "^${major_pattern}\.[0-9]+\.[0-9]+$" | sort -V | tail -n1 || true)
    if [[ "$newest_major" == "$INTENTIONAL_VERSION" ]]; then aliases="$aliases $major"; fi
  fi
  for alias in latest "$minor" "$major"; do
    alias_digest=$(crane digest "$repository:$alias" 2>/dev/null || true)
    entitled=""
    case " $aliases " in *" $alias "*) entitled=yes ;; esac
    if [[ "$alias_digest" == "$published" ]]; then
      test -n "$entitled"
      printf -- '  - name: "%s"\n    digest: "%s"\n' "$alias" "$alias_digest" >> "$aliases_file"
    else
      test -z "$entitled"
    fi
  done
else
  packaged="sha256:$(sha256sum "$INTENTIONAL_SUBJECT/devcontainer-feature-$INTENTIONAL_SUBJECT_IDENTITY.tgz" | cut -d' ' -f1)"
  layer=$(printf '%s' "$index" | jq -r '.layers[0].digest')
  if [[ "$layer" != "$packaged" ]]; then
    observe_state conflict \
      "$repository already holds $published under version $INTENTIONAL_VERSION, which is not the subject this release built"
    exit 0
  fi
  core=${INTENTIONAL_VERSION%%-*}
  major=${core%%.*}
  minor=${core%.*}
  stable=""
  case "$INTENTIONAL_VERSION" in *-*) ;; *) stable=yes ;; esac
  alias_error="$INTENTIONAL_WORK/feature-alias-error"
  for alias in latest "$minor" "$major"; do
    if alias_digest=$(crane digest "$repository:$alias" 2>"$alias_error"); then
      if [[ "$alias_digest" == "$published" ]]; then
        printf -- '  - name: "%s"\n    digest: "%s"\n' "$alias" "$alias_digest" >> "$aliases_file"
      elif [[ -n "$stable" ]]; then
        printf 'Required stable Feature alias %s resolves %s instead of %s\n' \
          "$alias" "$alias_digest" "$published" >&2
        exit 1
      fi
    elif grep -Eq '(^|[^A-Z_])MANIFEST_UNKNOWN([^A-Z_]|$)' "$alias_error"; then
      if [[ -n "$stable" ]]; then
        printf 'Required stable Feature alias %s is missing after publication\n' "$alias" >&2
        exit 1
      fi
    else
      printf 'Could not read Feature alias %s after publication: ' "$alias" >&2
      cat "$alias_error" >&2
      exit 1
    fi
  done
fi

if [[ " $INPUT_COMPONENTS " == *" sbom "* || " $INPUT_COMPONENTS " == *" provenance "* ]]; then
  attestations_file="$INTENTIONAL_WORK/attestations.jsonl"
  : > "$attestations_file"
  while IFS= read -r platform_entry; do
    platform_digest=$(jq -r '.digest' <<<"$platform_entry")
    platform=$(jq -r '.platform' <<<"$platform_entry")
    attestation=$(jq -r --arg digest "$platform_digest" \
      'first(.manifests[]? | select(.annotations["vnd.docker.reference.type"] == "attestation-manifest" and .annotations["vnd.docker.reference.digest"] == $digest) | .digest) // ""' <<<"$index")
    test -n "$attestation"
    predicates=$(crane manifest "$repository@$attestation")
    sbom_digest=$(jq -r 'first(.layers[]? | select(.annotations["in-toto.io/predicate-type"] | test("spdx")) | .digest) // ""' <<<"$predicates")
    provenance_digest=$(jq -r 'first(.layers[]? | select(.annotations["in-toto.io/predicate-type"] | test("slsa|provenance")) | .digest) // ""' <<<"$predicates")
    jq -cn --arg platform "$platform" --arg attestation "$attestation" \
      --arg sbom "$sbom_digest" --arg provenance "$provenance_digest" \
      '{platform: $platform, attestation: $attestation, sbom: $sbom, provenance: $provenance}' \
      >> "$attestations_file"
  done < <(jq -c '.manifests[] | select(.annotations["vnd.docker.reference.type"] != "attestation-manifest") | {digest, platform: (.platform.os + "/" + .platform.architecture)}' <<<"$index")
  test -s "$attestations_file"
fi

for component in $INPUT_COMPONENTS; do
  component_recorded=""
  case "$component" in
    sbom|provenance)
      while IFS= read -r attestation_entry; do
        component_digest=$(jq -r ".$component" <<<"$attestation_entry")
        component_reference="$repository@$(jq -r '.attestation' <<<"$attestation_entry")"
        test -n "$component_digest"
        printf -- '  - kind: "%s"\n    digest: "%s"\n    reference: "%s"\n' \
          "$component" "$component_digest" "$component_reference" >> "$metadata_file"
        if [[ "$component" == provenance ]]; then
          printf -- '  - kind: "oci-attestation"\n    digest: "%s"\n    reference: "%s"\n' \
            "$component_digest" "$component_reference" >> "$provenance_file"
        fi
        component_recorded=yes
      done < "$attestations_file"
      ;;
    signature)
      component_digest=$(crane digest "$(cosign triangulate "$repository@$published")")
      ;;
    *) component_digest="" ;;
  esac
  test -n "$component_digest"
  if [[ -z "$component_recorded" ]]; then
    printf -- '  - kind: "%s"\n    digest: "%s"\n    reference: "%s"\n' \
      "$component" "$component_digest" "$repository@$published" >> "$metadata_file"
  fi
done

rm -rf "$clean_client"
mkdir -p "$clean_client"
if ! retrieved=$(crane digest "$repository:$INTENTIONAL_VERSION" 2>/dev/null) \
  || ! crane manifest "$repository@$retrieved" \
    > "$clean_client/subject.json" 2>/dev/null; then
  printf '%s carries version %s but no public client can retrieve it; an OCI package is observable to its consumers only while it is public, and a package is private when it is first pushed\n' \
    "$repository" "$INTENTIONAL_VERSION" >&2
  observe_state pending
  exit 0
fi
test "$retrieved" = "$published"
test "sha256:$(sha256sum < "$clean_client/subject.json" | cut -d' ' -f1)" = "$retrieved"
INTENTIONAL_DESTINATION_DIGEST=$published
INTENTIONAL_RETRIEVED_DIGEST=$retrieved
INTENTIONAL_RETRIEVAL_VERSION=$(crane version)
if [[ "$INTENTIONAL_PACKAGER_ID" == buildx ]]; then
  INTENTIONAL_PACKAGER_VERSION=$(docker buildx version | head -n1)
else
  INTENTIONAL_PACKAGER_VERSION=$(devcontainer version | head -n1)
fi
observe_present
if [[ -s "$metadata_file" ]]; then
  printf 'attached-metadata:\n' >> "$INTENTIONAL_OBSERVATION"
  cat "$metadata_file" >> "$INTENTIONAL_OBSERVATION"
fi
if [[ -s "$aliases_file" ]]; then
  printf 'destination-aliases:\n' >> "$INTENTIONAL_OBSERVATION"
  cat "$aliases_file" >> "$INTENTIONAL_OBSERVATION"
fi
if [[ -s "$provenance_file" ]]; then
  printf 'build-provenance:\n' >> "$INTENTIONAL_OBSERVATION"
  cat "$provenance_file" >> "$INTENTIONAL_OBSERVATION"
fi
