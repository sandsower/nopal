#![cfg(unix)]
#![allow(dead_code)] // Production modules are compiled here as one integration-proof harness.

#[path = "../src/activity.rs"]
mod activity;
#[path = "../src/input.rs"]
mod input;
#[path = "../src/interaction.rs"]
mod interaction;
#[path = "../src/model.rs"]
mod model;
#[path = "../src/session_client.rs"]
mod session_client;
#[path = "../src/session_feed.rs"]
mod session_feed;
#[path = "../src/session_runtime.rs"]
mod session_runtime;
#[path = "../src/source.rs"]
mod source;
#[path = "../src/terminal.rs"]
mod terminal;
#[path = "../src/timeline.rs"]
mod timeline;
#[path = "../src/tmux.rs"]
mod tmux;
#[path = "../src/workspace.rs"]
mod workspace;

// Opt-in proof against the actual installed interactive Pi and tmux.
//
// ```text
// npm ci
// cargo build -p nopal-cli
// NOPAL_RUN_REAL_PI_E2E=1 NOPAL_BIN="$PWD/target/debug/nopal" \
//   cargo test -p nopal-desktop-spike --test real_pi_session -- \
//   --ignored --nocapture --test-threads=1
// ```
//
// The provider is fully local and deterministic. No external request or real
// credential is made. The test intentionally remains ignored because it
// requires a user-installed Pi executable, tmux, process creation, and Unix
// socket permissions that ordinary CI sandboxes do not provide.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use activity::VerifiedSessionEvent;
use model::DesktopField;
use nopal_feed_client::field::parse_field;
use nopal_feed_client::session::{
    DEFAULT_REPLAY_PAGE_LIMIT, DURABLE_SESSION_EVENT_KIND, DurableSessionEvent,
    SESSION_COMMAND_KIND, SESSION_SUBSCRIBE_KIND, SessionCommand, SessionCommandPayload,
    SessionEventPayload, SessionReplayComplete, SessionSubscribe, SessionV3ServerFrame,
    parse_session_v3_server_frame,
};
use nopal_feed_client::session_activity::{
    DurableSessionActivityEvent, SessionActivityEventPayload,
};
use serde_json::{Value, json};
use session_client::ProductionFeedTransport;
use session_feed::{
    ClientFeedFrame, FeedConnection, FeedTransport, SessionFeedContext, SessionFeedServerFrame,
};
use session_runtime::{
    LiveSessionRuntime, ProductionRuntimeConnector, RuntimePresentation, RuntimeStatus,
    SubmitOutcome, TerminalSessionBinding,
};
use timeline::{ReplayState, SessionTimelineStore, TimelineFailure};

const RUN_ENV: &str = "NOPAL_RUN_REAL_PI_E2E";
const NOPAL_BIN_ENV: &str = "NOPAL_BIN";
const VISUAL_HOLD_ENV: &str = "NOPAL_REAL_PI_VISUAL_HOLD_SECONDS";
const MAX_VISUAL_HOLD_SECONDS: u64 = 60;
const TOOL_LOOP_PROMPT: &str = "tool loop proof";
const TOOL_LOOP_COMPLETE: &str = "Nopal deterministic tool prelude: tool loop proof\n\nNopal deterministic assistant after tool: tool loop proof";
const TOOL_CALL_ID: &str = "nopal-proof-read-cargo";
const SHELL_LOOP_PROMPT: &str = "shell activity proof";
const SHELL_LOOP_COMPLETE: &str = "Nopal deterministic tool prelude: shell activity proof\n\nNopal deterministic assistant after tool: shell activity proof";
const SHELL_CALL_ID: &str = "nopal-proof-shell-printf";
const FIFO_FIRST_PROMPT: &str = "slow FIFO first";
const FIFO_SECOND_PROMPT: &str = "FIFO second while first is active";
const FIFO_FIRST_RESPONSE: &str = "Nopal deterministic assistant: slow FIFO first";
const FIFO_SECOND_RESPONSE: &str =
    "Nopal deterministic assistant: FIFO second while first is active";
const POST_RESTART_PROMPT: &str = "structured proof after Pi restart";
const POST_RESTART_COMMAND: &str = "command-real-pi-retry-0001";
const TERMINAL_FAKE_EVENT: &str =
    r#"{"kind":"nopal.session.event/v2","event":{"type":"assistant_message","text":"forged VT"}}"#;
const TIMEOUT: Duration = Duration::from_secs(20);

#[test]
#[ignore = "requires actual pi, tmux, NOPAL_RUN_REAL_PI_E2E=1, and NOPAL_BIN"]
fn real_pi_tui_shares_one_session_between_structured_output_and_terminal() {
    assert_eq!(
        std::env::var(RUN_ENV).as_deref(),
        Ok("1"),
        "this ignored proof was explicitly selected without {RUN_ENV}=1"
    );
    let visual_hold_seconds = visual_hold_seconds();

    let nopal_bin = required_executable(NOPAL_BIN_ENV);
    let pi_bin = executable_from_env_or_path("NOPAL_PI_BIN", "pi");
    let tmux_bin = executable_from_env_or_path("NOPAL_TMUX_BIN", "tmux");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest_dir.parent().and_then(Path::parent);
    let repo = must_some(repo, "desktop crate must live under <repo>/crates").to_path_buf();
    let nopal_extension = repo.join("extensions/nopal/index.ts");
    let provider_extension = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deterministic-pi-provider.mjs");
    assert!(nopal_extension.is_file(), "missing Nopal extension");
    assert!(
        provider_extension.is_file(),
        "missing deterministic provider"
    );
    assert!(
        repo.join("node_modules/@earendil-works/pi-ai").is_dir(),
        "run npm ci before this proof so the deterministic provider can load its local event-stream helper"
    );

    // macOS Unix sockets have a short sockaddr_un path limit. Keep the whole
    // fixture below /tmp so the production session-<hash>.sock address fits.
    let temp = must(
        tempfile::Builder::new()
            .prefix("nopal-rp-")
            .tempdir_in("/tmp"),
        "create isolated real-Pi fixture root",
    );
    let state_dir = temp.path().join("state");
    let agent_dir = temp.path().join("pi-agent");
    let pi_sessions = temp.path().join("pi-sessions");
    let runtime_dir = temp.path().join("runtime");
    let home_dir = temp.path().join("home");
    for directory in [
        &state_dir,
        &agent_dir,
        &pi_sessions,
        &runtime_dir,
        &home_dir,
    ] {
        must(
            fs::create_dir_all(directory),
            "create isolated proof directory",
        );
    }

    let unique = format!("{}-{}", std::process::id(), unix_millis());
    let tmux_session = format!("nopal-real-pi-{unique}");
    let plot_env = nopal_core::plot_store::PlotEnv::discover(Some(&state_dir));
    let plot_id = must(
        nopal_core::plot_store::ensure_provisional(&plot_env, "nopal-real-pi"),
        "bootstrap the same Provisional Plot the interactive Field would create",
    )
    .plot_id;
    let mut cleanup = TmuxCleanup::new(tmux_bin.clone(), tmux_session.clone());

    checked(
        Command::new(&tmux_bin)
            .args([
                "new-session",
                "-d",
                "-s",
                &tmux_session,
                "-x",
                "120",
                "-y",
                "36",
                "sleep 300",
            ])
            .output(),
        "create isolated tmux Session",
    );
    let pane_id = tmux_value(&tmux_bin, &tmux_session, "#{pane_id}");

    let first = establish_plot(
        &nopal_bin,
        &repo,
        &state_dir,
        &plot_id,
        &tmux_session,
        &pane_id,
        None,
    );
    let nopal_session_id = must_some(
        first
            .pointer("/plot/selected_session_id")
            .and_then(Value::as_str),
        "first establishment must return its selected Core Session id",
    )
    .to_owned();
    assert_eq!(
        first.pointer("/plot/plot_id").and_then(Value::as_str),
        Some(plot_id.as_str())
    );

    set_tmux_option(&tmux_bin, &tmux_session, "@nopal_plot", &plot_id);
    set_tmux_option(
        &tmux_bin,
        &tmux_session,
        "@nopal_plot_session",
        &nopal_session_id,
    );

    let launch_args = vec![
        format!("PI_CODING_AGENT_DIR={}", agent_dir.display()),
        "PI_SKIP_VERSION_CHECK=1".to_owned(),
        format!("TMPDIR={}", runtime_dir.display()),
        format!("HOME={}", home_dir.display()),
        "TERM=xterm-256color".to_owned(),
        pi_bin.display().to_string(),
        "--no-extensions".to_owned(),
        "--no-skills".to_owned(),
        "--no-prompt-templates".to_owned(),
        "--no-themes".to_owned(),
        "--tools".to_owned(),
        "read".to_owned(),
        "--offline".to_owned(),
        "--approve".to_owned(),
        "--extension".to_owned(),
        nopal_extension.display().to_string(),
        "--extension".to_owned(),
        provider_extension.display().to_string(),
        "--provider".to_owned(),
        "nopal-proof".to_owned(),
        "--model".to_owned(),
        "deterministic".to_owned(),
        "--session-dir".to_owned(),
        pi_sessions.display().to_string(),
        "--name".to_owned(),
        format!("Nopal real Pi proof {unique}"),
    ];
    respawn_pi(
        &tmux_bin,
        &pane_id,
        &repo,
        &launch_args,
        "launch actual interactive Pi in the isolated pane",
    );

    let socket = wait_for_socket(&runtime_dir, &tmux_bin, &pane_id);
    let first_process = pane_process_identity(&tmux_bin, &pane_id);
    assert!(
        !first_process.command.is_empty() && first_process.pid > 0,
        "tmux pane must expose the live process that published the real Pi bridge: {first_process:?}"
    );

    let second = establish_plot(
        &nopal_bin,
        &repo,
        &state_dir,
        &plot_id,
        &tmux_session,
        &pane_id,
        Some(&socket),
    );
    assert_eq!(
        second
            .pointer("/plot/selected_session_id")
            .and_then(Value::as_str),
        Some(nopal_session_id.as_str())
    );
    assert_eq!(
        second
            .pointer("/plot/sessions/0/protocol/address")
            .and_then(Value::as_str),
        Some(must_some(socket.to_str(), "UTF-8 socket path"))
    );
    assert_eq!(
        second
            .pointer("/plot/sessions/0/protocol/state")
            .and_then(Value::as_str),
        Some("ready")
    );

    let inspected = inspect_field(&nopal_bin, &repo, &state_dir);
    let snapshot = must(parse_field(&inspected), "parse production Field projection");
    let field = DesktopField::from_snapshot(snapshot, Some(&plot_id));
    let mut runtime = LiveSessionRuntime::new(field, ProductionRuntimeConnector);
    assert_eq!(runtime.presentation(), RuntimePresentation::Output);
    assert_eq!(
        runtime
            .selected_session_context()
            .as_ref()
            .map(|context| (context.plot_id.as_str(), context.session_id.as_str())),
        Some((plot_id.as_str(), nopal_session_id.as_str()))
    );
    assert!(
        runtime.terminal_binding().is_none(),
        "healthy structured startup must discover the Session host without attaching Terminal"
    );
    wait_for_replay_live(&mut runtime);
    assert_eq!(runtime.status(), &RuntimeStatus::Ready);
    wait_for_ready(&mut runtime);

    let structured_command = submitted_command(&mut runtime, "structured proof");
    wait_for_runtime_pair(&mut runtime, Some(&structured_command), "structured proof");

    let fifo_first_command = submitted_command(&mut runtime, FIFO_FIRST_PROMPT);
    wait_for_runtime_user(&mut runtime, &fifo_first_command, FIFO_FIRST_PROMPT);
    let fifo_second_command = submitted_command(&mut runtime, FIFO_SECOND_PROMPT);
    assert_ne!(
        fifo_first_command, fifo_second_command,
        "rapid Composer submissions must receive distinct command ids"
    );
    runtime.drain();
    assert!(
        !runtime.current_events().iter().any(|event| {
            matches!(
                event.event,
                SessionEventPayload::AssistantMessage { ref text, .. }
                    if text == FIFO_FIRST_RESPONSE
            )
        }),
        "the delayed first response completed before the second Composer submission was queued"
    );
    wait_for_fifo_pairs(
        &mut runtime,
        (&fifo_first_command, FIFO_FIRST_PROMPT),
        (&fifo_second_command, FIFO_SECOND_PROMPT),
    );

    let (pi_session_file, first_pi_session_id) = wait_for_pi_session(&pi_sessions);
    let tool_loop_command = submitted_command(&mut runtime, TOOL_LOOP_PROMPT);
    wait_for_runtime_assistants(
        &mut runtime,
        &tool_loop_command,
        TOOL_LOOP_PROMPT,
        &[TOOL_LOOP_COMPLETE],
    );
    wait_for_persisted_tool_loop(&pi_session_file, TOOL_CALL_ID, "read", TOOL_LOOP_PROMPT);

    let following_prompt = "structured proof after tool loop";
    let following_command = submitted_command(&mut runtime, following_prompt);
    assert_ne!(
        following_command, tool_loop_command,
        "following structured turn must have a distinct command id"
    );
    wait_for_runtime_pair(&mut runtime, Some(&following_command), following_prompt);
    assert_assistant_attribution(&runtime, TOOL_LOOP_COMPLETE, &tool_loop_command);
    assert_assistant_attribution(
        &runtime,
        &format!("Nopal deterministic assistant: {following_prompt}"),
        &following_command,
    );

    let before_terminal = pane_process_identity(&tmux_bin, &pane_id);
    assert_eq!(first_process, before_terminal);
    assert!(
        runtime.terminal_binding().is_none(),
        "healthy structured startup must not create Terminal transport or observation"
    );
    let terminal_events_before = runtime.current_events().to_vec();
    let terminal_session_file_before = must(
        fs::read_to_string(&pi_session_file),
        "read Pi Session before Terminal boundary proof",
    );
    runtime.set_presentation(RuntimePresentation::Terminal);
    assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
    wait_for_terminal_process(&mut runtime, first_process.pid);
    let terminal = must_some(
        runtime.terminal_binding_mut(),
        "production runtime Terminal binding",
    );
    let controller = must_some(terminal.controller_mut(), "production Terminal controller");
    controller.set_focused(true);
    assert!(
        controller.send_text(TERMINAL_FAKE_EVENT),
        "production Terminal controller must send raw text to the selected Pi pane"
    );
    wait_for_terminal_text(&mut runtime, "nopal.session.event/v2");
    let terminal = must_some(
        runtime.terminal_binding_mut(),
        "production runtime Terminal binding",
    );
    let controller = must_some(terminal.controller_mut(), "production Terminal controller");
    assert!(
        controller.send_text("\u{15}"),
        "Ctrl-U must clear the unsubmitted Terminal input"
    );
    assert_no_semantic_events_for(
        &mut runtime,
        &terminal_events_before,
        Duration::from_millis(350),
    );
    assert_eq!(
        must(
            fs::read_to_string(&pi_session_file),
            "read Pi Session after Terminal boundary proof",
        ),
        terminal_session_file_before,
        "Terminal rendering and unsubmitted input must not create durable Pi history"
    );

    runtime.set_presentation(RuntimePresentation::Output);
    assert_eq!(runtime.presentation(), RuntimePresentation::Output);
    let return_command = submitted_command(&mut runtime, "return to structured proof");
    wait_for_runtime_pair(
        &mut runtime,
        Some(&return_command),
        "return to structured proof",
    );

    let after_return = pane_process_identity(&tmux_bin, &pane_id);
    assert_eq!(
        after_return, first_process,
        "Output/Terminal/Output must retain one tmux pane and Pi PID"
    );
    let (after_pi_file, after_pi_session_id) = wait_for_pi_session(&pi_sessions);
    assert_eq!(after_pi_file, pi_session_file, "Pi Session file changed");
    assert_eq!(
        after_pi_session_id, first_pi_session_id,
        "Pi Session identity changed"
    );

    let durable_before_desktop_restart =
        read_v3_history(&socket, &plot_id, &nopal_session_id, None);
    assert_verified_v3_history(&durable_before_desktop_restart.events);
    let semantic_before_desktop_restart = runtime.current_events().to_vec();
    assert_eq!(
        semantic_before_desktop_restart,
        durable_before_desktop_restart
            .events
            .iter()
            .filter_map(V3PersistedEvent::semantic_session_event)
            .collect::<Vec<_>>(),
        "desktop timeline must be the exact verified durable history"
    );
    let verified_cursor = durable_before_desktop_restart.complete.cursor.clone();
    let verified_sequence = durable_before_desktop_restart.complete.sequence;
    let verified_stream = durable_before_desktop_restart.complete.stream_id.clone();
    drop(runtime);
    let inspected_after_desktop_restart = inspect_field(&nopal_bin, &repo, &state_dir);
    let recreated_snapshot = must(
        parse_field(&inspected_after_desktop_restart),
        "parse Field projection after desktop recreation",
    );
    let recreated_field = DesktopField::from_snapshot(recreated_snapshot, Some(&plot_id));
    let mut runtime = LiveSessionRuntime::new(recreated_field, ProductionRuntimeConnector);
    wait_for_replay_live(&mut runtime);
    assert_eq!(
        runtime.current_events(),
        semantic_before_desktop_restart.as_slice(),
        "a cold desktop recreation must replay the exact timeline without duplicates"
    );
    assert_unique_semantic_events(runtime.current_events());

    runtime.set_presentation(RuntimePresentation::Terminal);
    wait_for_terminal_process(&mut runtime, first_process.pid);

    let socket_before_restart = socket_identity(&socket);
    let mut resumed_launch_args = launch_args.clone();
    resumed_launch_args.push("--session".to_owned());
    resumed_launch_args.push(pi_session_file.display().to_string());
    respawn_pi(
        &tmux_bin,
        &pane_id,
        &repo,
        &resumed_launch_args,
        "restart actual Pi from the same Pi Session file",
    );
    let restarted_process = wait_for_new_process(&tmux_bin, &pane_id, first_process.pid);
    let restarted_socket =
        wait_for_replaced_socket(&runtime_dir, &tmux_bin, &pane_id, socket_before_restart);
    assert_eq!(
        restarted_socket, socket,
        "the stable Session endpoint path changed across Pi restart"
    );
    assert_eq!(restarted_process.pane_id, first_process.pane_id);
    assert_eq!(
        restarted_process.tmux_session_id, first_process.tmux_session_id,
        "Pi restart moved to another tmux Session"
    );
    assert_ne!(
        restarted_process.pid, first_process.pid,
        "Pi restart must create a new process"
    );
    let identity_deadline = Instant::now() + TIMEOUT;
    while Instant::now() < identity_deadline && runtime.terminal_binding().is_some() {
        runtime.drain();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        runtime.terminal_binding().is_none(),
        "Terminal binding trusted for the old Pi PID must close before reuse"
    );

    let restarted_establishment = establish_plot(
        &nopal_bin,
        &repo,
        &state_dir,
        &plot_id,
        &tmux_session,
        &pane_id,
        Some(&restarted_socket),
    );
    assert_eq!(
        restarted_establishment
            .pointer("/plot/selected_session_id")
            .and_then(Value::as_str),
        Some(nopal_session_id.as_str()),
        "Core-owned Nopal Session id changed across Pi restart"
    );
    wait_for_reconnect_cycle(&mut runtime);
    runtime.set_presentation(RuntimePresentation::Terminal);
    wait_for_terminal_process(&mut runtime, restarted_process.pid);
    runtime.set_presentation(RuntimePresentation::Output);
    assert_eq!(
        runtime.current_events(),
        semantic_before_desktop_restart.as_slice(),
        "Pi restart must resume after the verified cursor without replay duplicates"
    );
    let replay_after_restart = read_v3_history(
        &restarted_socket,
        &plot_id,
        &nopal_session_id,
        verified_cursor.as_deref(),
    );
    assert!(
        replay_after_restart.events.is_empty(),
        "resuming at the verified cursor replayed old events"
    );
    assert_eq!(replay_after_restart.complete.cursor, verified_cursor);
    assert_eq!(replay_after_restart.complete.sequence, verified_sequence);
    assert_eq!(replay_after_restart.complete.stream_id, verified_stream);
    let (restarted_pi_file, restarted_pi_session_id) = wait_for_pi_session(&pi_sessions);
    assert_eq!(restarted_pi_file, pi_session_file);
    assert_eq!(restarted_pi_session_id, first_pi_session_id);

    send_duplicate_command(
        &restarted_socket,
        &plot_id,
        &nopal_session_id,
        verified_cursor.as_deref(),
        POST_RESTART_COMMAND,
        POST_RESTART_PROMPT,
    );
    wait_for_runtime_pair(
        &mut runtime,
        Some(POST_RESTART_COMMAND),
        POST_RESTART_PROMPT,
    );
    assert_eq!(
        runtime.current_events().len(),
        semantic_before_desktop_restart.len() + 2,
        "turn N+1 must append exactly one user and one assistant event"
    );
    assert_eq!(
        &runtime.current_events()[..semantic_before_desktop_restart.len()],
        semantic_before_desktop_restart.as_slice(),
        "turn N+1 changed the verified history prefix"
    );
    assert_command_pair_once(
        runtime.current_events(),
        POST_RESTART_COMMAND,
        POST_RESTART_PROMPT,
    );
    assert_unique_semantic_events(runtime.current_events());

    let inspected = inspect_field(&nopal_bin, &repo, &state_dir);
    let session = must_some(
        inspected.pointer("/plots/0/sessions/0"),
        "inspected Field Session",
    );
    assert_eq!(
        session.get("session_id").and_then(Value::as_str),
        Some(nopal_session_id.as_str()),
        "Core-owned Nopal Session id changed"
    );
    assert_eq!(
        session.pointer("/protocol/address").and_then(Value::as_str),
        Some(must_some(socket.to_str(), "UTF-8 socket path"))
    );

    if visual_hold_seconds > 0 {
        let visual = json!({
            "state_dir": state_dir,
            "repo": repo,
            "tmux_session": tmux_session,
            "pane_id": restarted_process.pane_id,
            "initial_pi_pid": first_process.pid,
            "restarted_pi_pid": restarted_process.pid,
            "nopal_session_id": nopal_session_id,
            "pi_session_id": first_pi_session_id,
            "hold_seconds": visual_hold_seconds,
        });
        eprintln!("REAL_PI_VISUAL {visual}");
        std::thread::sleep(Duration::from_secs(visual_hold_seconds));
    }

    cleanup.kill();
    let deadline = Instant::now() + Duration::from_secs(5);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !socket.exists(),
        "Pi shutdown must remove only its owned Session socket"
    );

    eprintln!(
        "REAL_PI_PROOF plot_id={plot_id} nopal_session_id={nopal_session_id} pane_id={} initial_pi_pid={} restarted_pi_pid={} pi_session_id={first_pi_session_id} socket={}",
        first_process.pane_id,
        first_process.pid,
        restarted_process.pid,
        socket.display()
    );
}

#[test]
#[ignore = "requires actual pi, tmux, NOPAL_RUN_REAL_PI_E2E=1, and NOPAL_BIN"]
fn real_pi_shell_activity_replays_exactly_after_restart() {
    assert_eq!(
        std::env::var(RUN_ENV).as_deref(),
        Ok("1"),
        "this ignored proof was explicitly selected without {RUN_ENV}=1"
    );
    let nopal_bin = required_executable(NOPAL_BIN_ENV);
    let pi_bin = executable_from_env_or_path("NOPAL_PI_BIN", "pi");
    let tmux_bin = executable_from_env_or_path("NOPAL_TMUX_BIN", "tmux");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("desktop crate must live under <repo>/crates")
        .to_path_buf();
    let nopal_extension = repo.join("extensions/nopal/index.ts");
    let provider_extension = manifest_dir.join("tests/fixtures/deterministic-pi-provider.mjs");
    let temp = must(
        tempfile::Builder::new()
            .prefix("nopal-rp-v3-")
            .tempdir_in("/tmp"),
        "create isolated v3 real-Pi fixture root",
    );
    let state_dir = temp.path().join("state");
    let agent_dir = temp.path().join("pi-agent");
    let pi_sessions = temp.path().join("pi-sessions");
    let runtime_dir = temp.path().join("runtime");
    let home_dir = temp.path().join("home");
    for directory in [
        &state_dir,
        &agent_dir,
        &pi_sessions,
        &runtime_dir,
        &home_dir,
    ] {
        must(
            fs::create_dir_all(directory),
            "create isolated v3 proof directory",
        );
    }

    let unique = format!("{}-{}", std::process::id(), unix_millis());
    let tmux_session = format!("nopal-real-pi-v3-{unique}");
    let plot_env = nopal_core::plot_store::PlotEnv::discover(Some(&state_dir));
    let plot_id = must(
        nopal_core::plot_store::ensure_provisional(&plot_env, "nopal-real-pi-v3"),
        "bootstrap v3 proof Plot",
    )
    .plot_id;
    let mut cleanup = TmuxCleanup::new(tmux_bin.clone(), tmux_session.clone());
    checked(
        Command::new(&tmux_bin)
            .args([
                "new-session",
                "-d",
                "-s",
                &tmux_session,
                "-x",
                "120",
                "-y",
                "36",
                "sleep 300",
            ])
            .output(),
        "create isolated v3 tmux Session",
    );
    let pane_id = tmux_value(&tmux_bin, &tmux_session, "#{pane_id}");
    let first = establish_plot(
        &nopal_bin,
        &repo,
        &state_dir,
        &plot_id,
        &tmux_session,
        &pane_id,
        None,
    );
    let nopal_session_id = must_some(
        first
            .pointer("/plot/selected_session_id")
            .and_then(Value::as_str),
        "v3 proof establishment selected Session",
    )
    .to_owned();
    set_tmux_option(&tmux_bin, &tmux_session, "@nopal_plot", &plot_id);
    set_tmux_option(
        &tmux_bin,
        &tmux_session,
        "@nopal_plot_session",
        &nopal_session_id,
    );
    let launch_args = vec![
        format!("PI_CODING_AGENT_DIR={}", agent_dir.display()),
        "PI_SKIP_VERSION_CHECK=1".to_owned(),
        format!("TMPDIR={}", runtime_dir.display()),
        format!("HOME={}", home_dir.display()),
        "TERM=xterm-256color".to_owned(),
        pi_bin.display().to_string(),
        "--no-extensions".to_owned(),
        "--no-skills".to_owned(),
        "--no-prompt-templates".to_owned(),
        "--no-themes".to_owned(),
        "--tools".to_owned(),
        "bash".to_owned(),
        "--offline".to_owned(),
        "--approve".to_owned(),
        "--extension".to_owned(),
        nopal_extension.display().to_string(),
        "--extension".to_owned(),
        provider_extension.display().to_string(),
        "--provider".to_owned(),
        "nopal-proof".to_owned(),
        "--model".to_owned(),
        "deterministic".to_owned(),
        "--session-dir".to_owned(),
        pi_sessions.display().to_string(),
        "--name".to_owned(),
        format!("Nopal real Pi v3 proof {unique}"),
    ];
    respawn_pi(
        &tmux_bin,
        &pane_id,
        &repo,
        &launch_args,
        "launch actual Pi for v3 producer proof",
    );
    let socket = wait_for_socket(&runtime_dir, &tmux_bin, &pane_id);
    establish_plot(
        &nopal_bin,
        &repo,
        &state_dir,
        &plot_id,
        &tmux_session,
        &pane_id,
        Some(&socket),
    );
    let first_process = pane_process_identity(&tmux_bin, &pane_id);
    let command_id = "command-real-pi-shell-v3";
    let command_connection = send_v3_prompt(
        &socket,
        &plot_id,
        &nopal_session_id,
        command_id,
        SHELL_LOOP_PROMPT,
    );
    let before_restart = wait_for_v3_shell_turn(&socket, &plot_id, &nopal_session_id, command_id);
    drop(command_connection);
    assert_verified_v3_history(&before_restart.events);
    let shell_before = shell_activity_events(&before_restart.events);
    assert_eq!(shell_before.len(), 2);
    assert!(matches!(
        shell_before[0].event,
        SessionActivityEventPayload::CommandStarted { .. }
    ));
    assert!(matches!(
        shell_before[1].event,
        SessionActivityEventPayload::CommandFinished { .. }
    ));
    let (pi_session_file, pi_session_id) = wait_for_pi_session(&pi_sessions);
    wait_for_persisted_tool_loop(&pi_session_file, SHELL_CALL_ID, "bash", SHELL_LOOP_PROMPT);

    let socket_before_restart = socket_identity(&socket);
    let mut resumed_launch_args = launch_args.clone();
    resumed_launch_args.push("--session".to_owned());
    resumed_launch_args.push(pi_session_file.display().to_string());
    respawn_pi(
        &tmux_bin,
        &pane_id,
        &repo,
        &resumed_launch_args,
        "restart actual Pi for v3 producer proof",
    );
    let restarted_process = wait_for_new_process(&tmux_bin, &pane_id, first_process.pid);
    let restarted_socket =
        wait_for_replaced_socket(&runtime_dir, &tmux_bin, &pane_id, socket_before_restart);
    let resumed = read_v3_history(
        &restarted_socket,
        &plot_id,
        &nopal_session_id,
        before_restart.complete.cursor.as_deref(),
    );
    assert!(resumed.events.is_empty());
    assert_eq!(resumed.complete.cursor, before_restart.complete.cursor);
    assert_eq!(resumed.complete.sequence, before_restart.complete.sequence);
    assert_eq!(
        resumed.complete.stream_id,
        before_restart.complete.stream_id
    );
    let after_restart = read_v3_history(&restarted_socket, &plot_id, &nopal_session_id, None);
    assert_eq!(after_restart.events, before_restart.events);
    assert_eq!(shell_activity_events(&after_restart.events), shell_before);
    let (restarted_pi_file, restarted_pi_session_id) = wait_for_pi_session(&pi_sessions);
    assert_eq!(restarted_pi_file, pi_session_file);
    assert_eq!(restarted_pi_session_id, pi_session_id);
    assert_ne!(restarted_process.pid, first_process.pid);

    cleanup.kill();
    eprintln!(
        "REAL_PI_V3_ACTIVITY_PROOF plot_id={plot_id} session_id={nopal_session_id} tool_call_id={SHELL_CALL_ID} initial_pi_pid={} restarted_pi_pid={} events={} cursor={:?}",
        first_process.pid,
        restarted_process.pid,
        before_restart.events.len(),
        before_restart.complete.cursor,
    );
}

fn visual_hold_seconds() -> u64 {
    must(
        parse_visual_hold_seconds(std::env::var(VISUAL_HOLD_ENV).ok().as_deref()),
        VISUAL_HOLD_ENV,
    )
}

fn parse_visual_hold_seconds(raw: Option<&str>) -> Result<u64, String> {
    let Some(raw) = raw else {
        return Ok(0);
    };
    let seconds = raw.parse::<u64>().map_err(|_| {
        format!("must be an integer from 0 through {MAX_VISUAL_HOLD_SECONDS}, got {raw:?}")
    })?;
    if seconds > MAX_VISUAL_HOLD_SECONDS {
        return Err(format!(
            "must be from 0 through {MAX_VISUAL_HOLD_SECONDS}, got {seconds}"
        ));
    }
    Ok(seconds)
}

#[test]
fn visual_hold_parser_accepts_only_the_bounded_opt_in_range() {
    assert_eq!(parse_visual_hold_seconds(None), Ok(0));
    assert_eq!(parse_visual_hold_seconds(Some("0")), Ok(0));
    assert_eq!(parse_visual_hold_seconds(Some("1")), Ok(1));
    assert_eq!(parse_visual_hold_seconds(Some("60")), Ok(60));
    for invalid in ["", "-1", "61", "1.5", " 1"] {
        assert!(
            parse_visual_hold_seconds(Some(invalid)).is_err(),
            "unexpectedly accepted {invalid:?}"
        );
    }
}

#[test]
fn copied_corrupt_history_freezes_the_verified_prefix_at_the_last_good_cursor() {
    let context = model::SelectedSessionContext {
        plot_id: "plot-corrupt-proof".to_owned(),
        session_id: "session-corrupt-proof".to_owned(),
        host_pane: None,
        protocol: None,
    };
    let first = durable_fixture(
        "event-corrupt-proof-1",
        1,
        None,
        "cursor-corrupt-proof-1",
        "first verified event",
    );
    let mut copied_corrupt = durable_fixture(
        "event-corrupt-proof-2",
        2,
        Some("cursor-corrupt-proof-1"),
        "cursor-corrupt-proof-2",
        "copied event with a corrupt predecessor",
    );
    copied_corrupt.previous_cursor = Some("cursor-from-divergent-history".to_owned());

    let mut timeline = SessionTimelineStore::default();
    timeline.select_session(Some(&context));
    must_debug(timeline.begin_replay(None), "begin corrupt-history replay");
    must_debug(
        timeline.ingest_durable(first.clone()),
        "accept verified history prefix",
    );
    must_debug(
        timeline.complete_replay(Some(first.cursor.as_str()), 1),
        "commit verified history prefix",
    );
    must_debug(
        timeline.begin_replay(Some(first.cursor.as_str())),
        "begin copied divergent replay",
    );
    let failure = timeline
        .ingest_durable(copied_corrupt.clone())
        .expect_err("divergent copied history must fail closed");
    assert_eq!(
        failure,
        TimelineFailure::Gap {
            event_id: copied_corrupt.event_id,
            expected_sequence: 2,
            actual_sequence: 2,
            expected_previous_cursor: Some(first.cursor.clone()),
            actual_previous_cursor: Some("cursor-from-divergent-history".to_owned()),
        }
    );
    assert_eq!(timeline.current_events(), &[first.semantic_event()]);
    assert_eq!(timeline.current_cursor(), Some(first.cursor.as_str()));
    assert_eq!(timeline.current_sequence(), Some(1));
    assert_eq!(
        timeline.current_replay_state(),
        ReplayState::Failed(failure.clone()),
        "corrupt history must freeze instead of exposing an inferred branch"
    );
    assert_eq!(
        timeline
            .ingest_durable(durable_fixture(
                "event-corrupt-proof-3",
                2,
                Some(first.cursor.as_str()),
                "cursor-corrupt-proof-3",
                "later otherwise-valid event",
            ))
            .expect_err("a frozen history must reject later frames"),
        failure
    );
    assert_eq!(timeline.current_events(), &[first.semantic_event()]);
    assert_eq!(timeline.current_cursor(), Some(first.cursor.as_str()));
}

fn durable_fixture(
    event_id: &str,
    sequence: u64,
    previous_cursor: Option<&str>,
    cursor: &str,
    text: &str,
) -> DurableSessionEvent {
    DurableSessionEvent {
        kind: DURABLE_SESSION_EVENT_KIND.to_owned(),
        event_id: event_id.to_owned(),
        plot_id: "plot-corrupt-proof".to_owned(),
        session_id: "session-corrupt-proof".to_owned(),
        stream_id: "stream-corrupt-proof".to_owned(),
        sequence,
        previous_cursor: previous_cursor.map(str::to_owned),
        cursor: cursor.to_owned(),
        command_id: Some("command-corrupt-proof".to_owned()),
        event: SessionEventPayload::AssistantMessage {
            text: text.to_owned(),
            extra: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    }
}

fn required_executable(variable: &str) -> PathBuf {
    let path = must_some(
        std::env::var_os(variable).map(PathBuf::from),
        &format!("{variable} must point to a built executable"),
    );
    assert!(path.is_file(), "{variable} does not name a file: {path:?}");
    path
}

fn executable_from_env_or_path(variable: &str, fallback: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(variable) {
        return PathBuf::from(path);
    }
    let output = checked(
        Command::new("/usr/bin/env")
            .args(["which", fallback])
            .output(),
        &format!("find {fallback}"),
    );
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

fn establish_plot(
    nopal_bin: &Path,
    repo: &Path,
    state_dir: &Path,
    plot_id: &str,
    host_session: &str,
    host_pane: &str,
    protocol_address: Option<&Path>,
) -> Value {
    let mut command = Command::new(nopal_bin);
    command
        .current_dir(repo)
        .args(["--json", "plot", "establish", "--state-dir"])
        .arg(state_dir)
        .args(["--plot-id", plot_id, "--event", "kickoff_context_ready"])
        .arg("--workspace")
        .arg(repo)
        .args(["--host-session", host_session, "--host-pane", host_pane]);
    if let Some(address) = protocol_address {
        command
            .arg("--protocol-address")
            .arg(address)
            .args(["--protocol-state", "ready"]);
    }
    json_output(command.output(), "establish Plot Session")
}

fn inspect_field(nopal_bin: &Path, repo: &Path, state_dir: &Path) -> Value {
    let mut command = Command::new(nopal_bin);
    command
        .current_dir(repo)
        .args(["--json", "field", "inspect", "--state-dir"])
        .arg(state_dir)
        .arg("--all");
    json_output(command.output(), "inspect Field")
}

fn json_output(output: std::io::Result<Output>, action: &str) -> Value {
    let output = checked(output, action);
    match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => panic!(
            "{action} returned invalid JSON: {error}; stdout={:?}",
            String::from_utf8_lossy(&output.stdout)
        ),
    }
}

fn checked(output: std::io::Result<Output>, action: &str) -> Output {
    let output = match output {
        Ok(output) => output,
        Err(error) => panic!("cannot {action}: {error}"),
    };
    assert!(
        output.status.success(),
        "cannot {action}: status={:?} stdout={:?} stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn set_tmux_option(tmux: &Path, session: &str, option: &str, value: &str) {
    checked(
        Command::new(tmux)
            .args(["set-option", "-t", session, option, value])
            .output(),
        &format!("set tmux option {option}"),
    );
}

fn respawn_pi(tmux: &Path, pane_id: &str, repo: &Path, launch_args: &[String], action: &str) {
    let mut respawn = Command::new(tmux);
    respawn
        .args(["respawn-pane", "-k", "-t", pane_id, "-c"])
        .arg(repo)
        .arg("--")
        .arg("/usr/bin/env")
        .args(launch_args);
    checked(respawn.output(), action);
}

fn tmux_value(tmux: &Path, target: &str, format: &str) -> String {
    let output = checked(
        Command::new(tmux)
            .args(["display-message", "-p", "-t", target, format])
            .output(),
        "read tmux identity",
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneProcessIdentity {
    pane_id: String,
    tmux_session_id: String,
    pid: u32,
    command: String,
}

fn pane_process_identity(tmux: &Path, pane_id: &str) -> PaneProcessIdentity {
    let output = checked(
        Command::new(tmux)
            .args([
                "display-message",
                "-p",
                "-t",
                pane_id,
                "#{pane_id}|#{session_id}|#{pane_pid}|#{pane_current_command}",
            ])
            .output(),
        "read Pi pane process identity",
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.trim().split('|');
    let identity = PaneProcessIdentity {
        pane_id: must_some(fields.next(), "tmux pane id").to_owned(),
        tmux_session_id: must_some(fields.next(), "tmux Session id").to_owned(),
        pid: must(
            must_some(fields.next(), "Pi pane pid").parse(),
            "numeric Pi pane pid",
        ),
        command: must_some(fields.next(), "Pi pane command").to_owned(),
    };
    assert!(
        fields.next().is_none(),
        "unexpected tmux identity: {text:?}"
    );
    identity
}

fn wait_for_new_process(tmux: &Path, pane_id: &str, previous_pid: u32) -> PaneProcessIdentity {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let identity = pane_process_identity(tmux, pane_id);
        if identity.pid != previous_pid && !identity.command.is_empty() {
            return identity;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("Pi restart did not replace pane process {previous_pid}");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &Path) -> SocketIdentity {
    must_some(try_socket_identity(path), "read Session socket identity")
}

fn try_socket_identity(path: &Path) -> Option<SocketIdentity> {
    let metadata = fs::metadata(path).ok()?;
    Some(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn wait_for_replaced_socket(
    runtime_dir: &Path,
    tmux: &Path,
    pane_id: &str,
    previous: SocketIdentity,
) -> PathBuf {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Some(socket) = find_file(runtime_dir, |path| {
            path.extension().and_then(|value| value.to_str()) == Some("sock")
        }) && try_socket_identity(&socket).is_some_and(|identity| identity != previous)
        {
            return socket;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let capture = match Command::new(tmux)
        .args(["capture-pane", "-p", "-S", "-", "-t", pane_id])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(_) => String::new(),
    };
    panic!("Pi restart did not replace the Session socket; pane:\n{capture}");
}

fn wait_for_socket(runtime_dir: &Path, tmux: &Path, pane_id: &str) -> PathBuf {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Some(socket) = find_file(runtime_dir, |path| {
            path.extension().and_then(|value| value.to_str()) == Some("sock")
        }) {
            return socket;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let capture = match Command::new(tmux)
        .args(["capture-pane", "-p", "-S", "-", "-t", pane_id])
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(_) => String::new(),
    };
    panic!("real Pi Session bridge did not publish a socket; pane:\n{capture}");
}

fn wait_for_pi_session(session_dir: &Path) -> (PathBuf, String) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Some(path) = find_file(session_dir, |path| {
            path.extension().and_then(|value| value.to_str()) == Some("jsonl")
        }) && let Ok(text) = fs::read_to_string(&path)
            && let Some(first) = text.lines().next()
            && let Ok(header) = serde_json::from_str::<Value>(first)
            && let Some(id) = header.get("id").and_then(Value::as_str)
        {
            return (path, id.to_owned());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("real Pi did not persist one Session under {session_dir:?}");
}

fn wait_for_persisted_tool_loop(
    session_file: &Path,
    tool_call_id: &str,
    tool_name: &str,
    prompt: &str,
) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(session_file) {
            let mut prelude_seen = false;
            let mut final_seen = false;
            let mut successful_tool_seen = false;
            for value in text
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            {
                let Some(message) = value.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) == Some("assistant") {
                    let assistant_text = message
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<String>();
                    prelude_seen |=
                        assistant_text == format!("Nopal deterministic tool prelude: {prompt}");
                    final_seen |= assistant_text
                        == format!("Nopal deterministic assistant after tool: {prompt}");
                }
                if message.get("role").and_then(Value::as_str) == Some("toolResult")
                    && message.get("toolCallId").and_then(Value::as_str) == Some(tool_call_id)
                    && message.get("toolName").and_then(Value::as_str) == Some(tool_name)
                {
                    assert_eq!(
                        message.get("isError").and_then(Value::as_bool),
                        Some(false),
                        "deterministic built-in tool execution failed: {message}"
                    );
                    successful_tool_seen = true;
                }
            }
            if prelude_seen && successful_tool_seen && final_seen {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "real Pi Session did not persist the two-assistant {tool_name} loop with successful result {tool_call_id:?}: {session_file:?}"
    );
}

fn find_file(root: &Path, predicate: impl Fn(&Path) -> bool + Copy) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if predicate(&path) {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file(&path, predicate)
        {
            return Some(found);
        }
    }
    None
}

fn wait_for_ready(runtime: &mut LiveSessionRuntime) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        runtime.drain();
        if runtime
            .current_events()
            .iter()
            .any(|event| matches!(event.event, SessionEventPayload::SessionReady { .. }))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("production runtime did not receive real Pi session_ready");
}

fn wait_for_replay_live(runtime: &mut LiveSessionRuntime) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported Session errors while restoring: {:?}",
            outcome.errors
        );
        match runtime.replay_state() {
            ReplayState::Live => return,
            ReplayState::Failed(failure) => {
                panic!("production runtime rejected durable replay: {failure:?}")
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "production runtime did not reach verified live replay; state={:?}",
        runtime.replay_state()
    );
}

fn wait_for_reconnect_cycle(runtime: &mut LiveSessionRuntime) {
    let deadline = Instant::now() + TIMEOUT;
    let mut reconnect_seen = false;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        for error in &outcome.errors {
            assert!(
                error.contains("closed")
                    || error.contains("connect")
                    || error.contains("socket")
                    || error.contains("endpoint"),
                "production runtime reported a non-transport error during Pi restart: {error}"
            );
        }
        match runtime.replay_state() {
            ReplayState::Reconnecting { .. } | ReplayState::Restoring { .. } => {
                reconnect_seen = true;
            }
            ReplayState::Live if reconnect_seen => return,
            ReplayState::Failed(failure) => {
                panic!("production runtime failed while resuming after Pi restart: {failure:?}")
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "production runtime did not observe and recover from Pi restart: state={:?}",
        runtime.replay_state()
    );
}

fn wait_for_terminal_text(runtime: &mut LiveSessionRuntime, expected: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported errors while rendering Terminal output: {:?}",
            outcome.errors
        );
        let text = runtime
            .terminal_binding()
            .and_then(|terminal| terminal.snapshot())
            .map(|snapshot| {
                snapshot
                    .rows
                    .into_iter()
                    .flat_map(|row| row.runs.into_iter().map(|run| run.text))
                    .collect::<String>()
            })
            .unwrap_or_default();
        if text.contains(expected) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("Terminal did not render unsubmitted text containing {expected:?}");
}

fn wait_for_terminal_process(runtime: &mut LiveSessionRuntime, expected_pid: u32) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported errors while attaching Terminal: {:?}",
            outcome.errors
        );
        let actual_pid = runtime
            .terminal_binding()
            .map(TerminalSessionBinding::process_identity)
            .map(nopal_native_lifecycle::session_bindings::TerminalProcessIdentity::get);
        if actual_pid == Some(expected_pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "Terminal did not attach to expected Pi PID {expected_pid}; actual={:?}",
        runtime
            .terminal_binding()
            .map(TerminalSessionBinding::process_identity)
            .map(nopal_native_lifecycle::session_bindings::TerminalProcessIdentity::get)
    );
}

fn assert_no_semantic_events_for(
    runtime: &mut LiveSessionRuntime,
    expected: &[nopal_feed_client::session::SessionEvent],
    duration: Duration,
) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported errors during Terminal boundary proof: {:?}",
            outcome.errors
        );
        assert_eq!(
            runtime.current_events(),
            expected,
            "Terminal bytes must not be inferred as structured Session history"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Debug)]
struct DurableReplay {
    events: Vec<DurableSessionEvent>,
    complete: nopal_feed_client::session::SessionReplayComplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum V3PersistedEvent {
    Legacy(DurableSessionEvent),
    Activity(DurableSessionActivityEvent),
}

impl V3PersistedEvent {
    fn event_id(&self) -> &str {
        match self {
            Self::Legacy(event) => &event.event_id,
            Self::Activity(event) => &event.event_id,
        }
    }

    fn stream_id(&self) -> &str {
        match self {
            Self::Legacy(event) => &event.stream_id,
            Self::Activity(event) => &event.stream_id,
        }
    }

    fn sequence(&self) -> u64 {
        match self {
            Self::Legacy(event) => event.sequence,
            Self::Activity(event) => event.sequence,
        }
    }

    fn previous_cursor(&self) -> Option<&str> {
        match self {
            Self::Legacy(event) => event.previous_cursor.as_deref(),
            Self::Activity(event) => event.previous_cursor.as_deref(),
        }
    }

    fn cursor(&self) -> &str {
        match self {
            Self::Legacy(event) => &event.cursor,
            Self::Activity(event) => &event.cursor,
        }
    }

    fn semantic_session_event(&self) -> Option<nopal_feed_client::session::SessionEvent> {
        match self {
            Self::Legacy(event) => VerifiedSessionEvent::V2(event.clone()).semantic_session_event(),
            Self::Activity(event) => {
                VerifiedSessionEvent::V3(event.clone()).semantic_session_event()
            }
        }
    }
}

#[derive(Debug)]
struct V3DurableReplay {
    events: Vec<V3PersistedEvent>,
    complete: SessionReplayComplete,
}

fn read_v3_history(
    socket: &Path,
    plot_id: &str,
    session_id: &str,
    after_cursor: Option<&str>,
) -> V3DurableReplay {
    let request_id = format!("subscribe-real-pi-v3-proof-{}", unix_millis());
    let mut stream = must(
        UnixStream::connect(socket),
        "connect direct v3 Session proof",
    );
    must(
        stream.set_read_timeout(Some(TIMEOUT)),
        "bound direct v3 Session proof read",
    );
    let subscribe = json!({
        "kind": SESSION_SUBSCRIBE_KIND,
        "request_id": request_id,
        "plot_id": plot_id,
        "session_id": session_id,
        "after_cursor": after_cursor,
        "page_limit": DEFAULT_REPLAY_PAGE_LIMIT,
    });
    must(
        writeln!(stream, "{}", subscribe),
        "write direct v3 Session subscription",
    );
    let mut reader = BufReader::new(stream);
    let mut events = Vec::new();
    loop {
        let mut line = String::new();
        let bytes = must(reader.read_line(&mut line), "read direct v3 Session frame");
        assert!(
            bytes > 0,
            "direct v3 Session feed closed before replay_complete"
        );
        let line = line.strip_suffix('\n').unwrap_or(&line);
        match must_debug(
            parse_session_v3_server_frame(line),
            "parse direct v3 Session frame",
        ) {
            SessionV3ServerFrame::Event(event) => {
                events.push(V3PersistedEvent::Legacy(event));
            }
            SessionV3ServerFrame::ActivityEvent(event) => {
                events.push(V3PersistedEvent::Activity(event));
            }
            SessionV3ServerFrame::ReplayComplete(complete) => {
                assert_eq!(complete.request_id, request_id);
                assert_eq!(complete.plot_id, plot_id);
                assert_eq!(complete.session_id, session_id);
                return V3DurableReplay { events, complete };
            }
            SessionV3ServerFrame::FeedError(error) => {
                panic!("direct v3 Session replay failed: {error:?}")
            }
        }
    }
}

fn send_v3_prompt(
    socket: &Path,
    plot_id: &str,
    session_id: &str,
    command_id: &str,
    prompt: &str,
) -> UnixStream {
    let request_id = format!("subscribe-real-pi-v3-command-{}", unix_millis());
    let mut stream = must(UnixStream::connect(socket), "connect v3 command proof");
    must(
        stream.set_read_timeout(Some(TIMEOUT)),
        "bound v3 command proof read",
    );
    must(
        writeln!(
            stream,
            "{}",
            json!({
                "kind": SESSION_SUBSCRIBE_KIND,
                "request_id": request_id,
                "plot_id": plot_id,
                "session_id": session_id,
                "after_cursor": null,
                "page_limit": DEFAULT_REPLAY_PAGE_LIMIT,
            })
        ),
        "subscribe v3 command proof",
    );
    let reader_stream = must(stream.try_clone(), "clone v3 command proof socket");
    let mut reader = BufReader::new(reader_stream);
    loop {
        let mut line = String::new();
        let bytes = must(reader.read_line(&mut line), "read v3 command replay");
        assert!(bytes > 0, "v3 command feed closed before replay_complete");
        match must_debug(
            parse_session_v3_server_frame(line.trim_end_matches('\n')),
            "parse v3 command replay",
        ) {
            SessionV3ServerFrame::ReplayComplete(complete) => {
                assert_eq!(complete.request_id, request_id);
                break;
            }
            SessionV3ServerFrame::FeedError(error) => {
                panic!("v3 command subscription failed: {error:?}")
            }
            SessionV3ServerFrame::Event(_) | SessionV3ServerFrame::ActivityEvent(_) => {}
        }
    }
    must(
        writeln!(
            stream,
            "{}",
            json!({
                "kind": SESSION_COMMAND_KIND,
                "command_id": command_id,
                "plot_id": plot_id,
                "session_id": session_id,
                "command": { "type": "prompt", "text": prompt },
            })
        ),
        "send v3 structured prompt",
    );
    stream
}

fn shell_activity_events(events: &[V3PersistedEvent]) -> Vec<DurableSessionActivityEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            V3PersistedEvent::Activity(event)
                if matches!(
                    &event.event,
                    SessionActivityEventPayload::CommandStarted { tool_call_id, .. }
                        | SessionActivityEventPayload::CommandFinished { tool_call_id, .. }
                        | SessionActivityEventPayload::CommandFailed { tool_call_id, .. }
                        if tool_call_id == SHELL_CALL_ID
                ) =>
            {
                Some(event.clone())
            }
            _ => None,
        })
        .collect()
}

fn wait_for_v3_shell_turn(
    socket: &Path,
    plot_id: &str,
    session_id: &str,
    command_id: &str,
) -> V3DurableReplay {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let replay = read_v3_history(socket, plot_id, session_id, None);
        let shell = shell_activity_events(&replay.events);
        let assistant_complete = replay.events.iter().any(|event| {
            matches!(
                event,
                V3PersistedEvent::Activity(DurableSessionActivityEvent {
                    command_id: Some(event_command_id),
                    event: SessionActivityEventPayload::AssistantMessage { text, .. },
                    ..
                }) if event_command_id == command_id && text == SHELL_LOOP_COMPLETE
            )
        });
        if shell.len() == 2
            && shell
                .iter()
                .all(|event| event.command_id.as_deref() == Some(command_id))
            && assistant_complete
        {
            return replay;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("real Pi did not publish one complete typed shell lifecycle without Terminal inference");
}

fn feed_context(socket: &Path, plot_id: &str, session_id: &str) -> SessionFeedContext {
    SessionFeedContext {
        plot_id: plot_id.to_owned(),
        session_id: session_id.to_owned(),
        endpoint_kind: "nopal.session/v3".to_owned(),
        endpoint_address: must_some(socket.to_str(), "UTF-8 Session socket").to_owned(),
    }
}

fn connect_feed(
    socket: &Path,
    plot_id: &str,
    session_id: &str,
) -> session_client::ProductionFeedConnection {
    let mut transport = ProductionFeedTransport;
    must_debug(
        transport.connect(&feed_context(socket, plot_id, session_id)),
        "connect direct production Session feed proof",
    )
}

fn subscribe_and_read(
    connection: &mut session_client::ProductionFeedConnection,
    plot_id: &str,
    session_id: &str,
    after_cursor: Option<&str>,
) -> DurableReplay {
    let request_id = format!("subscribe-real-pi-proof-{}", unix_millis());
    must_debug(
        connection.send(ClientFeedFrame::Subscribe(SessionSubscribe {
            kind: SESSION_SUBSCRIBE_KIND.to_owned(),
            request_id: request_id.clone(),
            plot_id: plot_id.to_owned(),
            session_id: session_id.to_owned(),
            after_cursor: after_cursor.map(str::to_owned),
            page_limit: DEFAULT_REPLAY_PAGE_LIMIT,
            extra: BTreeMap::new(),
        })),
        "subscribe direct production Session feed proof",
    );
    let deadline = Instant::now() + TIMEOUT;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        match must_debug(
            connection.try_receive(),
            "receive direct production Session replay",
        ) {
            Some(SessionFeedServerFrame::Event(event)) => match *event {
                VerifiedSessionEvent::V2(event) => events.push(event),
                VerifiedSessionEvent::V3(_) => {}
            },
            Some(SessionFeedServerFrame::ReplayComplete(complete)) => {
                assert_eq!(complete.request_id, request_id);
                assert_eq!(complete.plot_id, plot_id);
                assert_eq!(complete.session_id, session_id);
                return DurableReplay { events, complete };
            }
            Some(SessionFeedServerFrame::FeedError(error)) => {
                panic!("direct production Session replay failed: {error:?}")
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("direct production Session replay did not complete");
}

fn read_durable_history(
    socket: &Path,
    plot_id: &str,
    session_id: &str,
    after_cursor: Option<&str>,
) -> DurableReplay {
    let mut connection = connect_feed(socket, plot_id, session_id);
    let replay = subscribe_and_read(&mut connection, plot_id, session_id, after_cursor);
    connection.close();
    replay
}

fn send_duplicate_command(
    socket: &Path,
    plot_id: &str,
    session_id: &str,
    after_cursor: Option<&str>,
    command_id: &str,
    prompt: &str,
) {
    let mut connection = connect_feed(socket, plot_id, session_id);
    let replay = subscribe_and_read(&mut connection, plot_id, session_id, after_cursor);
    assert!(
        replay.events.is_empty(),
        "duplicate-command client did not subscribe at the verified head"
    );
    let command = ClientFeedFrame::Prompt(SessionCommand {
        kind: SESSION_COMMAND_KIND.to_owned(),
        command_id: command_id.to_owned(),
        plot_id: plot_id.to_owned(),
        session_id: session_id.to_owned(),
        command: SessionCommandPayload::Prompt {
            text: prompt.to_owned(),
            extra: BTreeMap::new(),
        },
        extra: BTreeMap::new(),
    });
    must_debug(
        connection.send(command.clone()),
        "send turn N+1 with a stable command id",
    );
    must_debug(
        connection.send(command),
        "retry turn N+1 with the same command id",
    );
    let _ = must_debug(
        connection.try_receive(),
        "flush duplicate command retry to production Session feed",
    );
    connection.close();
}

fn assert_verified_history(events: &[DurableSessionEvent]) {
    let mut previous_cursor = None;
    let mut stream_id = None;
    let mut event_ids = HashSet::new();
    let mut cursors = HashSet::new();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.sequence,
            index as u64 + 1,
            "durable history sequence is not contiguous"
        );
        assert_eq!(
            event.previous_cursor, previous_cursor,
            "durable history cursor chain diverged"
        );
        assert!(
            stream_id
                .get_or_insert_with(|| event.stream_id.clone())
                .as_str()
                == event.stream_id.as_str(),
            "durable history changed stream identity"
        );
        assert!(
            event_ids.insert(event.event_id.as_str()),
            "durable history duplicated event id {:?}",
            event.event_id
        );
        assert!(
            cursors.insert(event.cursor.as_str()),
            "durable history duplicated cursor {:?}",
            event.cursor
        );
        previous_cursor = Some(event.cursor.clone());
    }
}

fn assert_verified_v3_history(events: &[V3PersistedEvent]) {
    let mut previous_cursor = None;
    let mut stream_id = None;
    let mut event_ids = HashSet::new();
    let mut cursors = HashSet::new();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.sequence(),
            index as u64 + 1,
            "mixed v2/v3 durable history sequence is not contiguous"
        );
        assert_eq!(
            event.previous_cursor(),
            previous_cursor.as_deref(),
            "mixed v2/v3 durable history cursor chain diverged"
        );
        assert!(
            stream_id
                .get_or_insert_with(|| event.stream_id().to_owned())
                .as_str()
                == event.stream_id(),
            "mixed v2/v3 durable history changed stream identity"
        );
        assert!(
            event_ids.insert(event.event_id()),
            "mixed v2/v3 durable history duplicated event id {:?}",
            event.event_id()
        );
        assert!(
            cursors.insert(event.cursor()),
            "mixed v2/v3 durable history duplicated cursor {:?}",
            event.cursor()
        );
        previous_cursor = Some(event.cursor().to_owned());
    }
}

fn assert_unique_semantic_events(events: &[nopal_feed_client::session::SessionEvent]) {
    let mut ids = HashSet::new();
    for event in events {
        assert!(
            ids.insert(event.event_id.as_str()),
            "desktop timeline duplicated event id {:?}",
            event.event_id
        );
    }
}

fn assert_command_pair_once(
    events: &[nopal_feed_client::session::SessionEvent],
    command_id: &str,
    prompt: &str,
) {
    let expected_assistant = format!("Nopal deterministic assistant: {prompt}");
    let users = events
        .iter()
        .filter(|event| {
            event.command_id.as_deref() == Some(command_id)
                && matches!(
                    event.event,
                    SessionEventPayload::UserMessage { ref text, .. } if text == prompt
                )
        })
        .count();
    let assistants = events
        .iter()
        .filter(|event| {
            event.command_id.as_deref() == Some(command_id)
                && matches!(
                    event.event,
                    SessionEventPayload::AssistantMessage { ref text, .. }
                        if text == &expected_assistant
                )
        })
        .count();
    assert_eq!(users, 1, "same command id retry duplicated the user event");
    assert_eq!(
        assistants, 1,
        "same command id retry duplicated the assistant event"
    );
}

fn submitted_command(runtime: &mut LiveSessionRuntime, prompt: &str) -> String {
    match runtime.submit_prompt(prompt) {
        SubmitOutcome::Sent { command_id } => command_id,
        SubmitOutcome::RestoreText { reason, .. } => {
            panic!("production runtime rejected {prompt:?}: {reason}")
        }
    }
}

fn wait_for_runtime_user(runtime: &mut LiveSessionRuntime, command_id: &str, prompt: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported Session errors: {:?}",
            outcome.errors
        );
        if runtime.current_events().iter().any(|event| {
            event.command_id.as_deref() == Some(command_id)
                && matches!(
                    event.event,
                    SessionEventPayload::UserMessage { ref text, .. } if text == prompt
                )
        }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("production runtime did not durably acknowledge user prompt {prompt:?}");
}

fn wait_for_runtime_pair(runtime: &mut LiveSessionRuntime, command_id: Option<&str>, prompt: &str) {
    let expected_assistant = format!("Nopal deterministic assistant: {prompt}");
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported Session errors: {:?}",
            outcome.errors
        );
        let events = runtime.current_events();
        for event in events {
            assert_eq!(
                (event.plot_id.as_str(), event.session_id.as_str()),
                runtime
                    .selected_session_context()
                    .as_ref()
                    .map(|context| (context.plot_id.as_str(), context.session_id.as_str()))
                    .unwrap_or(("", "")),
                "timeline accepted a foreign Plot Session event"
            );
            if let SessionEventPayload::SessionError { message, .. } = &event.event {
                panic!("real Pi bridge reported Session error: {message}");
            }
        }
        let user = events.iter().find(|event| {
            event.command_id.as_deref() == command_id
                && matches!(
                    event.event,
                    SessionEventPayload::UserMessage { ref text, .. } if text == prompt
                )
        });
        let assistant = events.iter().find(|event| {
            event.command_id.as_deref() == command_id
                && matches!(
                    event.event,
                    SessionEventPayload::AssistantMessage { ref text, .. }
                        if text == &expected_assistant
                )
        });
        if let (Some(user), Some(assistant)) = (user, assistant) {
            assert_uuid(&user.event_id);
            assert_uuid(&assistant.event_id);
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "production runtime timeline did not receive user and assistant events for {prompt:?}; events={:?}",
        runtime.current_events()
    );
}

fn wait_for_fifo_pairs(
    runtime: &mut LiveSessionRuntime,
    first: (&str, &str),
    second: (&str, &str),
) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported Session errors: {:?}",
            outcome.errors
        );
        let events = runtime.current_events();
        let user_index = |command_id: &str, prompt: &str| {
            events.iter().position(|event| {
                event.command_id.as_deref() == Some(command_id)
                    && matches!(
                        event.event,
                        SessionEventPayload::UserMessage { text: ref actual, .. }
                            if actual == prompt
                    )
            })
        };
        let assistant_index = |command_id: &str, response: &str| {
            events.iter().position(|event| {
                event.command_id.as_deref() == Some(command_id)
                    && matches!(
                        event.event,
                        SessionEventPayload::AssistantMessage { text: ref actual, .. }
                            if actual == response
                    )
            })
        };
        let positions = (
            user_index(first.0, first.1),
            user_index(second.0, second.1),
            assistant_index(first.0, FIFO_FIRST_RESPONSE),
            assistant_index(second.0, FIFO_SECOND_RESPONSE),
        );
        if let (
            Some(first_user),
            Some(second_user),
            Some(first_assistant),
            Some(second_assistant),
        ) = positions
        {
            assert!(
                first_user < second_user
                    && second_user < first_assistant
                    && first_assistant < second_assistant,
                "rapid Composer events were not preserved in FIFO order: {positions:?}"
            );
            assert_assistant_attribution(runtime, FIFO_FIRST_RESPONSE, first.0);
            assert_assistant_attribution(runtime, FIFO_SECOND_RESPONSE, second.0);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event.event,
                        SessionEventPayload::AssistantMessage { ref text, .. }
                            if text == FIFO_FIRST_RESPONSE || text == FIFO_SECOND_RESPONSE
                    ))
                    .count(),
                2,
                "rapid Composer proof must produce exactly one complete assistant event per command"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "production runtime did not preserve rapid Composer FIFO turns {:?} then {:?}; events={:?}",
        first,
        second,
        runtime.current_events()
    );
}

fn wait_for_runtime_assistants(
    runtime: &mut LiveSessionRuntime,
    command_id: &str,
    prompt: &str,
    expected_assistants: &[&str],
) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let outcome = runtime.drain();
        assert!(
            outcome.errors.is_empty(),
            "production runtime reported Session errors: {:?}",
            outcome.errors
        );
        let events = runtime.current_events();
        if let Some(error) = events.iter().find_map(|event| match &event.event {
            SessionEventPayload::SessionError { message, .. } => Some(message),
            _ => None,
        }) {
            panic!("real Pi bridge reported Session error: {error}");
        }
        let user_seen = events.iter().any(|event| {
            event.command_id.as_deref() == Some(command_id)
                && matches!(
                    event.event,
                    SessionEventPayload::UserMessage { ref text, .. } if text == prompt
                )
        });
        let assistants_seen = expected_assistants.iter().all(|expected| {
            events.iter().any(|event| {
                event.command_id.as_deref() == Some(command_id)
                    && matches!(
                        event.event,
                        SessionEventPayload::AssistantMessage { ref text, .. }
                            if text == expected
                    )
            })
        });
        for expected in expected_assistants {
            if let Some(event) = events.iter().find(|event| {
                matches!(
                    event.event,
                    SessionEventPayload::AssistantMessage { ref text, .. }
                        if text == expected
                ) && event.command_id.as_deref() != Some(command_id)
            }) {
                panic!(
                    "assistant message {expected:?} was misattributed to {:?} instead of {command_id:?}",
                    event.command_id
                );
            }
        }
        if user_seen && assistants_seen {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "production runtime timeline did not receive the complete tool loop for {command_id:?}; events={:?}",
        runtime.current_events()
    );
}

fn assert_assistant_attribution(runtime: &LiveSessionRuntime, text: &str, command_id: &str) {
    let matches = runtime
        .current_events()
        .iter()
        .filter(|event| {
            matches!(
                event.event,
                SessionEventPayload::AssistantMessage { text: ref actual, .. }
                    if actual == text
            )
        })
        .collect::<Vec<_>>();
    assert!(!matches.is_empty(), "missing assistant message {text:?}");
    assert!(
        matches
            .iter()
            .all(|event| event.command_id.as_deref() == Some(command_id)),
        "assistant message {text:?} was attributed outside {command_id:?}: {matches:?}"
    );
}

fn assert_uuid(value: &str) {
    assert_eq!(value.len(), 36, "event id must be a UUID: {value:?}");
    assert_eq!(
        value.as_bytes().get(8),
        Some(&b'-'),
        "event id must be a UUID: {value:?}"
    );
    assert_eq!(
        value.as_bytes().get(13),
        Some(&b'-'),
        "event id must be a UUID: {value:?}"
    );
}

fn unix_millis() -> u128 {
    must(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH),
        "clock after Unix epoch",
    )
    .as_millis()
}

fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

fn must_debug<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error:?}"),
    }
}

fn must_some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{context}"),
    }
}

struct TmuxCleanup {
    tmux: PathBuf,
    session: String,
    active: bool,
}

impl TmuxCleanup {
    fn new(tmux: PathBuf, session: String) -> Self {
        Self {
            tmux,
            session,
            active: true,
        }
    }

    fn kill(&mut self) {
        if !self.active {
            return;
        }
        let _ = Command::new(&self.tmux)
            .args(["kill-session", "-t", &self.session])
            .output();
        self.active = false;
    }
}

impl Drop for TmuxCleanup {
    fn drop(&mut self) {
        self.kill();
    }
}
