use std::fs;
use std::process::Command;

fn write_repository(root: &std::path::Path) -> std::io::Result<()> {
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()?
            .success()
    );
    let nopal = root.join(".nopal");
    fs::create_dir(&nopal)?;
    fs::write(
        nopal.join("nopal.jsonc"),
        r#"{"version":"nopal.project/v1","profile":"minimal"}"#,
    )?;
    fs::write(
        nopal.join("workflow.jsonc"),
        r#"{
            "version":"nopal.workflow/v1",
            "establishment":{"events":["kickoff_context_ready"]}
        }"#,
    )?;
    fs::write(
        nopal.join("roots.jsonc"),
        r#"{
            "version":"nopal.roots/v1",
            "roots":[{
                "id":"quality","statement":"Quality stays green",
                "proof_requirements":[{
                    "id":"proof","stage":"pre_pr","required":true,
                    "gates":["test"],"on_missing":"block","on_failure":"block"
                }]
            }]
        }"#,
    )?;
    fs::write(
        nopal.join("gates.jsonc"),
        r#"{"version":"nopal.gates/v1","gates":[{"id":"test","stage":"pre_pr","command":"cargo test"}]}"#,
    )?;
    Ok(())
}

fn establish(
    root: &std::path::Path,
    state: &std::path::Path,
    plot_id: &str,
    workspace: &std::path::Path,
) -> std::io::Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_nopal"))
        .arg("--dir")
        .arg(root)
        .args(["--json", "plot", "establish", "--state-dir"])
        .arg(state)
        .args([
            "--plot-id",
            plot_id,
            "--event",
            "kickoff_context_ready",
            "--workspace",
        ])
        .arg(workspace)
        .args(["--host-session", "nopal-work", "--host-pane", "%4"])
        .output()
}

fn establish_with_protocol(
    root: &std::path::Path,
    state: &std::path::Path,
    plot_id: &str,
    workspace: &std::path::Path,
    address: &str,
    protocol_state: &str,
) -> std::io::Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nopal"));
    command
        .arg("--dir")
        .arg(root)
        .args(["--json", "plot", "establish", "--state-dir"])
        .arg(state)
        .args([
            "--plot-id",
            plot_id,
            "--event",
            "kickoff_context_ready",
            "--workspace",
        ])
        .arg(workspace)
        .args([
            "--host-session",
            "nopal-work",
            "--host-pane",
            "%4",
            "--protocol-address",
            address,
            "--protocol-state",
            protocol_state,
        ]);
    command.output()
}

#[allow(clippy::too_many_arguments)]
fn establish_with_protocol_kind(
    root: &std::path::Path,
    state: &std::path::Path,
    plot_id: &str,
    workspace: &std::path::Path,
    protocol_kind: &str,
    address: &str,
    protocol_state: &str,
) -> std::io::Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nopal"));
    command
        .arg("--dir")
        .arg(root)
        .args(["--json", "plot", "establish", "--state-dir"])
        .arg(state)
        .args([
            "--plot-id",
            plot_id,
            "--event",
            "kickoff_context_ready",
            "--workspace",
        ])
        .arg(workspace)
        .args([
            "--host-session",
            "nopal-work",
            "--host-pane",
            "%4",
            "--protocol-kind",
            protocol_kind,
            "--protocol-address",
            address,
            "--protocol-state",
            protocol_state,
        ]);
    command.output()
}

#[test]
fn plot_establishment_cli_preserves_identity_replays_and_rejects_workspace_moves()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    write_repository(directory.path())?;
    let state = directory.path().join("state");
    let env = nopal_core::plot_store::PlotEnv::discover(Some(&state));
    let provisional = nopal_core::plot_store::ensure_provisional(&env, "nopal")?;
    let provisional =
        nopal_core::plot_store::bind_session(&env, &provisional.plot_id, "nopal-work", Some("%4"))?;

    let first = establish(
        directory.path(),
        &state,
        &provisional.plot_id,
        directory.path(),
    )?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    assert_eq!(first["kind"], "nopal.plot_establishment/v1");
    assert_eq!(first["outcome"], "established");
    assert_eq!(first["plot"]["plot_id"], provisional.plot_id);
    assert_eq!(
        first["plot"]["seed"],
        serde_json::json!({"source":"field_open","text":""})
    );
    assert_eq!(
        first["plot"]["sessions"][0]["session_id"],
        provisional.sessions[0].session_id
    );
    assert_eq!(first["plot"]["provisional"], false);

    let replay = establish(
        directory.path(),
        &state,
        &provisional.plot_id,
        directory.path(),
    )?;
    assert!(replay.status.success());
    let replay: serde_json::Value = serde_json::from_slice(&replay.stdout)?;
    assert_eq!(replay["outcome"], "unchanged");

    let second_workspace = directory.path().join("other-workspace");
    fs::create_dir(&second_workspace)?;
    let conflict = establish(
        directory.path(),
        &state,
        &provisional.plot_id,
        &second_workspace,
    )?;
    assert_eq!(conflict.status.code(), Some(1));
    let conflict: serde_json::Value = serde_json::from_slice(&conflict.stdout)?;
    assert_eq!(conflict["ok"], false);
    assert_eq!(
        conflict["diagnostics"][0]["code"],
        "plot_session_workspace_conflict"
    );
    Ok(())
}

#[test]
fn plot_establishment_cli_binds_and_updates_the_structured_protocol_endpoint()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    write_repository(directory.path())?;
    let state = directory.path().join("state");
    let env = nopal_core::plot_store::PlotEnv::discover(Some(&state));
    let provisional = nopal_core::plot_store::ensure_provisional(&env, "nopal")?;
    let provisional =
        nopal_core::plot_store::bind_session(&env, &provisional.plot_id, "nopal-work", Some("%4"))?;

    let first = establish_with_protocol(
        directory.path(),
        &state,
        &provisional.plot_id,
        directory.path(),
        "/tmp/nopal-session-1.sock",
        "starting",
    )?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout)?;
    assert_eq!(first["outcome"], "established");
    assert_eq!(
        first["plot"]["sessions"][0]["protocol"],
        serde_json::json!({
            "kind": "nopal.session/v4",
            "transport": "unix",
            "address": "/tmp/nopal-session-1.sock",
            "state": "starting"
        })
    );

    let ready = establish_with_protocol(
        directory.path(),
        &state,
        &provisional.plot_id,
        directory.path(),
        "/tmp/nopal-session-1.sock",
        "ready",
    )?;
    assert!(ready.status.success());
    let ready: serde_json::Value = serde_json::from_slice(&ready.stdout)?;
    assert_eq!(ready["outcome"], "extended");
    assert_eq!(ready["plot"]["sessions"][0]["protocol"]["state"], "ready");

    let persisted = nopal_core::plot_store::load_plot(&env, &provisional.plot_id)?;
    assert_eq!(
        persisted.sessions[0].protocol.as_ref().unwrap().state,
        "ready"
    );
    Ok(())
}

#[test]
fn plot_establishment_cli_keeps_explicit_v2_endpoints_readable()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    write_repository(directory.path())?;
    let state = directory.path().join("state");
    let env = nopal_core::plot_store::PlotEnv::discover(Some(&state));
    let provisional = nopal_core::plot_store::ensure_provisional(&env, "nopal")?;
    nopal_core::plot_store::bind_session(&env, &provisional.plot_id, "nopal-work", Some("%4"))?;

    let output = establish_with_protocol_kind(
        directory.path(),
        &state,
        &provisional.plot_id,
        directory.path(),
        "nopal.session/v2",
        "/tmp/nopal-session-v2.sock",
        "ready",
    )?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        report["plot"]["sessions"][0]["protocol"]["kind"],
        "nopal.session/v2"
    );
    let persisted = nopal_core::plot_store::load_plot(&env, &provisional.plot_id)?;
    assert_eq!(
        persisted.sessions[0]
            .protocol
            .as_ref()
            .expect("v2 endpoint remains readable")
            .kind,
        "nopal.session/v2"
    );
    Ok(())
}

#[test]
fn plot_establishment_cli_preserves_safe_future_kinds_and_rejects_blank_kind()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    write_repository(directory.path())?;
    let state = directory.path().join("state");
    let env = nopal_core::plot_store::PlotEnv::discover(Some(&state));
    let provisional = nopal_core::plot_store::ensure_provisional(&env, "nopal")?;
    nopal_core::plot_store::bind_session(&env, &provisional.plot_id, "nopal-work", Some("%4"))?;

    let future = establish_with_protocol_kind(
        directory.path(),
        &state,
        &provisional.plot_id,
        directory.path(),
        "nopal.session/v99-preview",
        "/tmp/nopal-session-future.sock",
        "starting",
    )?;
    assert!(future.status.success());
    let future: serde_json::Value = serde_json::from_slice(&future.stdout)?;
    assert_eq!(
        future["plot"]["sessions"][0]["protocol"]["kind"],
        "nopal.session/v99-preview"
    );

    let blank = establish_with_protocol_kind(
        directory.path(),
        &state,
        &provisional.plot_id,
        directory.path(),
        "   ",
        "/tmp/nopal-session-future.sock",
        "ready",
    )?;
    assert_eq!(blank.status.code(), Some(1));
    let blank: serde_json::Value = serde_json::from_slice(&blank.stdout)?;
    assert_eq!(blank["ok"], false);
    assert_eq!(
        blank["diagnostics"][0]["code"],
        "plot_establishment_conflict"
    );

    let persisted = nopal_core::plot_store::load_plot(&env, &provisional.plot_id)?;
    assert_eq!(
        persisted.sessions[0]
            .protocol
            .as_ref()
            .expect("future protocol remains")
            .kind,
        "nopal.session/v99-preview"
    );
    Ok(())
}
