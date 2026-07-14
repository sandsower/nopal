# Contributor and agent guidance

- Explain behavior, invariants, and tradeoffs directly in source comments.
- Do not use ticket numbers, pull request numbers, or private tracker shorthand as a substitute for an explanation.
- Keep local paths, private integrations, execution artifacts, and credentials out of the repository.
- Record durable architectural decisions under `docs/adr/`.
- Keep temporary implementation plans and agent workflow state outside the public source tree.
- Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `npm test` before proposing a change.
