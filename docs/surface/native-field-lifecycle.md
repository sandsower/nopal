# Native Field lifecycle and ownership

The native Field treats ownership as explicit product state rather than inferring it from process ancestry, names, or paths.
Core remains authoritative for Plot and Session facts.
Structured Session history remains authoritative for semantic output.
The desktop owns presentation state and only the resources it creates for that presentation.

## Resource ownership matrix

| Resource | Authority and ownership | Creation | Ordinary retirement | Crash recovery | Forbidden behavior |
| --- | --- | --- | --- | --- | --- |
| Core Field snapshot | Borrowed immutable fact owned by Core | Loaded through the bounded `nopal field inspect --json` contract | Replaced only by an accepted refresh generation | Last accepted snapshot remains visible after a refresh failure | The desktop must not invent, mutate, or persist Core facts |
| Selected Plot and Session preference | Desktop-owned UI intent scoped to the native instance | Written only after an exact current Core selection is activated | Cleared explicitly or replaced atomically | Malformed or future data is preserved and ignored | A preference must never override current Core identity |
| Singleton lock | Native application authority owned by the primary process | Acquired before Core, restore, host, feed, Composer, or Terminal construction | Released last after the host and activation service stop | The operating-system lock remains authoritative after stale files or PID reuse | A PID file must never establish primary authority |
| Activation endpoint | App-owned local IPC bound to the singleton scope | Created only by the primary lock holder | Stops accepting requests before host teardown | A later lock holder may reconcile the exact stale endpoint | A secondary must never remove a live primary endpoint |
| Native Field window | Renderer-host-owned presentation resource | Created at most once by the primary host | Closed or retained according to platform resident-app policy | Reopened by a completed activation action | Duplicate launches must not create another window |
| Structured Session binding | App-owned attachment to a borrowed exact Session | Created for the explicitly activated Session | Cancel producer, close connection, and retain failed cleanup authority | Next launch reconciles only app-owned external artifacts | Cleanup must never terminate the borrowed Session host |
| Terminal observer and transport | App-owned lazy attachment to a borrowed exact Session pane and process | Created only for explicit Terminal intent or forced degradation | Close observer and transport after stopping input and output producers | Durable recovery records only exact app-created observer artifacts | Terminal bytes must never become semantic Session history |
| Pi process, tmux Session, and pane | Borrowed Session host identity owned outside the desktop | Never created by presentation switching | Survives window close, app quit, retarget, and desktop crash | Observed for exact identity only | Desktop cleanup must never kill or register these borrowed resources for deletion |
| Composer draft store | App-owned renderer-neutral presentation state | Created once per native host and keyed by exact Plot and Session | Prunes only targets no longer present in accepted Core facts | In-memory drafts do not imply durable Session history | Lifecycle bindings, Terminal, and Session runtime must not own a second draft copy |
| Field refresh worker | App-owned bounded producer | Started by the primary host after startup composition | Cancel, stop publication, and join before consumers are dropped | A new host starts a new generation and ignores stale results | A refresh must never retarget the live Session or overwrite a newer generation |
| Activity and assurance projection | App-owned derived state over verified Session events and Core facts | Recomputed from one accepted source generation | Reconciles exact keys without changing the live binding | Last verified prefix and last accepted Core projection remain visible | Display text and Terminal output must never create assurance facts |
| App-owned ephemeral Session process | App-owned only when a future flow explicitly establishes that ownership | Must register durable cleanup authority before becoming live | Transfers to a durable owner or is terminated on quit | Next launch uses the exact recorded identity and cleanup recipe | Process names, ancestry, or coincidental IDs must never authorize termination |
| Recovery journal | App-owned durable cleanup authority | Records staged external resources before live activation | Retires an entry only after exact cleanup succeeds | Replays bounded exact cleanup before Core and host construction | Borrowed resources and unverified identities must never enter the journal |

## Selection and interaction boundaries

The live Session binding, visible workspace subject, selected activity row, and inspector selection are distinct identities.
Showing a Plot, execution, activity, approval, gate, evidence item, or diagnostic changes presentation state without replacing the live Session binding.
Only an explicit Session activation may construct a replacement Session binding.
The replacement must validate against the current accepted Core generation before construction.
The old binding remains owned until the replacement is ready.
Selection intent is persisted only after the replacement succeeds.
Inspection must preserve the exact Session identity, structured cursor, requested and effective presentation mode, lazy Terminal binding, and Composer draft.

## Lifecycle order

### Primary startup

The primary acquires the singleton lease before reading restore intent or Core state.
It reconciles app-owned crash artifacts before loading Core.
It loads one bounded Core Field snapshot and resolves restore intent exactly against that snapshot.
It creates the renderer-neutral host state before creating the first structured binding.
It binds only the exact restored Session and does not create Terminal at healthy startup.

### Secondary startup

A secondary sends one bounded activation request and waits for a completed focus or reopen acknowledgement.
It does not read Core, restore preferences, start workers, create a Composer, bind Output, attach Terminal, or construct a window.

### Refresh

Issuing generation N fences every older result immediately.
The worker loads data only and never mutates UI state.
The host prepares all projections and reconciliation before committing the candidate snapshot.
A stale result or projection failure leaves the accepted snapshot, view state, live binding, activity selection, inspector state, and Composer state unchanged.

### Explicit Session activation

The host validates the requested Session against the current accepted Core generation.
It constructs a new structured binding without giving lifecycle code access to Composer drafts.
If construction fails, the old binding and preference remain unchanged.
If construction succeeds, the host retires the old app-owned attachments, commits the new live identity, and persists that exact selection.

### Inspection and navigation

Sidebar navigation and main-panel inspection never invoke Session activation implicitly.
Execution and assurance details may replace the visible center subject while the live Session remains attached.
If an exact inspected key disappears, reconciliation closes it or selects the documented deterministic fallback within the same section.
That reconciliation never changes Plot, Session, Composer, Output, or Terminal ownership.

### Shutdown

The host stops accepting UI and activation work before teardown.
It cancels producers before consumers, joins workers, closes app-owned Terminal and structured bindings, and retains exact cleanup authority for failures.
It destroys the renderer host before releasing the singleton lease.
Borrowed Session hosts survive every desktop shutdown path unless a separate explicit Session-termination command owns that decision.
