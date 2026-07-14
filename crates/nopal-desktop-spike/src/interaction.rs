use crate::input::{Keystroke, TerminalInputMode, encode_keystroke_for_mode};
use crate::terminal::{Damage, TerminalModes, TerminalSnapshot, TerminalSurface};
use crate::tmux::PaneTransport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Degraded(String),
    Reconnecting(String),
    ReadOnly(String),
    Unavailable(String),
}

pub struct TerminalController<T>
where
    T: PaneTransport,
{
    pane_id: String,
    terminal: TerminalSurface,
    transport: T,
    original_size: (usize, usize),
    current_size: (usize, usize),
    focused: bool,
    connection_state: ConnectionState,
    selection: Option<((usize, usize), (usize, usize))>,
}

impl<T> TerminalController<T>
where
    T: PaneTransport,
{
    pub fn new(
        pane_id: String,
        terminal: TerminalSurface,
        transport: T,
        original_size: (usize, usize),
    ) -> Self {
        Self {
            pane_id,
            terminal,
            transport,
            original_size,
            current_size: original_size,
            focused: false,
            connection_state: ConnectionState::Connected,
            selection: None,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn apply_output(&mut self, bytes: &[u8]) -> Damage {
        self.terminal.apply_output(bytes)
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        self.terminal.snapshot()
    }

    pub fn input_mode(&self) -> TerminalModes {
        self.terminal.input_mode()
    }

    pub fn send_keystroke(&mut self, keystroke: &Keystroke) -> bool {
        let mode = TerminalInputMode {
            application_cursor: self.terminal.input_mode().application_cursor,
        };
        let Some(bytes) = encode_keystroke_for_mode(keystroke, mode) else {
            return false;
        };
        if !self.focused {
            return false;
        }
        self.terminal.scroll_to_bottom();
        self.selection = None;
        self.send_bytes(&bytes)
    }

    pub fn send_text(&mut self, text: &str) -> bool {
        if !self.focused || text.is_empty() {
            return false;
        }
        self.terminal.scroll_to_bottom();
        self.selection = None;
        self.send_bytes(text.as_bytes())
    }

    pub fn send_paste(&mut self, text: &str) -> bool {
        if !self.focused || text.is_empty() {
            return false;
        }
        let bytes = if self.terminal.input_mode().bracketed_paste {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            bytes
        } else {
            text.as_bytes().to_vec()
        };
        self.terminal.scroll_to_bottom();
        self.selection = None;
        self.send_bytes(&bytes)
    }

    pub fn submit_instruction(&mut self, instruction: &str) -> bool {
        if instruction.is_empty() {
            return false;
        }
        let mut bytes = if instruction.contains('\n') && self.terminal.input_mode().bracketed_paste
        {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(instruction.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            bytes
        } else {
            instruction.as_bytes().to_vec()
        };
        bytes.push(b'\r');
        self.terminal.scroll_to_bottom();
        self.selection = None;
        self.send_bytes(&bytes)
    }

    pub fn scroll_lines(&mut self, lines: i32) -> bool {
        self.terminal.scroll_lines(lines)
    }

    pub fn begin_selection(&mut self, cell: (usize, usize)) {
        self.selection = Some((cell, cell));
    }

    pub fn update_selection(&mut self, cell: (usize, usize)) {
        if let Some((anchor, _)) = self.selection {
            self.selection = Some((anchor, cell));
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        self.selection
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection
            .map(|(start, end)| self.terminal.text_between(start, end))
    }

    pub fn send_mouse(&mut self, button: u8, pressed: bool, cell: (usize, usize)) -> bool {
        let mode = self.terminal.input_mode();
        if !self.focused || !mode.mouse_reporting || !mode.sgr_mouse {
            return false;
        }
        let suffix = if pressed { 'M' } else { 'm' };
        let sequence = format!(
            "\x1b[<{button};{};{}{suffix}",
            cell.0.saturating_add(1),
            cell.1.saturating_add(1)
        );
        self.send_bytes(sequence.as_bytes())
    }

    fn send_bytes(&mut self, bytes: &[u8]) -> bool {
        match self.transport.send_input(&self.pane_id, bytes) {
            Ok(()) => true,
            Err(error) => {
                self.connection_state = ConnectionState::ReadOnly(error);
                false
            }
        }
    }

    pub fn resize(&mut self, columns: usize, rows: usize) -> bool {
        if !self.focused {
            return false;
        }
        let dimensions = (columns.max(2), rows.max(1));
        if dimensions == self.current_size {
            return false;
        }
        if let Err(error) = self
            .transport
            .resize_pane(&self.pane_id, dimensions.0, dimensions.1)
        {
            self.connection_state = ConnectionState::Degraded(error);
            return false;
        }
        self.current_size = dimensions;
        self.terminal.resize(dimensions.0, dimensions.1);
        true
    }

    pub fn restore_original_size(&mut self) {
        if self.current_size == self.original_size {
            return;
        }
        if let Err(error) =
            self.transport
                .resize_pane(&self.pane_id, self.original_size.0, self.original_size.1)
        {
            self.connection_state = ConnectionState::Degraded(error);
            return;
        }
        self.current_size = self.original_size;
        self.terminal
            .resize(self.original_size.0, self.original_size.1);
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.connection_state.clone()
    }

    pub fn mark_reconnecting(&mut self, detail: impl Into<String>) {
        self.connection_state = ConnectionState::Reconnecting(detail.into());
    }

    pub fn mark_connected(&mut self) {
        self.connection_state = ConnectionState::Connected;
    }

    pub fn mark_unavailable(&mut self, detail: impl Into<String>) {
        self.connection_state = ConnectionState::Unavailable(detail.into());
    }

    pub fn grid_for_pixels(
        width: f32,
        height: f32,
        cell_width: f32,
        line_height: f32,
    ) -> (usize, usize) {
        let columns = (width.max(cell_width) / cell_width.max(1.0)).floor() as usize;
        let rows = (height.max(line_height) / line_height.max(1.0)).floor() as usize;
        (columns.max(2), rows.max(1))
    }
}

impl<T> Drop for TerminalController<T>
where
    T: PaneTransport,
{
    fn drop(&mut self) {
        self.restore_original_size();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::process::Command;
    use std::rc::Rc;

    use crate::source::ProcessRunner;
    use crate::terminal::TerminalSurface;
    use crate::tmux::{PaneTransport, TmuxTransport};

    use super::{ConnectionState, TerminalController};

    #[derive(Clone, Default)]
    struct RecordingTransport {
        calls: Rc<RefCell<Vec<String>>>,
    }

    impl PaneTransport for RecordingTransport {
        fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String> {
            self.calls.borrow_mut().push(format!(
                "input {pane_id} {}",
                String::from_utf8_lossy(bytes)
            ));
            Ok(())
        }

        fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("resize {pane_id} {columns}x{rows}"));
            Ok(())
        }
    }

    #[test]
    fn focused_printable_input_reaches_only_the_selected_pane() {
        let transport = RecordingTransport::default();
        let calls = transport.calls.clone();
        let mut controller = TerminalController::new(
            "%41".to_owned(),
            TerminalSurface::new(80, 24),
            transport,
            (80, 24),
        );

        assert!(!controller.send_text("ignored"));
        controller.set_focused(true);
        assert!(controller.send_text("nopal"));

        assert_eq!(controller.connection_state(), ConnectionState::Connected);
        assert_eq!(calls.borrow().as_slice(), ["input %41 nopal"]);
    }

    #[test]
    fn composer_submission_reaches_the_selected_pane_as_one_instruction() {
        let transport = RecordingTransport::default();
        let calls = transport.calls.clone();
        let mut controller = TerminalController::new(
            "%42".to_owned(),
            TerminalSurface::new(80, 24),
            transport,
            (80, 24),
        );

        assert!(controller.submit_instruction("review this\nthen test it"));
        assert_eq!(
            calls.borrow().as_slice(),
            ["input %42 review this\nthen test it\r"]
        );
    }

    #[test]
    fn multiline_submission_uses_bracketed_paste_when_the_session_supports_it() {
        let transport = RecordingTransport::default();
        let calls = transport.calls.clone();
        let mut terminal = TerminalSurface::new(80, 24);
        terminal.apply_output(b"\x1b[?2004h");
        let mut controller =
            TerminalController::new("%43".to_owned(), terminal, transport, (80, 24));

        assert!(controller.submit_instruction("first\nsecond"));
        assert_eq!(
            calls.borrow().as_slice(),
            ["input %43 \u{1b}[200~first\nsecond\u{1b}[201~\r"]
        );
    }

    #[test]
    fn geometry_changes_are_deduplicated_and_original_size_can_be_restored() {
        let transport = RecordingTransport::default();
        let calls = transport.calls.clone();
        let mut controller = TerminalController::new(
            "%9".to_owned(),
            TerminalSurface::new(80, 24),
            transport,
            (80, 24),
        );

        controller.set_focused(true);
        assert!(controller.resize(100, 30));
        assert!(!controller.resize(100, 30));
        controller.restore_original_size();

        assert_eq!(
            calls.borrow().as_slice(),
            ["resize %9 100x30", "resize %9 80x24"]
        );
    }

    #[test]
    fn paste_scrollback_selection_and_mouse_sequences_share_one_controller() {
        let transport = RecordingTransport::default();
        let calls = transport.calls.clone();
        let mut terminal = TerminalSurface::new(10, 2);
        terminal.apply_output(b"one two\r\nthree\r\nfour\x1b[?2004h\x1b[?1000h\x1b[?1006h");
        let mut controller = TerminalController::new("%5".to_owned(), terminal, transport, (10, 2));
        controller.set_focused(true);

        assert!(controller.send_paste("alpha\nbeta"));
        assert!(controller.scroll_lines(1));
        controller.begin_selection((0, 4));
        controller.update_selection((1, 4));
        assert_eq!(controller.selected_text().as_deref(), Some("two\nthree"));
        assert!(controller.send_mouse(0, true, (2, 3)));
        assert!(controller.send_mouse(0, false, (2, 3)));

        assert_eq!(
            calls.borrow().as_slice(),
            [
                "input %5 \u{1b}[200~alpha\nbeta\u{1b}[201~",
                "input %5 \u{1b}[<0;3;4M",
                "input %5 \u{1b}[<0;3;4m"
            ]
        );
    }

    #[test]
    fn failed_input_becomes_observably_read_only() {
        #[derive(Clone)]
        struct FailingTransport;

        impl PaneTransport for FailingTransport {
            fn send_input(&self, _: &str, _: &[u8]) -> Result<(), String> {
                Err("pane disappeared".to_owned())
            }

            fn resize_pane(&self, _: &str, _: usize, _: usize) -> Result<(), String> {
                Ok(())
            }
        }

        let mut controller = TerminalController::new(
            "%6".to_owned(),
            TerminalSurface::new(80, 24),
            FailingTransport,
            (80, 24),
        );
        controller.set_focused(true);

        assert!(!controller.send_text("nope"));
        assert_eq!(
            controller.connection_state(),
            ConnectionState::ReadOnly("pane disappeared".to_owned())
        );
    }

    #[test]
    fn grid_measurement_clamps_tiny_canvases_and_floors_partial_cells() {
        assert_eq!(
            TerminalController::<RecordingTransport>::grid_for_pixels(803.0, 407.0, 8.0, 18.0),
            (100, 22)
        );
        assert_eq!(
            TerminalController::<RecordingTransport>::grid_for_pixels(1.0, 1.0, 8.0, 18.0),
            (2, 1)
        );
    }

    #[test]
    fn reconnect_state_is_explicit_and_can_recover() {
        let mut controller = TerminalController::new(
            "%8".to_owned(),
            TerminalSurface::new(80, 24),
            RecordingTransport::default(),
            (80, 24),
        );

        controller.mark_reconnecting("stream closed");
        assert_eq!(
            controller.connection_state(),
            ConnectionState::Reconnecting("stream closed".to_owned())
        );
        controller.mark_connected();
        assert_eq!(controller.connection_state(), ConnectionState::Connected);
        controller.mark_unavailable("pane is gone");
        assert_eq!(
            controller.connection_state(),
            ConnectionState::Unavailable("pane is gone".to_owned())
        );
    }

    #[test]
    fn real_controller_types_and_restores_an_isolated_tmux_window() {
        let session = format!("nopal-interaction-controller-{}", std::process::id());
        let created = Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, "-x", "80", "-y", "25"])
            .output()
            .expect("create tmux fixture");
        assert!(created.status.success(), "cannot create tmux fixture");
        let _cleanup = SessionCleanup(session.clone());
        let listed = Command::new("tmux")
            .args(["list-panes", "-t", &session, "-F", "#{pane_id}"])
            .output()
            .expect("list fixture pane");
        let pane_id = String::from_utf8_lossy(&listed.stdout).trim().to_owned();
        let transport = TmuxTransport::new(ProcessRunner);
        let original = transport.pane_size(&pane_id).expect("original size");
        let mut controller = TerminalController::new(
            pane_id.clone(),
            TerminalSurface::new(original.0, original.1),
            transport,
            original,
        );
        controller.set_focused(true);

        assert!(controller.resize(100, 30));
        assert!(controller.send_text("printf 'controller-proof\\n'\r"));
        assert_eq!(
            TmuxTransport::new(ProcessRunner)
                .pane_size(&pane_id)
                .expect("resized pane"),
            (100, 30)
        );
        controller.restore_original_size();
        assert_eq!(
            TmuxTransport::new(ProcessRunner)
                .pane_size(&pane_id)
                .expect("restored pane"),
            original
        );
    }

    struct SessionCleanup(String);

    impl Drop for SessionCleanup {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &self.0])
                .output();
        }
    }
}
