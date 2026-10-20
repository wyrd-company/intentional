# ---
# relationships:
#   demonstrates: overview
#   implements: intent-driven-polyglot-release
# ---

Output docs/assets/demo.gif

Require bash
Require git
Require jq

Set Shell "bash"
Set FontSize 22
Set Width 1280
Set Height 800
Set Theme "Catppuccin Mocha"
Set Padding 24
Set Margin 12
Set MarginFill "#11111b"
Set BorderRadius 8
Set WindowBar "Colorful"
Set TypingSpeed 28ms

Hide
Type 'export PATH="$PWD/target/debug:$PATH"' Enter
Type 'export PS1="demo> "' Enter
Type 'export GIT_AUTHOR_NAME="Demo User" GIT_AUTHOR_EMAIL="demo@example.invalid"' Enter
Type 'export GIT_COMMITTER_NAME="Demo User" GIT_COMMITTER_EMAIL="demo@example.invalid"' Enter
Type 'rm -rf /tmp/intentional-vhs-demo && mkdir -p /tmp/intentional-vhs-demo/.intentional/intents && cd /tmp/intentional-vhs-demo' Enter
Type 'git init -q' Enter
Type 'printf "%s\n" "{\"name\":\"sample-application\",\"version\":\"1.2.3\"}" > package.json' Enter
Type 'printf "%s\n" "\$schema: https://intentional.foo/schemas/config.yml" "contract: contract-1" "settings:" "  internal-dependency-bump: patch" "  pre-1-0-bump-mapping: compatibility" "release-units:" "  sample-app:" "    path: ." "    projections:" "      - adapter: npm" "        file: package.json" "        mode: committed" "    tags:" "      primary:" "        role: primary" "        template: \"{version}\"" > .intentional/config.yml' Enter
Type 'git add . && git commit -q -m "Initialize sample application"' Enter
Type 'intentional tag --baseline >/dev/null' Enter
Type 'clear' Enter
Show

Sleep 500ms
Type 'intentional status' Enter
Sleep 3s

Type 'intentional add --release-unit sample-app:minor --message "Add options."' Enter
Sleep 3s

Type 'intentional status' Enter
Sleep 4s

Type 'intentional apply --dry-run' Enter
Sleep 4s

Type "intentional plan | jq '{digest, release_units: [.release_units[] | {id, old_version, new_version}]}'" Enter
Sleep 6s
