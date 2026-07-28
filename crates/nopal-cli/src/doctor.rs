//! Cold explanation surface for evidence-backed gate scaffolding.

use std::io;
use std::path::Path;

use nopal_core::gate_scaffold::{self, GateScaffoldPlan};
use nopal_core::toon;
use serde::Serialize;

pub const DOCTOR_KIND: &str = "nopal.doctor/v1";

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub kind: &'static str,
    pub ok: bool,
    pub gate_scaffold: GateScaffoldPlan,
}

pub fn inspect(root: &Path) -> io::Result<DoctorReport> {
    let gate_scaffold = gate_scaffold::inspect_with_checked_in_authority(root)?;
    Ok(DoctorReport {
        kind: DOCTOR_KIND,
        ok: gate_scaffold.ok,
        gate_scaffold,
    })
}

pub fn to_toon(report: &DoctorReport) -> String {
    let Ok(json) = serde_json::to_value(report) else {
        return "kind: nopal.doctor/v1\nok: false\n".to_owned();
    };
    match toon::from_json(&json) {
        toon::Value::Obj(entries) => toon::encode(&entries),
        _ => "kind: nopal.doctor/v1\nok: false\n".to_owned(),
    }
}
