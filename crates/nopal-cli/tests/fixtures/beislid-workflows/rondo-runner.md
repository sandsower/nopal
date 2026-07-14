# Beislið workflow config - sanitized Rondo runner

This fixture models the local approved-slice runner surface: file-backed slice intake, Rondo AFK policy, lifecycle checkpoints, and deterministic proof gates.

```beislid:ticket_source
type: file
file_glob: '.beislid/exports/**/*.json'
id_pattern: '^[A-Z]+-\d+$'
```

```beislid:ticket_update
type: cli
comment_command: 'rondo ticket comment --id {id} --body-file {body_file}'
issue_command: 'rondo ticket update --id {id} --json-file {json_file}'
```

```beislid:pr_review_source
type: paste
```

```beislid:pr_review_update
type: manual
```

```beislid:gates
- name: fmt
  command: 'cargo fmt --all --check'
  autofix: 'cargo fmt --all'
- name: clippy
  command: 'cargo clippy --workspace --all-targets -- -D warnings'
- name: test
  command: 'cargo test --workspace'
- name: migration_bridge_proof
  command: 'cargo test -p nopal-cli migration_bridge_proof -- --nocapture'
```

```beislid:action_policy
modes:
  rondo-afk:
    rules:
      network-read: deny
      workspace-write: allow
      git-local: allow
      git-remote: deny
      destructive: deny
    actions:
      gate.*: allow
      git.push: deny
      pr.create: deny
```

```beislid:lifecycle_actions
kickoff_start:
  actions:
    - id: ledger-init
      type: cli
      command: 'nopal ledger init --skill kickoff'
      approval: auto
implementation_plan_created:
  actions:
    - id: plan-checkpoint
      type: artifact
      path: 'plans/implementation.md'
ready_for_review_pre_submit:
  actions:
    - id: proof-summary
      type: artifact
      path: 'docs/proof-summary.md'
```

```beislid:probe_cache
ttl_hours: 12
```
