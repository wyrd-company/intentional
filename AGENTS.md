# Intentional repository context

Intentional computes release versions from declared intent, materializes those
versions across package ecosystems, seals canonical release plans, and creates
verifiable annotated release records.

## Validation gates

Run `task ci` and `intentional check` as separate local gates. When work is
based on a branch with an open GitHub pull request, also run
`task hosted:check PR=<pull-request-number>`. A red hosted check must be
diagnosed and assigned before later work treats that branch as a valid base.
Run `task pinned-gnu:test` when changing release execution, packaging recipes,
emitted workflow bodies, or Linux GNU validation. It executes all workspace
targets and doctests through the same pinned Cross image and mounted
test-runtime paths as hosted Linux GNU evidence. The gate provisions actionlint,
jq, shellcheck, and python3, and runs under Git 2.7.4 and Bash 4.3.48.

## Domains

Wyrd Company registers and controls `intentional.foo`. It is the host of every
Intentional schema `$id` and `$schema` value, and it grounds the fixed Git
identity `Intentional <releases@intentional.foo>` that deterministic release
commits are authored and committed by.
