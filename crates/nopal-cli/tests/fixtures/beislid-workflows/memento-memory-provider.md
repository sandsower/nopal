# Beislið workflow config - sanitized Memento memory provider

This fixture models optional memory-provider handoff without making Beislið depend on Nopal or on a specific vault implementation.

```beislid:ticket_source
type: paste
```

```beislid:ticket_update
type: mcp
comment_tool: mcp__linear_personal__save_comment
```

```beislid:gates
- name: memory-context-preview
  command: 'memento session-context --dry-run'
- name: memory-no-secrets
  command: 'memento scan --redaction-check'
```

```beislid:action_policy
modes:
  supervised-auto:
    rules:
      network-read: ask
      workspace-write: allow
      git-remote: deny
      secret-bearing: ask
    actions:
      memento.search: allow
      memento.capture: ask
      memento.process: ask
```

```beislid:lifecycle_actions
kickoff_context_ready:
  actions:
    - id: memory-context-artifact
      type: artifact
      path: 'docs/memory-context.md'
spec_approved:
  actions:
    - id: memory-decision-note
      type: tracker
      approval: prompt
review_feedback_loaded:
  actions:
    - id: memory-review-artifact
      type: artifact
      path: 'docs/review-feedback.md'
```

```beislid:branch_pattern
^[a-z]+/([a-z]+-\d+)
```
