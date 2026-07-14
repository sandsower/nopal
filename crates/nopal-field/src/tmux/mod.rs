//! tmux session backend.
//!
//! Seats are real tmux panes; the field is a ratatui app in its own
//! pane; focus switching is tmux pane plumbing (`swap-pane`). This module
//! wraps one-shot `tmux` invocations; the push-based state feed lives in
//! [`sidecar`].
//!
//! Nopal owns the seat lifecycle natively: spawn, adopt,
//! kill, and agent launch/relaunch are field code, not delegated to a
//! third-party session manager. tmux (and git, for worktrees) remain the
//! substrate underneath - seats are real tmux sessions reachable by every
//! outside tool. `zoxide` is fed best-effort on spawn so the operator's
//! frecency tools still see nopal seats; it is optional interop, never a
//! dependency.

pub mod sidecar;

use std::io;
use std::process::Command;

/// Pane user option marking the field's own pane.
pub const ROLE_OPTION: &str = "@nopal_role";
/// Pane user option carrying the seat name.
pub const SEAT_OPTION: &str = "@nopal_seat";
/// Pane user option carrying the seat's repo tag.
pub const REPO_OPTION: &str = "@nopal_repo";
/// Session user option marking a session nopal opened or adopted. Session
/// scope resolves for every pane in the session, so one `set-option`
/// stamps the whole session (verified on tmux 3.6a).
pub const MANAGED_OPTION: &str = "@nopal_managed";
/// Session user option carrying the Core-owned Plot id.
pub const PLOT_OPTION: &str = "@nopal_plot";
/// Session user option carrying the Core-owned Session id.
pub const PLOT_SESSION_OPTION: &str = "@nopal_plot_session";
/// Name of the window hosting the field pane and the focused seat slot.
pub const FIELD_WINDOW: &str = "field";
/// Sidebar width in columns; the rest of the window is the focused seat.
pub const SIDEBAR_COLUMNS: u16 = 44;

/// One-shot tmux plumbing against a named session.
#[derive(Debug, Clone)]
pub struct Backend {
    pub session: String,
}

impl Backend {
    pub fn new(session: String) -> Self {
        Self { session }
    }

    /// Run tmux with `args`, returning trimmed stdout.
    fn tmux(args: &[&str]) -> io::Result<String> {
        let output = Command::new("tmux").args(args).output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "tmux {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn pane_window_size(pane_id: &str) -> Option<(u16, u16)> {
        let text = Self::tmux(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{window_width} #{window_height}",
        ])
        .ok()?;
        parse_size_pair(&text)
    }

    pub fn session_active_path(name: &str) -> Option<String> {
        let text = Self::tmux(&[
            "display-message",
            "-p",
            "-t",
            &format!("={name}:"),
            "#{pane_current_path}",
        ])
        .ok()?;
        (!text.is_empty()).then_some(text)
    }

    pub fn session_active_pane(name: &str) -> io::Result<String> {
        Self::tmux(&[
            "display-message",
            "-p",
            "-t",
            &format!("={name}:"),
            "#{pane_id}",
        ])
    }

    pub fn session_exists(&self) -> bool {
        // `=` pins an exact session name, not a prefix match.
        Command::new("tmux")
            .args(["has-session", "-t", &format!("={}", self.session)])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// Create the session detached with the field window: pane 0 (left,
    /// fixed width) runs `ui_command`; pane 1 (right) is the focused-seat
    /// slot holding the operator's shell until a seat is focused.
    pub fn create_session(&self, ui_command: &str) -> io::Result<()> {
        let target = format!("={}", self.session);
        let field_pane = Self::tmux(&[
            "new-session",
            "-d",
            "-s",
            &self.session,
            "-n",
            FIELD_WINDOW,
            "-x",
            "220",
            "-y",
            "60",
            "-P",
            "-F",
            "#{pane_id}",
        ])?;
        Self::tmux(&["set-option", "-p", "-t", &field_pane, ROLE_OPTION, "field"])?;
        // The slot pane: the operator's login shell to the right.
        let window = format!("{target}:{FIELD_WINDOW}");
        Self::tmux(&["split-window", "-h", "-d", "-t", &field_pane])?;
        let width = SIDEBAR_COLUMNS.to_string();
        Self::tmux(&["resize-pane", "-t", &field_pane, "-x", &width])?;
        // Keep the field from being killed by stray pane closes.
        Self::tmux(&["set-option", "-t", &window, "remain-on-exit", "off"])?;
        // The field's own session is managed by construction.
        let _ = Self::mark_session_managed(&self.session, "");
        // Start the UI only after the slot exists. Otherwise first-open
        // bootstrap can request zoom while this is still a one-pane window;
        // tmux treats that as a no-op and the later split leaves the
        // Plot-first surface permanently cramped.
        Self::tmux(&["respawn-pane", "-k", "-t", &field_pane, ui_command])?;
        Ok(())
    }

    /// Spawn a seat as a real tmux window (detached; placement-tagged).
    /// Returns the new pane id.
    pub fn spawn_seat(
        &self,
        name: &str,
        repo_tag: &str,
        dir: Option<&str>,
        command: Option<&str>,
    ) -> io::Result<String> {
        let target = format!("={}", self.session);
        let window_name = format!("seat:{name}");
        let mut args: Vec<&str> = vec![
            "new-window",
            "-d",
            "-t",
            &target,
            "-n",
            &window_name,
            "-P",
            "-F",
            "#{pane_id}",
        ];
        if let Some(dir) = dir {
            args.push("-c");
            args.push(dir);
        }
        if let Some(command) = command {
            args.push(command);
        }
        let pane_id = Self::tmux(&args)?;
        Self::tmux(&["set-option", "-p", "-t", &pane_id, SEAT_OPTION, name])?;
        Self::tmux(&["set-option", "-p", "-t", &pane_id, REPO_OPTION, repo_tag])?;
        Ok(pane_id)
    }

    /// Swap `seat_pane` into the focused-seat slot position: `swap-pane -d`
    /// trades the two panes' places (pane ids travel with the panes, ttys
    /// never move), then renames the vacated window after its new occupant,
    /// but only when the field itself named it (`seat:*` prefix);
    /// adopted windows keep the user's names (their window-status formats
    /// display them). `seat_pane == slot_pane` (the seat already is the
    /// slot) is the caller's guard to make, not this function's - it is an
    /// associated function over three bare pane/window ids, with no
    /// `&self` needed, so [`Self::focus_seat`] (which also wants tmux's
    /// active-pane focus afterward) and the keyboard/context-menu "swap
    /// into slot" control (which deliberately does not) can both call
    /// exactly this shared half without duplicating the swap-pane +
    /// conditional rename sequence.
    pub fn swap_seat_into_slot(
        seat_pane: &str,
        slot_pane: &str,
        vacated_window: &str,
        vacated_window_name: &str,
        displaced_label: &str,
    ) -> io::Result<()> {
        if seat_pane != slot_pane {
            Self::tmux(&["swap-pane", "-d", "-s", seat_pane, "-t", slot_pane])?;
            if vacated_window_name.starts_with("seat:") {
                Self::tmux(&[
                    "rename-window",
                    "-t",
                    vacated_window,
                    &format!("seat:{displaced_label}"),
                ])?;
            }
        }
        Ok(())
    }

    /// Bring `seat_pane` into the focused-seat slot next to the field
    /// pane and give it input focus: [`Self::swap_seat_into_slot`]'s swap +
    /// rename, then the select-window/select-pane focus tail that makes it
    /// the operator's active pane. The swap-only half is what the
    /// `w`/"swap into slot" control wants instead - see
    /// [`Self::swap_seat_into_slot`]'s own doc for why that is a separate,
    /// `self`-free function rather than a flag on this one.
    pub fn focus_seat(
        &self,
        seat_pane: &str,
        slot_pane: &str,
        vacated_window: &str,
        vacated_window_name: &str,
        displaced_label: &str,
    ) -> io::Result<()> {
        Self::swap_seat_into_slot(
            seat_pane,
            slot_pane,
            vacated_window,
            vacated_window_name,
            displaced_label,
        )?;
        // After the swap the seat pane id sits at the slot position.
        Self::tmux(&[
            "select-window",
            "-t",
            &format!("={}:{FIELD_WINDOW}", self.session),
        ])?;
        Self::tmux(&["select-pane", "-t", seat_pane])?;
        Ok(())
    }

    /// Focus a seat living in another session: switch the operator's
    /// terminal client(s) there - explicitly the non-control clients
    /// attached to the field's session, so the control-mode sidecar is
    /// never switched away (its subscriptions are session-scoped).
    /// Targets are ids, never names, so slash-named sessions
    /// (`agenticfootball/_bare/x`) are safe. The field stays reachable
    /// through the operator's session switcher (sesh last / prefix-L).
    pub fn switch_to_pane(
        field_session_id: &str,
        session_id: &str,
        pane_id: &str,
    ) -> io::Result<()> {
        // Select destination window/pane first so the arriving client
        // lands on the seat.
        Self::tmux(&["select-window", "-t", pane_id])?;
        Self::tmux(&["select-pane", "-t", pane_id])?;
        let clients = Self::tmux(&[
            "list-clients",
            "-F",
            "#{client_name}|#{client_control_mode}|#{session_id}",
        ])?;
        let mut switched = false;
        for line in clients.lines() {
            let fields: Vec<&str> = line.split('|').collect();
            if fields.len() == 3 && fields[1] == "0" && fields[2] == field_session_id {
                Self::tmux(&["switch-client", "-c", fields[0], "-t", session_id])?;
                switched = true;
            }
        }
        if !switched {
            return Err(io::Error::other(
                "no terminal client attached to the field session",
            ));
        }
        Ok(())
    }

    /// Spawn or attach a seat session at `path` under `name`, natively.
    /// There is no delegation to a third-party session manager and no
    /// `NOPAL_FIELD_SESH` knob - that whole dual path is deleted, not
    /// flagged off).
    ///
    /// If a session named `name` already exists, this is an attach: the
    /// existing session is left alone (whatever is running in it keeps
    /// running) and `agent_cmd` is not sent. Otherwise a fresh detached
    /// session is created at `path` and `agent_cmd` is launched in its
    /// active pane as a single literal command line via `send-keys`
    /// (never split into words, never PATH-relative - the caller passes
    /// an absolute `nopal_bin` invocation per the binary-staleness
    /// binary-staleness constraint). Either way the session is stamped
    /// `@nopal_managed`;
    /// a fresh spawn also best-effort feeds `zoxide`. The terminal client
    /// is never switched away from the field - the caller surfaces the
    /// seat in the embedded panel, and full focus stays an explicit `f`
    /// by design. Returns `(how, session_name, pane_id)` for
    /// the caller's status line and explicit Core Session binding.
    pub fn spawn_session_seat(
        path: &str,
        name: &str,
        agent_cmd: &str,
        size: Option<(u16, u16)>,
    ) -> io::Result<(String, String, String)> {
        let repo = crate::state::worktree_repo_tag(path);
        let target = format!("={name}");
        let exists = Command::new("tmux")
            .args(["has-session", "-t", &target])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if exists {
            let _ = Self::mark_session_managed(name, &repo);
            let pane_id = Self::session_active_pane(name)?;
            return Ok((format!("attached {name}"), name.to_owned(), pane_id));
        }
        let new_session = new_session_args(name, path, size);
        let new_session_args: Vec<&str> = new_session.iter().map(String::as_str).collect();
        Self::tmux(&new_session_args)?;
        // Managed marker before the agent send: even a failed launch must
        // leave the seat visible in the sidebar (`s` relaunches it).
        let _ = Self::mark_session_managed(name, &repo);
        let send = deferred_agent_send(name, path, agent_cmd);
        let send_args: Vec<&str> = send.iter().map(String::as_str).collect();
        Self::tmux(&send_args)?;
        zoxide_add(path);
        // Deliberately no switch-client here:
        // yanking the terminal client to the raw session throws the
        // operator out of the field. The caller queues the new seat for
        // the embedded panel instead; full focus stays an explicit `f`.
        let pane_id = Self::session_active_pane(name)?;
        Ok((format!("spawned {name}"), name.to_owned(), pane_id))
    }

    /// (Re)launch `agent_cmd` in an existing pane: used for the relaunch
    /// keybind on a seat whose agent already exited, and for adopted
    /// seats that never had one. Same single-literal-argument `send-keys`
    /// contract as the spawn path.
    pub fn launch_agent_in_pane(pane_id: &str, agent_cmd: &str) -> io::Result<()> {
        Self::tmux(&["send-keys", "-t", pane_id, agent_cmd, "Enter"]).map(|_| ())
    }

    /// Kill a whole-session seat by name (window seats inside the
    /// field's own session use [`Backend::kill_pane`] instead).
    pub fn kill_session_named(name: &str) -> io::Result<()> {
        Self::tmux(&["kill-session", "-t", &format!("={name}")]).map(|_| ())
    }

    /// Tag a pane with the field role (idempotent; self-repair after
    /// resurrect restores, which drop pane user options).
    pub fn tag_field_pane(pane_id: &str) -> io::Result<()> {
        Self::tmux(&["set-option", "-p", "-t", pane_id, ROLE_OPTION, "field"]).map(|_| ())
    }

    /// Stamp the `@nopal_managed` marker (and repo tag, if given) on a
    /// session by name, bringing it into nopal's managed set. Session-scope
    /// options resolve for every pane in the session. `set-option -t`
    /// rejects the `=exact` prefix on session targets (tmux 3.6a), so the
    /// bare name is passed. Best-effort: a missing session is not an error
    /// the caller must handle.
    pub fn mark_session_managed(session: &str, repo: &str) -> io::Result<()> {
        Self::tmux(&["set-option", "-t", session, MANAGED_OPTION, "1"])?;
        if !repo.is_empty() {
            let _ = Self::tmux(&["set-option", "-t", session, REPO_OPTION, repo]);
        }
        Ok(())
    }

    /// Stamp the explicit Core Plot and Session identities onto a tmux
    /// session. Session scope makes both values visible from every pane.
    pub fn stamp_plot_identity(
        session: &str,
        plot_id: &str,
        plot_session_id: &str,
    ) -> io::Result<()> {
        Self::tmux(&["set-option", "-t", session, PLOT_OPTION, plot_id])?;
        Self::tmux(&[
            "set-option",
            "-t",
            session,
            PLOT_SESSION_OPTION,
            plot_session_id,
        ])?;
        Ok(())
    }

    /// Zoom (or unzoom) the window a pane lives in so the field pane can
    /// fill the window for the embedded-seat panel. Toggling to the desired
    /// state only; querying the zoom flag first avoids a double-toggle.
    pub fn set_zoom(pane_id: &str, zoomed: bool) -> io::Result<()> {
        let current = Self::tmux(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{window_zoomed_flag}",
        ])
        .map(|s| s.trim() == "1")
        .unwrap_or(false);
        if current != zoomed {
            Self::tmux(&["resize-pane", "-Z", "-t", pane_id])?;
        }
        Ok(())
    }

    /// Kill a seat pane (its window dies with its last pane).
    pub fn kill_pane(&self, pane_id: &str) -> io::Result<()> {
        Self::tmux(&["kill-pane", "-t", pane_id]).map(|_| ())
    }

    /// Move `seat_pane` into the field window as a real tmux split next
    /// to `slot_pane`: `join-pane -h` ("split right"/"split left", side by
    /// side) or `-v` ("split below"/"split top", stacked); `before` adds
    /// `-b`, inserting the seat on the leading side of the split instead of
    /// the trailing one - left of the slot for a horizontal join, above it
    /// for a vertical one (row-drag edge drops are the only
    /// caller that ever passes `true`; the context menu's split right/split
    /// below always join on the trailing side). `seat_pane`/`slot_pane` are
    /// pane ids, so this works whether the seat lives in the field's own
    /// session (an ordinary `seat:*` window) or a foreign one - `join-pane`
    /// moves panes across sessions natively, and seat identity
    /// (`SEAT_OPTION`/`REPO_OPTION`) is stamped on the pane, not the
    /// window, so it survives the move. The seat's origin window dying
    /// when its last pane leaves is correct tmux behavior, not a leak.
    ///
    /// Stamps `@nopal_role=split` on the moved pane so reconcile and
    /// `App::focused_seat` can tell a joined split apart from the true slot
    /// pane - both now share the field window's id, which is exactly the
    /// ambiguity that marker resolves. Then re-asserts the field pane's
    /// fixed `SIDEBAR_COLUMNS` width: `join-pane` re-balances the whole
    /// window's layout, the same way `create_session`'s initial split does,
    /// so the sidebar would otherwise lose its fixed width on every join.
    /// This is an associated function, not a `&self` method: nothing here
    /// needs the field's session name, only the three pane ids the
    /// caller already has in hand.
    pub fn join_seat_split(
        seat_pane: &str,
        slot_pane: &str,
        field_pane: &str,
        horizontal: bool,
        before: bool,
    ) -> io::Result<()> {
        let flag = if horizontal { "-h" } else { "-v" };
        let mut args: Vec<&str> = vec!["join-pane", flag];
        if before {
            args.push("-b");
        }
        args.extend(["-s", seat_pane, "-t", slot_pane]);
        Self::tmux(&args)?;
        Self::tmux(&["set-option", "-p", "-t", seat_pane, ROLE_OPTION, "split"])?;
        let width = SIDEBAR_COLUMNS.to_string();
        Self::tmux(&["resize-pane", "-t", field_pane, "-x", &width])?;
        Ok(())
    }

    /// Move a split-in seat back out to its own fresh background window,
    /// undoing [`Self::join_seat_split`]. `-d` keeps the new window
    /// detached (not switched to) - the caller decides whether to also
    /// focus it afterward ("break to window" does; "return to its window"
    /// does not). Clears the `@nopal_role=split` marker so the pane reads
    /// as an ordinary windowed seat again once reconcile catches up.
    ///
    /// Returns the fresh window's id (`-P -F "#{window_id}"`), the same way
    /// `create_session` captures its field pane id: a caller that needs
    /// to act on the new window immediately (the "break to window" context
    /// action does, to swap the seat straight into the slot) cannot wait
    /// for a reconcile to learn it, and a window id is a valid
    /// `target-window` on its own (no `session:` prefix needed) per tmux's
    /// target resolution rules.
    pub fn break_seat_out(seat_pane: &str, name: &str) -> io::Result<String> {
        let window_name = format!("seat:{name}");
        let window_id = Self::tmux(&[
            "break-pane",
            "-d",
            "-P",
            "-F",
            "#{window_id}",
            "-s",
            seat_pane,
            "-n",
            &window_name,
        ])?;
        Self::tmux(&["set-option", "-p", "-u", "-t", seat_pane, ROLE_OPTION])?;
        Ok(window_id)
    }

    /// Give `pane_id` tmux's active-pane focus without moving it or
    /// changing the active window: used for "open" on a seat already split
    /// into the field window. It sits right next to the field UI pane
    /// already - opening a VT mirror of it would show the same content
    /// twice - so "open" here means simply making it the pane that receives
    /// keystrokes, which is what tmux's active pane already governs.
    /// `target-pane` accepts a bare pane id directly (per tmux's target
    /// resolution rules), so no window lookup is needed.
    pub fn select_pane(pane_id: &str) -> io::Result<()> {
        Self::tmux(&["select-pane", "-t", pane_id]).map(|_| ())
    }

    /// Stamp `@nopal_seat` on a pane, the same identity `spawn_seat` writes
    /// at birth. Used before `join_seat_split` moves an adopted (unstamped)
    /// seat into the field window: its label is derived from window and
    /// session names the join replaces, so the name must be frozen onto the
    /// pane - the one thing that travels - to survive the move and name a
    /// later `break_seat_out` window.
    pub fn stamp_seat_name(pane_id: &str, name: &str) -> io::Result<()> {
        Self::tmux(&["set-option", "-p", "-t", pane_id, SEAT_OPTION, name]).map(|_| ())
    }

    /// Move terminal focus back to the field pane.
    pub fn focus_field(&self, field_pane: &str) -> io::Result<()> {
        Self::tmux(&[
            "select-window",
            "-t",
            &format!("={}:{FIELD_WINDOW}", self.session),
        ])?;
        Self::tmux(&["select-pane", "-t", field_pane]).map(|_| ())
    }

    pub fn kill_session(&self) -> io::Result<()> {
        Self::tmux(&["kill-session", "-t", &format!("={}", self.session)]).map(|_| ())
    }
}

/// Whether a session named `name` exists, server-wide (not scoped to any
/// particular `Backend`). The [`seat::naming::resolve_session_name`]
/// collision probe for the spawn picker; `=` pins an exact match.
///
/// [`seat::naming::resolve_session_name`]: crate::seat::naming::resolve_session_name
pub fn session_exists_named(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", &format!("={name}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn parse_size_pair(text: &str) -> Option<(u16, u16)> {
    let mut parts = text.split_whitespace();
    let width = parts.next()?.parse().ok()?;
    let height = parts.next()?.parse().ok()?;
    if parts.next().is_some() || width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn new_session_args(name: &str, path: &str, size: Option<(u16, u16)>) -> Vec<String> {
    let mut args = vec![
        "new-session".to_owned(),
        "-d".to_owned(),
        "-s".to_owned(),
        name.to_owned(),
        "-c".to_owned(),
        path.to_owned(),
    ];
    if let Some((width, height)) = size {
        args.extend([
            "-x".to_owned(),
            width.to_string(),
            "-y".to_owned(),
            height.to_string(),
        ]);
    }
    args
}

/// Best-effort `zoxide add <path>`, feeding the operator's frecency store.
/// `zoxide` being absent, failing, or refusing the path is a no-op: this is
/// optional interoperability, never a dependency.
pub(crate) fn zoxide_add(path: &str) {
    let _ = Command::new("zoxide").args(["add", path]).output();
}

/// Single-quote `text` for the shell tmux hands a pane command to. Mirrors
/// `cli::quote`'s convention; not shared directly since that helper is
/// private to its module and this crate has no shell-quoting seam yet.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The tmux argv that delivers `agent_cmd` (plus Enter) to a fresh seat's
/// shell. A shell still running its init files eats or mangles
/// keystrokes injected right after `new-session` (observed with zsh +
/// powerlevel10k instant prompt), and there is no clean "init done"
/// signal to wait for, so the send runs server-side (`run-shell -b`,
/// non-blocking, survives a field exit) as a verify-and-retry loop:
/// each attempt clears any half-delivered line (`C-u`), types the
/// command, and the next round stops once the agent is up. A launch that
/// exhausts its attempts degrades to the seat's `○` state, healed by the
/// `s` relaunch key.
///
/// "Agent is up" is a two-signal probe, NOT `pane_current_command`:
/// shell-integration wrappers that proxy command execution onto a
/// separate pty (observed live with kiro-cli-term) keep the pane's
/// foreground command reporting the shell forever. Signal one is cwd:
/// `lsof` lists the pids whose working directory is the seat's path,
/// which stays per-seat accurate across simultaneous spawns. Signal two
/// is the ps title's first-token basename, which must be the pi binary
/// (`NOPAL_PI_BIN` basename or `pi`) or `nopal` - the same matching rule
/// as `feeds::agents`. Neither signal suffices alone: in an end-to-end run,
/// `nopal cli` execs into pi, whose kernel-image name
/// for lsof is `node` (pi is a node script; node rewrites the ps title
/// to `pi`), so an lsof name-list either misses the live agent entirely
/// (`-c nopal`, and the retry loop re-types the command into pi's input)
/// or false-matches the transient node helper kiro-cli-term spawns in
/// the seat's cwd during shell init (`-c node`, and the launch is
/// silently suppressed). Known edge: a matching-titled process merely
/// started from the same directory satisfies the probe and suppresses
/// the launch; `s` recovers. `=name` alone resolves for session targets
/// but not for send-keys' pane target (tmux 3.6a); `=name:` names the
/// session's active pane.
///
/// The `-t` on `run-shell` itself is load-bearing, not decoration: a
/// background job with no explicit target anchors to the invoking
/// client's implied pane, and when the invoker is a command client that
/// already has `TMUX` in its environment - which the field always is,
/// living inside its own session - the job is silently discarded on
/// client exit before its first `sleep` (observed live on tmux 3.6a:
/// rc=0, no job process, no keystrokes ever delivered). Anchoring
/// the job to the seat's own pane makes it independent of the invoking
/// client's lifetime; the job then dies with the seat, which is the
/// lifetime it should have anyway.
fn deferred_agent_send(name: &str, path: &str, agent_cmd: &str) -> Vec<String> {
    let target = shell_quote(&format!("={name}:"));
    let pi_name = std::env::var("NOPAL_PI_BIN")
        .ok()
        .filter(|bin| !bin.is_empty())
        .map(|bin| bin.rsplit('/').next().unwrap_or(&bin).to_owned())
        .unwrap_or_else(|| "pi".to_owned());
    let script = format!(
        "for d in 1.5 2.5 2.5 2.5; do \
         sleep $d; \
         for p in $(lsof -a -d cwd -F pn 2>/dev/null | grep -B1 -xF {cwd_line} | sed -n 's/^p//p'); do \
         t=$(ps -o command= -p \"$p\" 2>/dev/null); t=${{t%% *}}; t=${{t##*/}}; \
         [ \"$t\" = {pi_name} ] || [ \"$t\" = nopal ] && exit 0; \
         done; \
         tmux send-keys -t {target} C-u; \
         tmux send-keys -t {target} {cmd} Enter; \
         done",
        cwd_line = shell_quote(&format!("n{path}")),
        pi_name = shell_quote(&pi_name),
        target = target,
        cmd = shell_quote(agent_cmd)
    );
    vec![
        "run-shell".to_owned(),
        "-b".to_owned(),
        "-t".to_owned(),
        format!("={name}:"),
        script,
    ]
}

/// The reconcile query the sidecar issues at startup, after topology
/// changes, and periodically: a full-server pane snapshot (`-a`), since
/// `%*` subscriptions only push for the attached session (verified on
/// 3.6a). Reply lines match `SEAT_SUBSCRIPTION_FORMAT` and flow through
/// the same reducer as subscription pushes, replacing the inventory.
pub fn reconcile_command() -> String {
    format!(
        "list-panes -a -F \"{}\"",
        crate::state::SEAT_SUBSCRIPTION_FORMAT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_plain_text() {
        assert_eq!(
            shell_quote("/usr/local/bin/nopal cli"),
            "'/usr/local/bin/nopal cli'"
        );
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn parse_size_pair_accepts_one_positive_width_height_pair() {
        assert_eq!(parse_size_pair("211 59"), Some((211, 59)));
        assert_eq!(parse_size_pair("211"), None);
        assert_eq!(parse_size_pair("211 59 extra"), None);
        assert_eq!(parse_size_pair("0 59"), None);
        assert_eq!(parse_size_pair("211 0"), None);
        assert_eq!(parse_size_pair("wide high"), None);
    }

    #[test]
    fn new_session_args_carries_field_window_size_when_available() {
        let args = new_session_args("alpha", "/w/alpha", Some((211, 59)));
        assert_eq!(
            args,
            vec![
                "new-session",
                "-d",
                "-s",
                "alpha",
                "-c",
                "/w/alpha",
                "-x",
                "211",
                "-y",
                "59",
            ]
        );
    }

    #[test]
    fn new_session_args_keeps_size_flags_absent_without_a_valid_size() {
        let args = new_session_args("alpha", "/w/alpha", None);
        assert_eq!(
            args,
            vec!["new-session", "-d", "-s", "alpha", "-c", "/w/alpha"]
        );
    }

    #[test]
    fn deferred_agent_send_retries_clears_and_quotes() {
        let args = deferred_agent_send("alpha", "/w/alpha", "'/opt/nopal' cli");
        // `-t` anchors the background job to the seat's pane: without it,
        // a command client with `TMUX` set (the field, always) has its
        // `-b` job silently discarded before the first sleep.
        assert_eq!(args[..4], ["run-shell", "-b", "-t", "=alpha:"]);
        let script = &args[4];
        assert!(script.contains("for d in 1.5 2.5 2.5 2.5"), "{script}");
        assert!(script.contains("send-keys -t '=alpha:' C-u"), "{script}");
        assert!(
            script.contains("send-keys -t '=alpha:' ''\\''/opt/nopal'\\'' cli' Enter"),
            "{script}"
        );
        // cwd pids from lsof, agent-hood from the ps title's first-token
        // basename (pi or nopal) - see the function doc for why an lsof
        // name-list cannot decide this on its own.
        assert!(script.contains("grep -B1 -xF 'n/w/alpha'"), "{script}");
        assert!(
            script.contains("[ \"$t\" = 'pi' ] || [ \"$t\" = nopal ] && exit 0"),
            "{script}"
        );
    }
}
