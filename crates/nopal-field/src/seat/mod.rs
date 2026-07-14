//! Native seat lifecycle: config, spawn candidates, worktree creation,
//! and session naming.
//!
//! Field-internal on purpose: this is promoted to
//! a nopal-core seam or CLI surface only once a second consumer
//! actually needs it. Everything here is pure or a thin, testable
//! wrapper over `git`; the tmux side of spawn/kill/relaunch lives in
//! `tmux::Backend` and the UI wiring lives in `app`/`ui` - neither is
//! touched by this module.

pub mod candidates;
pub mod config;
pub mod naming;
pub mod worktree;

pub use candidates::{
    Candidate, CandidateKind, CandidateSource, ProjectSource, RegistrySource, WorktreeSource,
    discover_projects, merge,
};
pub use config::SeatConfig;
pub use naming::resolve_session_name;
