#!/usr/bin/env bash
# ---

# Assignments feed helpers defined by the sourcing dispatcher.
# shellcheck disable=SC2034
# relationships:
#   implements: github-release-executor
# ---

digest_tree() {
  python3 - "$1" <<'PY'
import hashlib, os, stat, sys
root = os.fsencode(sys.argv[1])
members = []
for directory, _, names in os.walk(root, followlinks=False):
    for name in names:
        member = os.path.join(directory, name)
        if stat.S_ISREG(os.stat(member, follow_symlinks=False).st_mode):
            members.append(member)
members.sort(key=lambda member: os.path.relpath(member, root).split(os.sep.encode()))
if not members:
    raise SystemExit(1)
manifest = bytearray()
for member in members:
    relative = os.path.relpath(member, root).decode(errors='replace').encode()
    digest = hashlib.sha256(open(member, 'rb').read()).hexdigest().encode()
    manifest.extend(relative + b'\0sha256:' + digest + b'\n')
print('sha256:' + hashlib.sha256(manifest).hexdigest())
PY
}

test "$(digest_tree "$INTENTIONAL_SUBJECT")" = "$INTENTIONAL_SUBJECT_DIGEST"
mkdir -p "$(dirname "$INTENTIONAL_WORK")"

read_homebrew() {
  local generated="$INTENTIONAL_SUBJECT/homebrew"
  test -d "$generated"
  local formulas=()
  while IFS= read -r -d '' formula; do formulas+=("$formula"); done \
    < <(find "$generated" -type f -name '*.rb' -print0 | sort -z)
  test "${#formulas[@]}" -gt 0
  rm -rf "$INTENTIONAL_WORK"
  git clone --quiet --depth 1 \
    "https://x-access-token:${INPUT_REGISTRY_TOKEN}@github.com/${INTENTIONAL_DESTINATION}.git" \
    "$INTENTIONAL_WORK" || return 1
  for formula in "${formulas[@]}"; do
    relative=${formula#"$generated/"}
    cmp --silent "$formula" "$INTENTIONAL_WORK/$relative" || return 1
  done
}

read_aur() {
  local pkgbuild="$INTENTIONAL_SUBJECT/aur/$INTENTIONAL_DESTINATION.pkgbuild"
  local srcinfo="$INTENTIONAL_SUBJECT/aur/$INTENTIONAL_DESTINATION.srcinfo"
  test -f "$pkgbuild"
  test -f "$srcinfo"
  install -d -m 700 "$HOME/.ssh"
  printf '%s\n' "$INPUT_REGISTRY_TOKEN" > "$HOME/.ssh/intentional-observe-aur"
  chmod 600 "$HOME/.ssh/intentional-observe-aur"
  ssh-keyscan -t ed25519 aur.archlinux.org > "$INTENTIONAL_WORK-host-key"
  test "$(ssh-keygen -l -f "$INTENTIONAL_WORK-host-key" | cut -d' ' -f2)" \
    = "SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4"
  cat "$INTENTIONAL_WORK-host-key" >> "$HOME/.ssh/known_hosts"
  export GIT_SSH_COMMAND="ssh -i $HOME/.ssh/intentional-observe-aur -o IdentitiesOnly=yes"
  rm -rf "$INTENTIONAL_WORK"
  git clone --quiet "ssh://aur@aur.archlinux.org/${INTENTIONAL_DESTINATION}.git" \
    "$INTENTIONAL_WORK" || return 1
  cmp --silent "$pkgbuild" "$INTENTIONAL_WORK/PKGBUILD" || return 1
  cmp --silent "$srcinfo" "$INTENTIONAL_WORK/.SRCINFO" || return 1
}

INTENTIONAL_ELAPSED=0
while :; do
  if "read_$INTENTIONAL_PUBLISHER"; then break; fi
  if ! wait_again; then
    observe_state pending
    exit 0
  fi
done
rm -rf "$INTENTIONAL_WORK/.git"
INTENTIONAL_DESTINATION_DIGEST=$(digest_tree "$INTENTIONAL_WORK")
INTENTIONAL_RETRIEVED_DIGEST=$INTENTIONAL_DESTINATION_DIGEST
case "$INTENTIONAL_PACKAGER_ID" in
  goreleaser) INTENTIONAL_PACKAGER_VERSION=$(goreleaser --version | head -n1) ;;
  cargo-archive) INTENTIONAL_PACKAGER_VERSION=$(intentional --version | awk '{print $2}') ;;
  *) printf 'unsupported repository packager: %s\n' "$INTENTIONAL_PACKAGER_ID" >&2; exit 1 ;;
esac
INTENTIONAL_RETRIEVAL_VERSION=$(git --version | head -n1)
observe_present
