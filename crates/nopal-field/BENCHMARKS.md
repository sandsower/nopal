# Field feel benchmarks

These ten benchmarks are the acceptance gate for the tmux-backed Field architecture.
Scriptable ones run through `nopal field bench`; the rest are manual with exact instructions below.
Measured on tmux 3.6a, macOS (Darwin 24.6.0), release build, 2026-07-07.

## How to run the scriptable set

```sh
nopal field bench --seats 20 --iterations 100
```

The harness creates a throwaway session `nopal-field-test-bench-<pid>` (history-limit 10000, 20 sleeping seats), runs against a real tmux server and real control-mode sidecar, renders into an in-memory ratatui `TestBackend`, and cleans the session up.

## Results

| # | Benchmark | Gate | Result | How measured |
|---|-----------|------|--------|--------------|
| 1 | Focused-seat echo | <= +10ms over raw tmux; p99 < 20ms under load | **Pass by construction** (see note) | structural argument + manual spot check |
| 2 | Ctrl+C under firehose | interrupt lands < 150ms; field never freezes | **Pass by construction + firehose run** | manual check + `bench` firehose |
| 3 | Tear-free under load | zero partially drawn frames, 6 streaming seats | manual | instructions below |
| 4 | Cursor never flickers | sidebar activity must not touch seat cursor | **Pass by construction** (separate panes) | manual regression check below |
| 5 | Resize | reflow < 200ms, no stale wrap | native tmux behavior; manual | instructions below |
| 6 | Scrollback | 100k lines smooth, correct colors | native tmux behavior; manual | instructions below |
| 7 | Cold open/attach | 20 seats, start -> sidebar live < 1.5s | **6.6ms** (21 rows + first frame) | `bench` cold-open |
| 8 | Sidebar under load | rows <= 1s behind reality | **p50 0.25ms, p99 0.52ms** event->render (own session); foreign sessions <= 5s snapshot | `bench` latency (100 samples) |
| 9 | Protocol gauntlet | htop/vim/kitty-graphics/CSI-u per feature | native tmux passthrough; manual | instructions below |
| 10 | Idle cost ~0 + profiling counters | ~0 CPU idle | **0ms CPU / 3s idle; 0ms CPU / 3s firehose; 24 notifications** | `bench` idle + firehose; `p` key shows counters |

## Embedded-view echo (new metric, Feature 3)

The embedded seat panel (`Enter` on a seat) mirrors a live pane with an `alacritty_terminal` VT grid fed by a per-pane `pipe-pane -O` fifo, and routes input back with `send-keys -H`.
This is a deliberately honest, separate metric: it does **not** beat raw tmux and is not meant to.
The zero-overhead flagship path (`f`, full-focus via `swap-pane`/`switch-client`) remains benchmark #1's "pass by construction" and is what you use for real work; the embedded panel is a peek-and-light-interaction surface.

| Benchmark | What it measures | Result |
|-----------|------------------|--------|
| Embedded-view echo | keystroke `send-keys -H` -> tty echo -> `pipe-pane` fifo -> VT parse -> grid ready (100 samples, `cat` pane) | **p50 4.5ms, p99 5.6ms** |

Run it as part of the scriptable set (`nopal field bench`); it is the `embedded-view echo` line.
The cost is the server round trip (input to the pane's PTY, output back out through the fifo, parse into the grid) plus a render; it is bounded and steady, an order of magnitude slower than the flagship's +0ms echo, and that trade is the whole point of keeping full-focus as a separate key.

### Cursor and flicker (Feature 3)

herdr's weakest area was hide-cursor-every-frame flicker (its issues #930/#967).
The embedded panel avoids that class by construction: the cursor is rendered from the VT grid's own cursor state via ratatui's `set_cursor_position` (surfaced only while the seat holds input focus), never by toggling DECTCEM per frame.
Repaint minimization is ratatui's own double-buffer cell diff - only cells that changed between frames are written to the outer terminal - so a busy seat does not force full-panel repaints.

### Embedded-view scope cuts (v1, deliberate)

- No scrollback in the embedded grid (the flagship full-focus path has native tmux scrollback).
- No mouse passthrough into the mirrored seat.
- 256-color and RGB only (named + indexed + truecolor); other SGR niceties (OSC 8 hyperlinks, kitty graphics) are not mirrored - use full-focus.
- The grid is sized to the seat's real dimensions at attach; a seat resized elsewhere after attach is not reflowed (reopen the panel). The seat is never resized by the field.

## Why benchmarks 1, 2, and 4 hold by construction

Architecture B puts nothing between the seat and the outer terminal.
The focused seat is a real tmux pane rendered by the operator's own terminal; keystrokes travel terminal -> tmux server -> seat PTY exactly as in bare tmux, and the field process is not on that path at all.
Focused-seat echo therefore equals raw tmux echo by identity (+0ms), not by optimization.
The same holds for Ctrl+C: the interrupt goes straight to the seat's PTY; the sidecar attaches with `-f no-output`, so a seat streaming full tilt sends the field nothing (the firehose run shows 24 notifications in 3s - pane_current_command churn, not output).
The field UI cannot freeze the seat because they are separate processes in separate panes; cursor visibility in the seat pane belongs to the seat, and sidebar spinners run in the field's own pane cursor scope.

## Manual benchmark instructions

Benchmark 3 (tear-free): spawn 6 seats each running `yes $(date)`, focus one, record the terminal at 120fps (e.g. iPhone slow-mo on the screen or `asciinema` + frame dump), and inspect for partially drawn frames.
tmux >= 3.4 wraps pane redraws in synchronized-output brackets when the outer terminal advertises mode 2026; verify with `tmux display -p '#{client_flags}'`.

Benchmark 5 (resize): with a full-screen vim session in the focused seat and 100k lines of scrollback, drag-resize the terminal window; reflow should complete within 200ms of the drag ending and history must re-wrap without stale line breaks.
This is native tmux reflow; the field adds a fixed 44-column sidebar split only.

Benchmark 6 (scrollback): `seq 100000` in a seat, enter copy mode, wheel-scroll and jump to top (`g`); scrolling must stay smooth with correct colors at every offset.

Benchmark 9 (protocol gauntlet): in the focused seat run htop (mouse), vim (alt-screen), `kitty +kitten icat` an image with `allow-passthrough on`, and Shift+Enter inside Claude Code (CSI u / extended keys; `set -g extended-keys on`).
Score each pass/fail; all are native tmux passthrough, so failures indict tmux configuration, not the field.

Benchmark 10 profiling counters: press `p` in the field for events-reduced and frames-rendered counters in the status line.

## Reading the firehose number

`-f no-output` is the load-bearing design decision: the control client subscribes to state formats, never pane output.
The 3s `yes` firehose reaches the field as ~24 subscription/notification lines (command churn), not megabytes of output, and costs ~0 CPU (below the 10ms measurement resolution).
Any future client consuming `%output` must clear this same gate before promotion.
