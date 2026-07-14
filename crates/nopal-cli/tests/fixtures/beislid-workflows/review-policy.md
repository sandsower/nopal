# Beislið workflow config - sanitized review-risk dogfood

This fixture mirrors the `review_policy`/`split_policy`/gate-metadata fences
from Beislið's own `.beislid/workflow.md`.
The importer must preserve `review_policy` and `split_policy`.

```beislid:review_policy
agentic_reviewer:
  mode: opt_in_final_review
  provider: coderabbit
  label: coderabbit-ready
  description_keyword: coderabbit:review
risk:
  max_auto_closeout_risk: low
  high_risk_paths:
    - '**/.github/workflows/**'
    - 'bin/**'
  low_risk_paths:
    - 'docs/**'
    - '**/*.md'
  high_risk_file_count: 12
  high_risk_total_changes: 500
  low_risk_file_count: 3
  low_risk_total_changes: 120
```

```beislid:split_policy
exclusive
```

```beislid:gates
- name: fmt
  command: 'cargo fmt --all --check'
  parallel_safe: true
  mutates: false
- name: docs-lint
  command: 'markdownlint .'
```
