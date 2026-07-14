# Nopal desktop spike benchmarks

Run the terminal replay harness in release mode:

```sh
cargo run --release -p nopal-desktop-spike -- --terminal-benchmark
```

The harness feeds an eight MiB ANSI workload containing true color, cursor movement, Unicode wide characters, combining marks, and emoji through the same `TerminalSurface` used by the native application in 8,192-byte live-pipe-sized chunks.
Every chunk includes VT parsing, damage comparison, and immutable snapshot replacement.
The harness then measures cached immutable snapshot handoff over 1,000 samples.

## Session replay benchmark

Run the durable Session replay harness in release mode:

```sh
cargo run --release -p nopal-desktop-spike -- --session-replay-benchmark
```

The harness emits one `nopal.desktop-session-replay-benchmark/v1` JSON report.
It runs a representative 10,000-event history and the 100,000-event maximum-bound history through the production `SessionTimelineStore`.
Each workload measures a cold staged replay, a resume from the midpoint cursor, an exact-head zero-event replay, and an exact duplicate-head overlap.
The report also measures 10,000 A/B/A selection cycles across two resident 10,000-event Session timelines.

The workload fields are:

- `event_count`: durable events in the generated history.
- `serialized_history_bytes`: the exact sum of each durable event serialized as JSON plus one LF framing byte.
- `cold_staged_replay_nanoseconds`: time to create and select a timeline, stage every event, validate replay completion, and publish the verified timeline.
- `cold_staged_replay_events_per_second`: cold replay throughput derived from the measured duration.
- `midpoint_resume_from_sequence`: the verified sequence present before the measured resume.
- `midpoint_resume_event_count`: events staged after the midpoint cursor.
- `midpoint_resume_nanoseconds`: time to stage and commit only the resumed suffix.
- `midpoint_resume_events_per_second`: suffix replay throughput derived from the measured duration.
- `exact_head_replay_nanoseconds`: time to validate and commit a zero-event replay at the exact verified head.
- `exact_head_validated`: confirms the exact-head replay retained the event count, sequence, cursor, and live state.
- `duplicate_overlap_nanoseconds`: time to stage one exact duplicate of the head event and validate replay completion.
- `duplicate_overlap_validated`: confirms duplicate suppression retained the event count, sequence, cursor, and live state.

The `selection_a_b_a` object reports resident events per Session, cycles, transitions, total nanoseconds, and average nanoseconds per transition.
Workload generation, JSON serialization, midpoint-prefix construction, and selection-history construction occur outside their corresponding timed regions.
The benchmark does not include socket transport, NDJSON parsing, GPUI rendering, or end-to-end application startup.
`serialized_history_bytes` is an explicit, deterministic bound on serialized input-history size only.
It is not process RSS, allocator usage, or an estimate of the in-memory Rust object graph.

### Initial Session replay result

Observed on 2026-07-13 on arm64 macOS using the optimized profile:

```json
{"kind":"nopal.desktop-session-replay-benchmark/v1","memory_proxy":{"kind":"serialized_ndjson_history_bytes","scope":"serialized durable input history only","includes_lf_framing":true,"not_process_rss":true},"workloads":[{"event_count":10000,"serialized_history_bytes":4366650,"cold_staged_replay_nanoseconds":16191417,"cold_staged_replay_events_per_second":617611,"midpoint_resume_from_sequence":5000,"midpoint_resume_event_count":5000,"midpoint_resume_nanoseconds":9386625,"midpoint_resume_events_per_second":532672,"exact_head_replay_nanoseconds":1459,"exact_head_validated":true,"duplicate_overlap_nanoseconds":25334,"duplicate_overlap_validated":true},{"event_count":100000,"serialized_history_bytes":43866653,"cold_staged_replay_nanoseconds":222597750,"cold_staged_replay_events_per_second":449240,"midpoint_resume_from_sequence":50000,"midpoint_resume_event_count":50000,"midpoint_resume_nanoseconds":120588208,"midpoint_resume_events_per_second":414634,"exact_head_replay_nanoseconds":10917,"exact_head_validated":true,"duplicate_overlap_nanoseconds":28250,"duplicate_overlap_validated":true}],"selection_a_b_a":{"resident_events_per_session":10000,"cycles":10000,"transitions":30000,"total_nanoseconds":4738250,"average_nanoseconds_per_transition":157}}
```

The maximum-bound cold replay completed in approximately 222.6 milliseconds, while its 50,000-event midpoint suffix completed in approximately 120.6 milliseconds.
Both overlap checks retained the exact verified timeline.
The 100,000-event serialized history occupied 43,866,653 bytes, approximately 41.8 MiB, before any in-memory representation or allocator overhead.

### Session replay verification result

A fresh optimized verification run after the reconnect-cursor hardening completed the 100,000-event cold replay in approximately 313.6 milliseconds and its 50,000-event midpoint suffix in approximately 244.4 milliseconds.
Exact-head replay completed in approximately 3.6 microseconds and duplicate overlap in approximately 20.1 microseconds.
Both overlap validations retained the exact verified timeline, while A/B/A selection averaged 186 nanoseconds per transition.

## Adoption gates

- Warm application startup under 500 ms.
- Idle CPU below 1 percent.
- Idle memory below 150 MB.
- Terminal draw p50 below 4 ms.
- Terminal draw p99 below 8 ms under realistic load.
- No ordinary-interaction frame above 16.7 ms.
- Key-to-tmux dispatch p99 below 10 ms.
- Bounded terminal queue under sustained output.
- No lost input.
- No unbounded memory growth over one hour.
- IME candidate position tracks the terminal cursor.
- VoiceOver identifies and operates primary navigation.
- A bundled, licensed monospace font covers Powerline and common Nerd Font glyphs without tofu boxes.

## Evidence status

Parser and snapshot results are emitted as `nopal.desktop-terminal-benchmark/v1` JSON.
GPU paint timing, startup, idle CPU, memory, input dispatch, IME, accessibility, and long-duration observations remain required before GPUI adoption.
The hydrated walkthrough also showed missing private-use Powerline glyphs under the current `SF Mono` spike font, so font selection and packaging remain an explicit fidelity gate.
The interaction spike now covers rendered-shell key dispatch, native clipboard paste, composed-text commitment, scrollback, wide-character selection, SGR mouse encoding, exact single-pane resize and restore, bounded live-pipe reconnect, and a real controller-to-tmux path.
Manual IME candidate-window positioning, selection fidelity under complex grapheme clusters, and long-duration reconnect behavior still require visual dogfooding.
An optimized isolated-pane walkthrough resized a `120x40` pane to the focused canvas geometry of `66x35` and restored it to `120x40` after SIGINT.
This specifically verifies attach-time geometry ownership and signal-time restoration, including the shutdown path that bypasses Rust destructors.

## Initial result

Observed on 2026-07-12 on arm64 macOS 15.6.1 using the optimized profile:

```json
{"kind":"nopal.desktop-terminal-benchmark/v1","bytes":8388620,"parse_and_damage_mib_per_second":8.65666375533576,"snapshot_handoff_p50_microseconds":4,"snapshot_handoff_p99_microseconds":4,"runs_per_snapshot":88}
```

At 8,192-byte chunks, parse plus damage replacement averaged under one millisecond per chunk.
This clears the terminal-model budget for realistic output rates, but it does not establish GPU frame or input-dispatch performance.

The optimized idle shell was also observed after 17 seconds with no live Plot attached:

- CPU: 0.5 percent.
- Resident memory: 42,880 KiB, approximately 41.9 MiB.

These observations clear the provisional idle CPU and memory gates for the empty shell.
They must be repeated with a hydrated live Session and sustained output before adoption.

## Verification result

A fresh release-mode verification run on 2026-07-12 produced:

```json
{"kind":"nopal.desktop-terminal-benchmark/v1","bytes":8388620,"parse_and_damage_mib_per_second":7.095390537920004,"snapshot_handoff_p50_microseconds":4,"snapshot_handoff_p99_microseconds":4,"runs_per_snapshot":88}
```

The lower parse-throughput observation still keeps an 8,192-byte chunk near one millisecond, but adoption remains gated on the GPU and hydrated-session evidence above.

## Interactive-spike verification result

Four consecutive optimized runs after adding interaction support observed parse and damage throughput between 6.16 and 6.90 MiB/s.
Snapshot handoff p50 stayed between 3 and 4 microseconds, while p99 ranged from 4 to 25 microseconds.
The interaction controller does not participate in this synthetic replay path, so the lower throughput and wider p99 are recorded as measurement variance that must be separated from real GPU and input-dispatch timing before adoption.

## Output-first spike evidence

The output-first spike adds a separate native multiline composer, exact-pane instruction submission, optimistic user cards, a neutral terminal-screen projection, an Output/Terminal mode switch, and an app-owned clean tmux Session.
Automated coverage proves style-independent projection, private-use and control-character filtering, UTF-16 native input range handling, marked-text replacement, multiline editing, Enter submission bytes, bracketed multiline submission, Output as the default mode, and startup of a shell with `env -i` and no user startup files.

The presentation path does not change the terminal parser benchmark because it projects an already cached immutable snapshot only when the GPUI shell renders.
GPU timing for the added output and composer surfaces remains an adoption gate.
The current application-owned fixture is deliberately a clean shell, so natural-language text is treated as shell input unless an agent process is started in that Session.
The spike therefore proves the application interaction seam, not an agent protocol.

A release-mode run after the output-first changes measured 3.82 MiB/s parse and damage throughput, with 4 microsecond snapshot handoff at both p50 and p99.
This is below the earlier synthetic throughput range and reinforces that parser performance must be profiled before adoption, even though the cached snapshot handoff remains stable.

## Composer editor evidence

The expanded composer tests exercise rendered-shell Backspace without tmux writes, complete grapheme deletion, forward deletion, character and word navigation, hard-line vertical movement, forward and reverse selection, select-all, cut, clipboard output, undo, redo, redo invalidation, UTF-16 native ranges, and pointer-to-text hit testing.
The rendered multiline test also proves that the bounded three-line viewport follows the active cursor and exposes only the expected hard-line ranges for interaction.
Soft wrapping and rich structured composer content remain separate adoption work.
