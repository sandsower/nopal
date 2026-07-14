//! Versioned reports for Plot Establishment commands.

use std::path::Path;

use serde::Serialize;

use crate::diagnostics::{self, Code, Diagnostic};
use crate::plot::{PlotDocument, SessionProtocolEndpoint};
use crate::plot_establishment::{ApplyOutcome, EstablishmentError, ResolveError};
use crate::plot_store::{self, EstablishStoreError, PlotEnv};
use crate::toon::{self, Value};

pub const ESTABLISHMENT_REPORT_KIND: &str = "nopal.plot_establishment/v1";

#[derive(Debug, Clone, Serialize)]
pub struct EstablishmentReport {
    pub kind: &'static str,
    pub ok: bool,
    pub outcome: Option<String>,
    pub plot: Option<PlotDocument>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn establish(
    state_dir: Option<&Path>,
    plot_id: Option<&str>,
    field_session: &str,
    event: &str,
    workspace: &Path,
    host_session: &str,
    host_pane: Option<&str>,
) -> EstablishmentReport {
    establish_with_protocol(
        state_dir,
        plot_id,
        field_session,
        event,
        workspace,
        host_session,
        host_pane,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn establish_with_protocol(
    state_dir: Option<&Path>,
    plot_id: Option<&str>,
    field_session: &str,
    event: &str,
    workspace: &Path,
    host_session: &str,
    host_pane: Option<&str>,
    protocol: Option<SessionProtocolEndpoint>,
) -> EstablishmentReport {
    let env = PlotEnv::discover(state_dir);
    let plot_id = match plot_id {
        Some(plot_id) => plot_id.to_owned(),
        None => match plot_store::selected_for_field_session(&env, field_session) {
            Ok(Some(plot)) => plot.plot_id,
            Ok(None) => {
                return failure(
                    Code::PlotNotFound,
                    field_session,
                    "the Field session has no selected Plot",
                );
            }
            Err(error) => {
                return failure(Code::PlotSnapshotInvalid, field_session, error.to_string());
            }
        },
    };
    let frozen_workflow = plot_store::load_plot(&env, &plot_id)
        .ok()
        .and_then(|plot| plot.establishment.map(|value| value.effective_workflow));
    let input = match frozen_workflow {
        Some(workflow) => crate::plot_establishment::resolve_contribution_input(
            workspace,
            event,
            host_session,
            host_pane,
            workflow,
        ),
        None => crate::plot_establishment::resolve_input(workspace, event, host_session, host_pane),
    };
    let input = match input {
        Ok(mut input) => {
            input.protocol = protocol;
            input
        }
        Err(error) => return resolve_failure(workspace, error),
    };
    match plot_store::establish(&env, &plot_id, input) {
        Ok((plot, outcome)) => EstablishmentReport {
            kind: ESTABLISHMENT_REPORT_KIND,
            ok: true,
            outcome: Some(outcome_name(outcome).to_owned()),
            plot: Some(plot),
            diagnostics: Vec::new(),
        },
        Err(error) => store_failure(&plot_id, error),
    }
}

pub fn failure(
    code: Code,
    path: impl Into<String>,
    message: impl Into<String>,
) -> EstablishmentReport {
    EstablishmentReport {
        kind: ESTABLISHMENT_REPORT_KIND,
        ok: false,
        outcome: None,
        plot: None,
        diagnostics: vec![Diagnostic::error(code, path, message)],
    }
}

fn resolve_failure(path: &Path, error: ResolveError) -> EstablishmentReport {
    let code = match error {
        ResolveError::EventNotAllowed { .. } => Code::PlotEstablishmentEventInvalid,
        _ => Code::PlotSnapshotInvalid,
    };
    failure(code, path.display().to_string(), error.to_string())
}

fn store_failure(plot_id: &str, error: EstablishStoreError) -> EstablishmentReport {
    let code = match &error {
        EstablishStoreError::Domain(EstablishmentError::SessionWorkspaceConflict { .. }) => {
            Code::PlotSessionWorkspaceConflict
        }
        EstablishStoreError::Domain(_) => Code::PlotEstablishmentConflict,
        EstablishStoreError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Code::PlotNotFound
        }
        EstablishStoreError::Io(_) => Code::PlotSnapshotInvalid,
    };
    failure(code, plot_id, error.to_string())
}

fn outcome_name(outcome: ApplyOutcome) -> &'static str {
    match outcome {
        ApplyOutcome::Established => "established",
        ApplyOutcome::Extended => "extended",
        ApplyOutcome::Unchanged => "unchanged",
    }
}

pub fn establishment_toon(report: &EstablishmentReport) -> String {
    let plot = report.plot.as_ref();
    let document = vec![
        ("kind".to_owned(), Value::str(report.kind)),
        ("ok".to_owned(), Value::Bool(report.ok)),
        (
            "outcome".to_owned(),
            Value::str(report.outcome.as_deref().unwrap_or("-")),
        ),
        (
            "plot_id".to_owned(),
            Value::str(plot.map_or("-", |plot| plot.plot_id.as_str())),
        ),
        (
            "repositories_total".to_owned(),
            Value::Int(plot.map_or(0, |plot| plot.repositories.len()) as i64),
        ),
        (
            "workspaces_total".to_owned(),
            Value::Int(plot.map_or(0, |plot| plot.workspaces.len()) as i64),
        ),
        (
            "sessions_total".to_owned(),
            Value::Int(plot.map_or(0, |plot| plot.sessions.len()) as i64),
        ),
        (
            "diagnostics".to_owned(),
            diagnostics::toon_table(&report.diagnostics),
        ),
    ];
    toon::encode(&document)
}
