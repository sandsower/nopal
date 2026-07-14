//! Nopal Field: the tmux-backed field surface.
//!
//! Seats are real tmux panes rendered by the outer terminal; the field
//! is a ratatui app in its own tmux pane; a `tmux -C` control-mode sidecar
//! (`-f no-output`, `-B` format subscriptions) supplies field events. The
//! field renders and routes, never decides.

pub mod app;
pub mod bench;
pub mod cli;
pub mod embed;
pub mod feeds;
pub mod keys;
pub mod notify;
pub mod registry;
pub mod seat;
pub mod state;
pub mod tmux;
pub mod ui;

/// Every input the app loop reduces, from any thread.
#[derive(Debug)]
pub enum AppEvent {
    /// A tmux control-mode notification from the sidecar.
    Tmux(notify::Notification),
    /// A feed adapter result.
    Feed(state::FeedEvent),
    /// A terminal input event in the field pane.
    Input(crossterm::event::Event),
    /// A chunk of live output from the embedded-seat pipe.
    Embed(embed::EmbedChunk),
}
