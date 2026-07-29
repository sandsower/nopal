//! nopal-core: the deterministic core behind the `nopal` CLI.
//!
//! The config modules are pure with respect to the outside world: functions
//! take paths or parsed values in and return typed results and diagnostics
//! out. No network, no shell, no agent calls - by contract. Three
//! modules are deliberate exceptions, still without network or agent calls:
//! `run_ledger*` (durable state under the state dir, probes git),
//! `discover` (probes git to find the enclosing repo toplevel), and
//! `scaffold` (writes `.nopal/` defaults under the project root on
//! first real launch).

pub mod beislid_import;
pub mod bundle;
pub mod config;
pub mod confined_read;
pub mod diagnostics;
pub mod discover;
pub mod distribution;
pub mod enforcement;
pub mod gate_scaffold;
pub mod gates;
pub mod gates_report;
pub mod guidance;
pub mod isolation;
pub mod policy;
pub mod process_artifact;
pub mod profile;
pub mod review_policy;
pub mod roots;
pub mod run_ledger;
pub mod run_ledger_report;
pub mod run_ledger_store;
pub mod scaffold;
pub mod selection;
pub mod status;
pub mod toon;
pub mod validate;
pub mod workflow;
