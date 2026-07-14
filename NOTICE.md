# Notices

This project includes or adapts ideas/code from:

## ifiokjr/oh-pi (`@ifi/oh-pi-extensions`)

- Repository: https://github.com/ifiokjr/oh-pi/tree/main/packages/extensions
- License: MIT

`extensions/usage-tracker/` vendors and adapts the usage tracker from this package, narrowed to usage and quota tracking only, with GitHub Copilot quota probing added on top.
It was previously ported through `sandsower/pi-extensions` (MIT, archived); see that repository's `LICENSE-notes.md` for the original adaptation notes.

## nicobailon/pi-subagents

- Repository: https://github.com/nicobailon/pi-subagents
- License: MIT

`extensions/subagent-runner/` adapts selected runner-hardening utilities and implementation ideas from `pi-subagents` v0.23.0: child-boundary runtime behavior, Pi spawn argument construction, atomic JSON writes, and post-exit stdio cleanup.
Agents, roles, chains, manager UI, intercom, worktrees, and upstream slash-command surfaces were not adopted.
It was previously ported through `sandsower/pi-extensions` (MIT, archived); see that repository's `LICENSE-notes.md` for the original adaptation notes.

## Rondo

- Repository: https://github.com/sandsower/rondo
- License: Apache-2.0

Release archives bundle a version-pinned Rondo executable.
The archive includes its complete license and notice as `Rondo-LICENSE` and `Rondo-NOTICE`.

## Rust dependencies

Release archives include `THIRD_PARTY_LICENSES.html`, generated from the locked Cargo dependency graph with `cargo-about`.
