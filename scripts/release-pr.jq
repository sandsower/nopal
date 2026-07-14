[
  .[]
  | select(
      .head.ref == env.RELEASE_BRANCH
      and .base.ref == "main"
      and .merged_at != null
    )
]
| first
| .number // empty
