# Contributor and agent guidance

- Explain behavior, invariants, and tradeoffs directly in source comments.
- Do not use ticket numbers, pull request numbers, or private tracker shorthand as a substitute for an explanation.
- Keep local paths, private integrations, execution artifacts, and credentials out of the repository.
- Record durable architectural decisions under `docs/adr/`.
- Keep temporary implementation plans and agent workflow state outside the public source tree.
- Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `npm test` before proposing a change.

## Agent skills

This repo uses [Beislið](https://github.com/sandsower/beislid) for orchestrator skills.

- Read `.beislid/workflow.md` first.
- Existing ticket or branch -> `kickoff`
- Clear requirements, implementation still undecided -> `blueprint`
- Work is done but not yet proven -> `verify`
- Branch is ready for PR -> `ready-for-review`
- Use direct skill invocation when the right entry point is already obvious.
- Run `/setup` when the repo workflow config is missing or needs updating.

- Project config: `.beislid/workflow.md`
- Audit setup: `/doctor`
- Configure: `/setup`
