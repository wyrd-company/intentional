# Intentional repository context

Intentional computes release versions from declared intent, materializes those
versions across package ecosystems, seals canonical release plans, and creates
verifiable annotated release records.

## Validation gates

Run `task ci` and `intentional check` as separate local gates. Docker must be
available because `task ci` executes the Cargo/Homebrew compatibility gate.
When work is based on a branch with an open GitHub pull request, also run
`task hosted:check PR=<pull-request-number>`. A red hosted check must be
diagnosed and assigned before later work treats that branch as a valid base.
Run `task pinned-gnu:test` when changing release execution, packaging recipes,
emitted workflow bodies, or Linux GNU validation. It executes all workspace
targets and doctests through the pinned Cross image and mounted test-runtime
paths. Hosted Linux GNU evidence runs the all-target leg only. The local gate
adds doctests so pre-handoff validation covers them without duplicating the
costly Cross leg in every hosted run. Both gates provision actionlint, jq,
shellcheck, and python3, and run under Git 2.7.4 and Bash 4.3.48.

## Domains

Wyrd Company registers and controls `intentional.foo`. It is the host of every
Intentional schema `$id` and `$schema` value, and it grounds the fixed Git
identity `Intentional <releases@intentional.foo>` that deterministic release
commits are authored and committed by.
