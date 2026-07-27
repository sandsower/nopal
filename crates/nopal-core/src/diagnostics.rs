//! Diagnostics with stable codes.
//!
//! Codes are part of nopal's public contract from day one (the fix-forward
//! conformance lesson from the beislid trio): consumers match on `code`,
//! never on message text. Messages may improve; codes never change meaning.

use serde::Serialize;

use crate::toon::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    ManifestMissing,
    ManifestParseError,
    VersionUnsupported,
    ProfileUnknown,
    ModuleMissing,
    ModuleParseError,
    DuplicateId,
    StageUnknown,
    CommandMissing,
    CommandConflict,
    CommandInvalid,
    PlaceholderInvalid,
    PlaceholderUnknown,
    GateRefUnknown,
    GateSetUnknown,
    FieldInvalid,
    WorkflowEventUnknown,
    WorkflowActionTypeUnknown,
    IntegrationProviderInvalid,
    GuidanceAuthorityInvalid,
    PolicyShapeInvalid,
    PolicyModeUnknown,
    PolicyRuleInvalid,
    PolicyRuleDuplicateId,
    PolicyDecisionInvalid,
    PolicyPlacementInvalid,
    PolicyClassUnknown,
    PolicyEnvInvalid,
    PolicyKeyUnknown,
    RunIdInvalid,
    RunIdCollision,
    RunNotFound,
    RunAmbiguous,
    LedgerStatusInvalid,
    LedgerEntryInvalid,
    AskIdInvalid,
    AskIdCollision,
    AskNotFound,
    AskEntryInvalid,
    AskStateInvalid,
    AskDecisionInvalid,
    AskAlreadyResolved,
    AskExpired,
    ProcessArtifactMissing,
    ProcessArtifactParseError,
    ProcessArtifactDrift,
    ProcessArtifactRedacted,
    BeislidImportParseError,
    BeislidImportUnsupported,
    BeislidImportOverwriteBlocked,
    BeislidImportMissing,
    BeislidImportCheckParseError,
    BeislidImportDrift,
    BundleMissing,
    BundleParseError,
    BundleResourceMissing,
    BundleAmbientKindUnknown,
    DistributionLockMissing,
    DistributionLockParseError,
    DistributionLockDrift,
    DistributionPackageInvalid,
    DistributionPackageMissing,
    DistributionSourceUnsupported,
    DistributionIntegrityMismatch,
    DistributionBoundaryFailure,
    ScaffoldIncomplete,
    ScaffoldLegacyDetected,
    FieldRondoFeedAbsent,
    FieldRondoFeedUnreadable,
    FieldRondoUnmatched,
    FieldPartialCoverage,
    PlotNotFound,
    PlotSnapshotInvalid,
    PlotEstablishmentEventInvalid,
    PlotEstablishmentConflict,
    PlotSessionWorkspaceConflict,
    ScaffoldDefaults,
    ScaffoldTemplateInvalid,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::ManifestMissing => "manifest_missing",
            Code::ManifestParseError => "manifest_parse_error",
            Code::VersionUnsupported => "version_unsupported",
            Code::ProfileUnknown => "profile_unknown",
            Code::ModuleMissing => "module_missing",
            Code::ModuleParseError => "module_parse_error",
            Code::DuplicateId => "duplicate_id",
            Code::StageUnknown => "stage_unknown",
            Code::CommandMissing => "command_missing",
            Code::CommandConflict => "command_conflict",
            Code::CommandInvalid => "command_invalid",
            Code::PlaceholderInvalid => "placeholder_invalid",
            Code::PlaceholderUnknown => "placeholder_unknown",
            Code::GateRefUnknown => "gate_ref_unknown",
            Code::GateSetUnknown => "gate_set_unknown",
            Code::FieldInvalid => "field_invalid",
            Code::WorkflowEventUnknown => "workflow_event_unknown",
            Code::WorkflowActionTypeUnknown => "workflow_action_type_unknown",
            Code::IntegrationProviderInvalid => "integration_provider_invalid",
            Code::GuidanceAuthorityInvalid => "guidance_authority_invalid",
            Code::PolicyShapeInvalid => "policy_shape_invalid",
            Code::PolicyModeUnknown => "policy_mode_unknown",
            Code::PolicyRuleInvalid => "policy_rule_invalid",
            Code::PolicyRuleDuplicateId => "policy_rule_duplicate_id",
            Code::PolicyDecisionInvalid => "policy_decision_invalid",
            Code::PolicyPlacementInvalid => "policy_placement_invalid",
            Code::PolicyClassUnknown => "policy_class_unknown",
            Code::PolicyEnvInvalid => "policy_env_invalid",
            Code::PolicyKeyUnknown => "policy_key_unknown",
            Code::RunIdInvalid => "run_id_invalid",
            Code::RunIdCollision => "run_id_collision",
            Code::RunNotFound => "run_not_found",
            Code::RunAmbiguous => "run_ambiguous",
            Code::LedgerStatusInvalid => "ledger_status_invalid",
            Code::LedgerEntryInvalid => "ledger_entry_invalid",
            Code::AskIdInvalid => "ask_id_invalid",
            Code::AskIdCollision => "ask_id_collision",
            Code::AskNotFound => "ask_not_found",
            Code::AskEntryInvalid => "ask_entry_invalid",
            Code::AskStateInvalid => "ask_state_invalid",
            Code::AskDecisionInvalid => "ask_decision_invalid",
            Code::AskAlreadyResolved => "ask_already_resolved",
            Code::AskExpired => "ask_expired",
            Code::ProcessArtifactMissing => "process_artifact_missing",
            Code::ProcessArtifactParseError => "process_artifact_parse_error",
            Code::ProcessArtifactDrift => "process_artifact_drift",
            Code::ProcessArtifactRedacted => "process_artifact_redacted",
            Code::BeislidImportParseError => "beislid_import_parse_error",
            Code::BeislidImportUnsupported => "beislid_import_unsupported",
            Code::BeislidImportOverwriteBlocked => "beislid_import_overwrite_blocked",
            Code::BeislidImportMissing => "beislid_import_missing",
            Code::BeislidImportCheckParseError => "beislid_import_check_parse_error",
            Code::BeislidImportDrift => "beislid_import_drift",
            Code::BundleMissing => "bundle_missing",
            Code::BundleParseError => "bundle_parse_error",
            Code::BundleResourceMissing => "bundle_resource_missing",
            Code::BundleAmbientKindUnknown => "bundle_ambient_kind_unknown",
            Code::DistributionLockMissing => "distribution_lock_missing",
            Code::DistributionLockParseError => "distribution_lock_parse_error",
            Code::DistributionLockDrift => "distribution_lock_drift",
            Code::DistributionPackageInvalid => "distribution_package_invalid",
            Code::DistributionPackageMissing => "distribution_package_missing",
            Code::DistributionSourceUnsupported => "distribution_source_unsupported",
            Code::DistributionIntegrityMismatch => "distribution_integrity_mismatch",
            Code::DistributionBoundaryFailure => "distribution_boundary_failure",
            Code::ScaffoldIncomplete => "scaffold_incomplete",
            Code::ScaffoldLegacyDetected => "scaffold_legacy_detected",
            Code::FieldRondoFeedAbsent => "field_rondo_feed_absent",
            Code::FieldRondoFeedUnreadable => "field_rondo_feed_unreadable",
            Code::FieldRondoUnmatched => "field_rondo_unmatched",
            Code::FieldPartialCoverage => "field_partial_coverage",
            Code::PlotNotFound => "plot_not_found",
            Code::PlotSnapshotInvalid => "plot_snapshot_invalid",
            Code::PlotEstablishmentEventInvalid => "plot_establishment_event_invalid",
            Code::PlotEstablishmentConflict => "plot_establishment_conflict",
            Code::PlotSessionWorkspaceConflict => "plot_session_workspace_conflict",
            Code::ScaffoldDefaults => "scaffold_defaults",
            Code::ScaffoldTemplateInvalid => "scaffold_template_invalid",
        }
    }
}

/// 1-based line/column, present when the underlying parser reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Code,
    /// Project-relative path of the file the diagnostic is about.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    pub message: String,
}

impl Diagnostic {
    fn new(
        severity: Severity,
        code: Code,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Diagnostic {
            severity,
            code,
            path: path.into(),
            position: None,
            message: message.into(),
        }
    }

    pub fn error(code: Code, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, path, message)
    }

    pub fn warning(code: Code, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, path, message)
    }

    pub fn with_position(mut self, line: usize, column: usize) -> Self {
        self.position = Some(Position { line, column });
        self
    }
}

/// The one TOON table builder for diagnostics lists. Every envelope renders
/// diagnostics through this so the columns can never drift between commands
/// or crates.
pub fn toon_table(diagnostics: &[Diagnostic]) -> Value {
    Value::Table {
        fields: vec![
            "severity".into(),
            "code".into(),
            "path".into(),
            "position".into(),
            "message".into(),
        ],
        rows: diagnostics
            .iter()
            .map(|d| {
                vec![
                    Value::str(d.severity.as_str()),
                    Value::str(d.code.as_str()),
                    Value::str(d.path.clone()),
                    Value::str(
                        d.position
                            .map_or("-".to_owned(), |p| format!("{}:{}", p.line, p.column)),
                    ),
                    Value::str(d.message.clone()),
                ]
            })
            .collect(),
    }
}

/// Deterministic report order: path, then position, then code.
pub fn sort(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.position.cmp(&b.position))
            .then(a.code.cmp(&b.code))
    });
}
