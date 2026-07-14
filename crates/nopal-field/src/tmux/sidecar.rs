//! The `tmux -C` control-mode sidecar: the field's push-based state feed.
//!
//! Attaches a control-mode client with `-f no-output` (no pane-output
//! firehose) and installs a `-B` format subscription over all panes, so
//! seat state arrives as `%subscription-changed` pushes - never by
//! polling `capture-pane` or scraping screens. The same client doubles as
//! a command channel: reconcile queries write to its stdin and their
//! `%begin`/`%end` replies flow through the notification parser.

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;

use crate::AppEvent;
use crate::notify::Parser;
use crate::state::{SEAT_SUBSCRIPTION_FORMAT, SEAT_SUBSCRIPTION_NAME};

pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    reader: Option<JoinHandle<()>>,
}

impl Sidecar {
    /// Attach to `session` and start pumping notifications into `tx`.
    pub fn attach(session: &str, tx: Sender<AppEvent>) -> io::Result<Self> {
        let mut child = Command::new("tmux")
            .args([
                "-C",
                "attach-session",
                "-f",
                "no-output",
                "-t",
                &format!("={session}"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("sidecar stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("sidecar stdout unavailable"))?;

        // Subscribe to seat state over every pane in the session; pushes
        // arrive as %subscription-changed and reuse the reducer's format.
        writeln!(
            stdin,
            "refresh-client -B {SEAT_SUBSCRIPTION_NAME}:%*:\"{SEAT_SUBSCRIPTION_FORMAT}\""
        )?;

        let reader = std::thread::spawn(move || {
            let mut parser = Parser::new();
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(notification) = parser.feed(&line)
                    && tx.send(AppEvent::Tmux(notification)).is_err()
                {
                    break;
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            reader: Some(reader),
        })
    }

    /// Write one command down the control channel; its reply arrives as a
    /// `CommandReply` notification.
    pub fn send(&mut self, command: &str) -> io::Result<()> {
        writeln!(self.stdin, "{command}")
    }

    /// Ask tmux for the full server-wide pane inventory. Needed because
    /// subscription pushes do not re-fire when pane options change after
    /// creation, and never fire for foreign sessions (verified on 3.6a).
    pub fn reconcile(&mut self) -> io::Result<()> {
        let command = super::reconcile_command();
        self.send(&command)
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // detach-client ends the control client cleanly; fall back to kill.
        let _ = self.send("detach-client");
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
