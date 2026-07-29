# Contributing to Nopal

Thank you for helping improve Nopal.

## Before opening a change

Open an issue for behavior changes that affect public contracts, command semantics, persisted state, or release packaging.
Small fixes, tests, and documentation improvements can go directly to a pull request.
Keep temporary plans, local agent state, credentials, and private integration details outside the repository.
Explain behavior and invariants directly instead of using issue or pull request numbers as shorthand in source comments.

## Development setup

Install the Rust toolchain pinned by the workflows and Node.js 24 or newer.
The adapter tests use only Node's built-in test runner and require no installed JavaScript dependencies.

Build and test the Rust workspace:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Run the extension, public-tree, and release-contract checks:

```sh
npm test
scripts/check-public-tree.sh
scripts/check-active-tree-identity.sh
sh scripts/test-release-contracts.sh
git diff --check
```

## Pull requests

Keep each pull request focused on one coherent outcome.
Add or update tests for every behavior change.
Document compatibility effects when changing a versioned contract, command envelope, configuration schema, or persisted format.
Do not edit generated release artifacts by hand.
All required checks must pass before merge.

By contributing, you agree that your contribution is licensed under the repository's MIT License.
