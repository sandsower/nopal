# Nopal desktop spike

This crate is an isolated feasibility spike for a native Nopal Field.
It tests a Codex-inspired GPUI application shell, a separate native instruction composer, a normalized output-first Session view, a custom Alacritty-backed terminal escape hatch, and tmux-backed Session persistence without changing the existing TUI or Nopal Core contracts.

The crate is intentionally non-publishable until the adoption gate in `BENCHMARKS.md` passes.

For visual dogfooding against a Nopal-owned tmux Session without shell startup files or user prompt customization, pass `--clean-demo`.
For testing against an existing isolated pane without writing Plot state, pass `--demo-pane %NN`.
This development-only path uses the real tmux capture, live-output, input, terminal-model, and GPU-canvas boundaries with a fixture Plot shell.

## Output-first interaction

- Output is the default center view.
- It projects the current terminal screen into Nopal-owned typography and neutral colors.
- Terminal colors, font attributes, cursor chrome, control characters, and private-use prompt glyphs are not carried into Output.
- Type in the persistent composer at the bottom of the window.
- Edit with grapheme-safe Backspace and Delete.
- Move by character, word, line, or visual hard line with Arrow, Option-Arrow, Command-Arrow, Home, and End.
- Hold Shift with navigation or drag the pointer to select text.
- Use Command-A/C/X/V/Z/Shift-Z on macOS, with Control equivalents on Linux and Windows, for selection, clipboard editing, undo, and redo.
- Click within visible composer text to place the cursor.
- The bounded three-line viewport follows the active cursor through longer multiline input.
- Press Enter to submit an instruction to the selected Session.
- Press Shift-Enter to insert a newline.
- Multiline submissions use bracketed paste when the attached Session advertises support.
- Submitted instructions appear as application-owned user cards.
- Switch to Terminal for the complete terminal model and direct interaction.

Output is intentionally labeled `Live output projection`.
It is a normalized view of current terminal state, not a semantic conversation transcript.
Reliable assistant turns, tool calls, attribution, and append-only history require a future structured Session event protocol.
Soft wrapping, attachments, mentions, slash commands, and structured content chips remain outside this spike.

## Terminal interaction

- Click the terminal to focus it.
- Type normally or use Control, Option, navigation, Insert, Delete, Page Up, Page Down, and F1 through F12.
- Paste with Command-V on macOS or Control-Shift-V on Linux and Windows.
- Drag to select locally, then copy with Command-C or Control-Shift-C.
- Hold Shift while selecting when the running terminal application has enabled mouse reporting.
- Scroll with the wheel or trackpad for local history, or hold Shift to force local history while mouse reporting is active.
- Resize the native window to resize an isolated single-pane tmux window while focused.

The status strip distinguishes focused, connected, fixed-size degraded, reconnecting, read-only, and unavailable states.
Shared tmux windows containing multiple panes remain interactive at their existing geometry instead of resizing neighboring panes.
The original single-pane geometry is restored when the native terminal detaches.
