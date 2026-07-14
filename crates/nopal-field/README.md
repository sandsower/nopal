# nopal-field

The Nopal Field is a tmux-backed terminal interface.
Seats are real tmux panes rendered by the outer terminal; the field is a ratatui app living in its own tmux pane; a `tmux -C` control-mode sidecar supplies field state for the whole server.
The field renders and routes, but never decides: every fact comes from tmux, `nopal field`, `nopal ask`, or the `rondo.core/v1` feed, and every decision-shaped question such as placement or ask semantics goes to Nopal Core.

## Using it

```
nopal field native          # require the separately installed native Field sibling
nopal field native --state-dir <p>
nopal field legacy          # explicit tmux-backed compatibility fallback
nopal field                 # attach-or-create the "nopal" session and take the terminal
nopal field --session work  # separate field session name
nopal field --all           # start with every tmux session shown, not just nopal-managed ones
nopal field --rondo-run rondo:RUN-abc123   # also tail a rondo run's event feed directly
nopal field bench           # run the scriptable feel benchmarks (see BENCHMARKS.md)
```

The live Field is Plot-first.
A compact Plot rail stays on the left, and the selected Plot's interactive Sessions and unattended executions appear as sibling activity tabs in the dominant center panel.
A live Session keeps its embedded terminal, an unavailable Session says so without closing the stage, and an execution renders durable read-only status, outcome, cursor, manifest, and timestamp facts.
The Plot inspector has separate Overview, Roots, Evidence, and Fruit tabs on wide terminals.
Evidence keeps its service, Repository, and run provenance with the opaque Rondo-owned URI, while Fruit remains explicitly absent unless a future authorized decision changes it.
The inspector hides automatically before the activity panel becomes cramped.
Press `z` while the Plot stage is open to hide or show the inspector manually.

On a clean first interactive open, Nopal Core creates one durable Provisional Plot and the Field starts one ordinary Nopal/Pi Session for it.
Reopening the Field resumes the same Plot and Session identity instead of creating duplicates.
The Provisional Plot does not require a Repository or Workspace.
Later Establishment work binds those through an authoritative configured seam.
The field is multi-session and **scoped by default to sessions nopal opened or adopted** (see "Nopal-scoped seats" below); the sidecar still observes the whole server, but the sidebar hides unmanaged sessions until you ask for them.

Keys:

- `j`/`k` (or arrows) while the Plot stage has navigation focus: move the Plot selection.
- `Tab`/`Shift+Tab` while the Plot stage has navigation focus: move between Session and execution siblings.
- `z` while the Plot stage is open: hide or show the Plot inspector.
- `Enter` on a seat: open it **live in the main panel** (embedded view, see below); on a run: open its event/evidence detail; on an ask: show its full context.
- `f`: full-focus the selected seat's real pane - the zero-overhead flagship path (same-session: `swap-pane` into the slot; other session: switch the terminal client there).
- `i` / `Ctrl-o` (live Session activity only): take / release seat input focus.
- `esc` / `q` (Plot stage or legacy embedded view): close the panel and return to the sidebar.
- `a`: jump to the ask queue; `y`/`d`: approve/deny the selected ask (routed to `nopal ask resolve`).
- `/`: filter (subsequence match); `n`: spawn picker (type to filter recents/projects/worktrees, enter to spawn or create a worktree).
- `x`: kill the selected seat (`y` confirms, anything else cancels); `s`: (re)launch the agent in the selected seat's pane.
- `A`: toggle showing all sessions vs nopal-managed only; `G`: adopt the selected unmanaged session into nopal.
- `r`: reconcile; `p`: profiling counters; `?`: help overlay (every key with a one-line description, always showing the *effective* binding); `q`: quit the UI (the tmux session and seats survive).

Every binding above is a default, not a rule.
A `keys` section in `<state-dir>/field/config.json` (sibling of `projects` and `worktrees`, see `crate::seat::config`) remaps any binding by action name.
`crate::keys` contains the full action inventory, key-spec grammar, and validation rules.
An invalid remap reports itself once in the status line at startup and keeps the default rather than failing the field.

Moving into a full-focused seat pane is tmux-native (`prefix + arrow`, or mouse); the field never intercepts seat input on that path.
Coming back from a full-focused foreign-session seat is the operator's own session switcher (`sesh last` / `prefix-L`), by design.
Detach with `prefix d`; `nopal field` reattaches with everything alive.

## Design decisions (v1)

**Crate shape.**
The `crates/nopal-field` workspace crate remains the legacy tmux implementation exposed through bare `nopal field` and explicit `nopal field legacy` until the native desktop route is ready for adoption.
The explicit `nopal field native` route is reserved for a separately packaged `nopal-field-native` GUI process and never falls back to the legacy surface silently.
The sibling is not included in release archives while renderer selection and packaging remain unfinished.
The entrypoint is a plain `cli::run(&FieldArgs)`, deliberately free of subcommand coupling so a later slice can make the field the default surface (bare `nopal`) by routing to the same function.
As groundwork for that, the launcher refuses to start without a tty on stdin/stdout instead of blindly starting a TUI.

**Concurrency: threads + `std::sync::mpsc`, no async runtime.**
The workload is a handful of blocking producers (sidecar stdout reader, feed poller threads, crossterm input) funneling into one consumer loop; tokio would add a dependency tree and an executor for no structural gain.
The loop drains bursts and renders at most every 33ms.

**State feed: control-mode subscriptions plus server snapshots, never scraping.**
The sidecar attaches `tmux -C attach-session -f no-output` and installs one format subscription over the attached session's panes:
`refresh-client -B nopal-seats:%*:"#{pane_id}|#{window_id}|#{window_name}|#{pane_current_command}|#{@nopal_seat}|#{@nopal_repo}|#{@nopal_role}|#{@nopal_managed}|#{pane_dead}|#{session_id}|#{session_name}|#{pane_active}|#{window_active}|#{@nopal_plot}|#{@nopal_plot_session}|#{pane_current_path}"`.
tmux scopes `%*` subscriptions to the attached session (verified on 3.6a), but the same control client does receive server-wide `%unlinked-window-add/renamed/close` and `%sessions-changed` notifications; those - plus a coarse 5s timer - trigger a full-server snapshot (`list-panes -a -F <same format>`) through the same client, whose reply replaces the seat inventory.
Push covers the local session at sub-millisecond latency; snapshots keep the rest of the field honest.
Two further tmux 3.6a quirks found empirically: pane user-option changes after window creation do not re-fire subscriptions (so window-add also triggers a reconcile), and `set-option -t` rejects the `=exact` session prefix other commands accept.

**Seat model: one seat per session across the server; swap-pane focus locally.**
The field lives in many per-project sessions (sesh convention: session name = project dir basename), so foreign sessions each surface as one seat - their active pane - and focusing one switches the operator's terminal client(s) there by session/pane id, never by name (slash-named sessions are safe).
Only non-control clients attached to the field's session are switched; the sidecar is never moved.
Within the field's own session, seats are windows: the field window holds the sidebar pane (tagged `@nopal_role=field`) plus one slot pane, and focusing is `swap-pane` + `select-pane`.
Vacated windows are renamed after their new occupant only when the field itself named them (`seat:*`); adopted windows keep the user's names because user window-status formats display them.
Because every seat is a real pane, PTY passthrough, mouse, alt-screen, and extended keys are tmux-native; the field is not in the input or output path (see BENCHMARKS.md).

**Nopal-scoped seats: the default view is only nopal-opened sessions.**
The operator's tmux server carries their own sessions (`teotl`, `0`, ad-hoc sesh sessions); the field is a field surface, not a session switcher, so the SEATS list defaults to sessions nopal opened or adopted.
Ownership is a session-scoped tmux user option `@nopal_managed 1`, stamped at spawn time (and on the field's own session); tmux resolves session options for every pane in the session, so one `set-option` marks the whole session and the existing subscription format carries `#{@nopal_managed}` per pane at no extra cost.
The sidebar filters the display to marker-bearing sessions; the sidecar still observes every session server-wide, because both the event feed and the filter need the full inventory - only the display is scoped.
Escape hatch: `A` (or `--all`) reveals unmanaged sessions, and `g` **adopts** the selected one (stamps the marker, bringing it into nopal).
AFK RUNS are ledger/field-fed and completely independent of this scoping.

Restore safety: tmux user options - session-scoped ones included - do **not** survive a resurrect/continuum restore (the same fact the field already relies on for pane options), so the marker alone is not durable.
A small JSON registry under the nopal state dir (`field/managed-seats.json`) records session name -> seat metadata at spawn/adopt time; on every launch the field re-applies the marker to each still-live listed session, healing a restore.
The marker stays the fast per-frame filter input; the registry is the durable source of truth behind it.

**Embedded seat view: a herdr-class VT mirror in the main panel.**
`Enter` on a seat opens it live inside the field, not by switching the client away.
This adds a VT display layer for the embedded panel only; the tmux backend, seat model, and sidecar are unchanged.
The seat's live output is backfilled with `capture-pane -e -p` (colors/attrs preserved) and then streamed through a **per-pane** `pipe-pane -O` fifo - deliberately off the `-f no-output` state sidecar, so the output firehose never touches the control channel.
Bytes are parsed by `alacritty_terminal` (Apache-2.0) into a `Term` grid that the UI renders to ratatui cells (foreground, background, attributes, and cursor).
The **technique** is ported from herdr, never its AGPL source.
While the panel holds input focus (`i`), keys are re-encoded to the exact bytes a terminal would send and delivered with `send-keys -H` (`Ctrl-o` releases focus so every other key, Esc and Ctrl-C included, reaches the seat verbatim).
Geometry is safe by construction: the grid is sized to the seat's reported width/height and clipped/centered into the panel, and the field only zooms its **own** pane for room - the seat's real session is never resized (verified against a seat attached elsewhere: its `pane_width`/`pane_height` are untouched).
The cursor is rendered from the VT grid's cursor state, not toggled per frame (herdr's flicker class), and repaint is ratatui's own cell diff.
`f` promotes to the zero-overhead flagship full-focus path.
Scope cuts and the honest echo-latency number are in BENCHMARKS.md.

**Nopal owns the seat lifecycle natively: spawn, kill, and agent launch or relaunch are field code, not delegated to a third-party session manager.**
`n` opens a spawn picker fed by recents (the managed-seats registry), configured/scanned project roots, and their `git worktree`s (`crates/nopal-field/src/seat/`); typing narrows it, Enter spawns the selection or - for a `+ new worktree in <project>` row - collects a name and runs `git worktree add` before spawning.
A query matching nothing that starts with `/` or `~` still spawns at that literal path, the v1 free-entry fallback.
Session naming stays basename-compatible (`seat/naming.rs`), disambiguating on collision with `<base>@<parent-dir>` then a numeric suffix, so third-party tools (sesh, fzf workflows) still recognize nopal seats even though nothing here delegates to them; a best-effort `zoxide add` on spawn feeds the operator's frecency store.
`x` kills the selected seat behind a `y`/`n` confirmation (whole-session seats via `kill-session`, in-field window seats via `kill-pane`); `s` (re)launches the agent in a seat whose pane isn't already running one.
Every spawned or adopted session is stamped `@nopal_managed` and recorded in the registry (now including its path) so it appears in the default scope, reappears as a recent candidate, and survives a restore.

**Config non-interference and restore safety.**
The field sets no global tmux options, installs no keybinds, and writes pane user options only on panes it owns; the operator's status-bar position, pane-border-status, mouse, and resurrect/continuum setup pass through untouched.
`nopal field` is idempotently relaunchable: if the session exists it revives a dead field pane (the state resurrect restores leave behind) via `respawn-pane` and re-tags it, rebuilding the field window if it vanished entirely; the UI also re-tags its own pane on every start, so lost tags self-heal.
`detach-on-destroy off` is compatible: the sidecar tracks the field pane by pane id, session switches only produce `%session-changed` notifications it tolerates, and cross-session switches never target it.

**Feeds: one composed field query, adapters behind a trait.**
`nopal field inspect --json` (`nopal.field/v1`) is the primary fact source: per-run placement, repository-tagged worktrees grouped under their parent repository, ledger status, latest gate attempts, pending asks, and Rondo facts when a feed is attached via `--rondo-events`.
Ask resolution routes to `nopal ask resolve` (`nopal.ask/v1`).
A direct `rondo.core/v1` tail (`mix rondo.run_events`, run through mise when present) exists for per-event streaming of explicitly registered runs (`--rondo-run repo:run`), polling coarsely because each poll boots a BEAM VM.
Every adapter implements the `Feed` trait and degrades to an "unavailable" badge when its source is absent; swapping or adding a source is one adapter.
AFK runs render exclusively from these structured feeds - events, gates, evidence pointers - never from log tails or screen scraping.

**Placement is reported, not decided.**
Seat spawn asks `nopal placement --json` and shows the core's placement and source in the status line; the field performs no placement logic of its own.

## Scope cuts (v1, deliberate)

- Plan cart (fifth surface): deferred; it is routing-only by spec and fits the existing dispatch seams (`send-keys`, `run.submit`) without architectural change.
- Foreign-session seat state is snapshot-fresh (<= 5s), not push-fresh; a persistent transport or per-session control clients are the upgrade if that ever matters.
- Ask detail pane: asks show action/repo/session in the bar and full reason via `Enter`; a dedicated queue pane with evidence rendering is v1.1.
- In-field peek at a seat without switching sessions **is** built (the embedded VT view, `Enter`); its deliberate cuts (no scrollback, no mouse passthrough, 256/RGB color only, no post-attach reflow) are listed in BENCHMARKS.md.
- tmux config polish (2026 deferral, escape-time, extended-keys defaults) is left to the operator's tmux.conf rather than imposed.

## v1.1 candidates

Plan cart; embedded-view scrollback + mouse passthrough + post-attach reflow; `nopal field` push transport or persistent rondo transport; ask evidence pane; bare-`nopal` default-surface remap; mouse support in the sidebar and spawn picker.
