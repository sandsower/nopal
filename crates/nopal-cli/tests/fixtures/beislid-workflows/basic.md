# Beislið workflow

```beislid:ticket_source
type: mcp
tool: mcp__linear__get_issue
id_pattern: '^[A-Z]+-\d+$'
link_template: 'https://linear.app/acme/issue/{id}'
```

```beislid:ticket_update
type: mcp
comment_tool: mcp__linear__save_comment
issue_tool: mcp__linear__save_issue
```

```beislid:pr_review_source
type: cli
summary_command: 'gh pr view --json url,number'
threads_command: 'gh api repos/{owner}/{repo}/pulls/{number}/comments'
```

```beislid:pr_review_update
type: cli
reply_command: 'gh api repos/{owner}/{repo}/pulls/{number}/comments --method POST --input {json_file}'
```

```beislid:gates
- name: fmt
  command: 'cargo fmt --all --check'
  autofix: 'cargo fmt --all'
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
    actions:
      gate.*: allow
      git.push: deny
```

```beislid:probe_cache
ttl_hours: 24
```

```beislid:branch_pattern
^[a-z]+/([a-z]+-\d+)$
```
