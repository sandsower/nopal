# Beislið workflow config - sanitized Nopal current

This fixture mirrors the repo-local `.beislid/workflow.md` shape without workspace-specific URLs or secrets.

```beislid:ticket_source
type: mcp
tool: mcp__linear_personal__get_issue
id_pattern: '^[A-Z]+-\d+$'
link_template: 'https://linear.example/nopal/issue/{id}'
```

```beislid:branch_pattern
^[a-z]+/([a-z]+-\d+)
```

```beislid:ticket_update
type: mcp
comment_tool: mcp__linear_personal__save_comment
issue_tool: mcp__linear_personal__save_issue
```

```beislid:pr_review_source
type: cli
summary_command: 'gh pr view --json url,number,reviewDecision,reviews,comments'
threads_command: 'gh api repos/{owner}/{repo}/pulls/{number}/comments'
```

```beislid:pr_review_update
type: cli
reply_command: 'gh api repos/{owner}/{repo}/pulls/{number}/comments --method POST --input {json_file}'
rerequest_command: 'gh api repos/{owner}/{repo}/pulls/{number}/requested_reviewers --method POST --input {json_file}'
```

```beislid:gates
- name: fmt
  command: 'cargo fmt --all --check'
  autofix: 'cargo fmt --all'
- name: clippy
  command: 'cargo clippy --workspace --all-targets -- -D warnings'
- name: test
  command: 'cargo test --workspace'
```

```beislid:action_policy
modes:
  supervised-auto:
    sandbox:
      on_uncommitted_changes: allow
    rules:
      network-read: allow
      workspace-write: allow
      git-local: allow
      git-remote: allow
    actions:
      gate.*: allow
      git.commit: allow
      git.push: allow
      pr.review.reply: allow
      pr.review.rerequest: allow
      ticket.comment: allow
      ticket.issue: allow
      memento.capture: allow
      retro.run: allow
```

```beislid:lifecycle_actions
events:
  break_spec_approved:
    actions:
      - name: write-structure-artifact
        type: artifact
        approval: auto
        path: 'plans/{feature}-structure.md'
        on_failure: prompt
```

```beislid:probe_cache
ttl_hours: 24
```
