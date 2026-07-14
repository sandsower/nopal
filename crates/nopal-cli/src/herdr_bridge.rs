//! Headless `nopal.field/v1` to herdr sidebar bridge.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use nopal_feed_client::field::{FieldAsk, FieldEntry, FieldSnapshot};
use serde::Deserialize;

const SOURCE: &str = "custom:nopal";
const AGENT: &str = "nopal";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct HerdrProtocolError {
    method: String,
    code: String,
    message: String,
}

impl std::fmt::Display for HerdrProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "herdr {} failed: {}: {}",
            self.method, self.code, self.message
        )
    }
}

impl std::error::Error for HerdrProtocolError {}

fn protocol_error_code(err: &io::Error) -> Option<&str> {
    err.get_ref()
        .and_then(|source| source.downcast_ref::<HerdrProtocolError>())
        .map(|error| error.code.as_str())
}

pub struct Options {
    pub dir: PathBuf,
    pub socket: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub interval: Duration,
    pub once: bool,
}

pub fn run(options: &Options) -> io::Result<ExitCode> {
    let env_socket = std::env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let socket = resolve_socket_path(
        options.socket.as_deref(),
        env_socket.as_deref(),
        home.as_deref(),
        xdg.as_deref(),
    );
    let nopal_bin = std::env::current_exe()?;
    loop {
        match poll_once(
            &socket,
            &nopal_bin,
            &options.dir,
            options.state_dir.as_deref(),
        ) {
            Ok(()) => {}
            Err(err) if socket_absent(&err) => {
                if options.once {
                    return Ok(ExitCode::SUCCESS);
                }
            }
            Err(err) if !options.once && socket_interrupted(&err) => {}
            Err(err) => return Err(err),
        }
        if options.once {
            return Ok(ExitCode::SUCCESS);
        }
        std::thread::sleep(options.interval);
    }
}

fn poll_once(
    socket: &Path,
    nopal_bin: &Path,
    dir: &Path,
    state_dir: Option<&Path>,
) -> io::Result<()> {
    let mut client = HerdrClient::connect(socket)?;
    let pong = client.ping()?;
    if pong.protocol == 0 || pong.version.is_empty() {
        return Err(io::Error::other(
            "herdr ping returned invalid version/protocol metadata",
        ));
    }
    let panes = client.panes()?;
    let snapshot = run_field(nopal_bin, dir, state_dir)?;
    let reports: BTreeMap<String, ProjectedReport> = project_reports(&snapshot, &panes)
        .into_iter()
        .map(|report| (report.pane_id.clone(), report))
        .collect();

    // Reconcile the bridge's exact authority against every pane in Herdr's
    // current snapshot. Herdr owns the durable report state, so a separate
    // `--once` invocation or a restarted daemon must not rely on process-local
    // claim history to clear a report whose Nopal correlation disappeared.
    // Conversely, panes absent from this snapshot are never addressed, which
    // avoids stale pane IDs terminating the daemon after a Herdr restart.
    for pane in &panes {
        if let Some(report) = reports.get(&pane.pane_id) {
            client.report_agent(report)?;
        } else {
            client.release_agent(&pane.pane_id)?;
        }
    }
    Ok(())
}

fn socket_absent(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

fn socket_interrupted(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

fn run_field(nopal_bin: &Path, dir: &Path, state_dir: Option<&Path>) -> io::Result<FieldSnapshot> {
    let mut command = Command::new(nopal_bin);
    command
        .args(["--dir"])
        .arg(dir)
        .args(["--json", "field", "inspect"]);
    if let Some(state_dir) = state_dir {
        command.arg("--state-dir").arg(state_dir);
    }
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|err| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        io::Error::other(format!(
            "nopal field returned malformed JSON: {err}; {}",
            stderr.trim()
        ))
    })?;
    nopal_feed_client::field::parse_field(&value).map_err(io::Error::other)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct HerdrPane {
    pane_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    foreground_cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectedReport {
    pane_id: String,
    state: String,
    custom_status: String,
    message: String,
    run_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pong {
    version: String,
    protocol: u32,
}

fn resolve_socket_path(
    explicit: Option<&Path>,
    env_socket: Option<&Path>,
    home: Option<&Path>,
    xdg_config_home: Option<&Path>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    if let Some(path) = env_socket {
        return path.to_path_buf();
    }
    if let Some(path) = xdg_config_home {
        return path.join("herdr/herdr.sock");
    }
    home.unwrap_or_else(|| Path::new("."))
        .join(".config/herdr/herdr.sock")
}

/// Project host-neutral feed facts into one semantic report per positively
/// correlated pane. All runs rooted at the pane cwd contribute; ordering is
/// stable so repeated polls produce byte-equivalent requests.
fn project_reports(snapshot: &FieldSnapshot, panes: &[HerdrPane]) -> Vec<ProjectedReport> {
    let mut reports = Vec::new();
    for pane in panes {
        let (matched, unbound) = matching_facts(snapshot, pane);
        if matched.is_empty() && unbound.is_empty() {
            continue;
        }
        let mut asks: BTreeMap<&str, &FieldAsk> = BTreeMap::new();
        for run in &matched {
            for ask in &run.asks {
                if ask.state == "pending" {
                    asks.insert(ask.ask_id.as_str(), ask);
                }
            }
        }
        for ask in unbound {
            asks.insert(ask.ask_id.as_str(), ask);
        }

        let mut run_ids: Vec<String> = matched.iter().map(|run| run.run_id.clone()).collect();
        run_ids.sort();
        let active = matched.iter().any(|run| run.status == "running");
        let state = if !asks.is_empty() {
            "blocked"
        } else if active {
            "working"
        } else {
            "unknown"
        };
        let run_status = aggregate_run_status(&matched);
        let gate_status = aggregate_gate_status(&matched);
        let custom_status = compact_status(asks.len(), &run_status, gate_status.as_deref());
        let message = format!(
            "{} run(s), {} pending ask(s){}",
            matched.len(),
            asks.len(),
            gate_status
                .as_deref()
                .map(|gate| format!(", gate {gate}"))
                .unwrap_or_default()
        );
        reports.push(ProjectedReport {
            pane_id: pane.pane_id.clone(),
            state: state.to_owned(),
            custom_status,
            message,
            run_ids,
        });
    }
    reports.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
    reports
}

fn matching_facts<'a>(
    snapshot: &'a FieldSnapshot,
    pane: &HerdrPane,
) -> (Vec<&'a FieldEntry>, Vec<&'a FieldAsk>) {
    for cwd in [pane.foreground_cwd.as_deref(), pane.cwd.as_deref()]
        .into_iter()
        .flatten()
    {
        let mut matched: Vec<&FieldEntry> = snapshot
            .entries
            .iter()
            .filter(|run| path_is_within(cwd, &run.placement.repo))
            .collect();
        if !matched.is_empty() {
            matched.sort_by(|a, b| {
                a.updated_at
                    .cmp(&b.updated_at)
                    .then(a.run_id.cmp(&b.run_id))
            });
        }
        let mut unbound: Vec<&FieldAsk> = snapshot
            .asks_unbound
            .iter()
            .filter(|ask| ask.state == "pending" && path_is_within(cwd, &ask.repo))
            .collect();
        unbound.sort_by(|a, b| a.ask_id.cmp(&b.ask_id));
        if !matched.is_empty() || !unbound.is_empty() {
            return (matched, unbound);
        }
    }
    (Vec::new(), Vec::new())
}

fn path_is_within(candidate: &str, repo: &str) -> bool {
    !candidate.is_empty() && !repo.is_empty() && Path::new(candidate).starts_with(Path::new(repo))
}

fn aggregate_run_status(runs: &[&FieldEntry]) -> String {
    runs.iter()
        .map(|run| run.status.as_str())
        .max_by_key(|status| (run_status_rank(status), *status))
        .unwrap_or("unknown")
        .to_owned()
}

fn aggregate_gate_status(runs: &[&FieldEntry]) -> Option<String> {
    runs.iter()
        .flat_map(|run| run.gates.iter())
        .map(|gate| gate.status.as_str())
        .max_by_key(|status| (gate_status_rank(status), *status))
        .map(str::to_owned)
}

fn run_status_rank(status: &str) -> u8 {
    match status {
        "running" => 4,
        "interrupted" => 3,
        "failed" => 2,
        "completed" => 1,
        _ => 0,
    }
}

fn gate_status_rank(status: &str) -> u8 {
    match status {
        "fail" | "failed" | "error" => 5,
        "blocked" => 4,
        "running" => 3,
        "pass" | "passed" => 2,
        "skipped" => 1,
        _ => 0,
    }
}

fn compact_status(ask_count: usize, run_status: &str, gate_status: Option<&str>) -> String {
    let text = if ask_count > 0 {
        match gate_status {
            Some(gate) => format!("ask:{ask_count} gate:{gate}"),
            None => format!("ask:{ask_count}"),
        }
    } else {
        match gate_status {
            Some(gate) => format!("run:{run_status} gate:{gate}"),
            None => format!("run:{run_status}"),
        }
    };
    sanitize_and_truncate(&text, 32)
}

fn sanitize_and_truncate(text: &str, limit: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

struct HerdrClient {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl HerdrClient {
    fn connect(path: &Path) -> io::Result<Self> {
        let writer = UnixStream::connect(path)?;
        writer.set_read_timeout(Some(SOCKET_TIMEOUT))?;
        writer.set_write_timeout(Some(SOCKET_TIMEOUT))?;
        let reader_stream = writer.try_clone()?;
        Ok(Self {
            writer,
            reader: BufReader::new(reader_stream),
            next_id: 1,
        })
    }

    fn ping(&mut self) -> io::Result<Pong> {
        let result = self.request("ping", serde_json::json!({}))?;
        if result.get("type").and_then(serde_json::Value::as_str) != Some("pong") {
            return Err(io::Error::other("herdr ping returned a non-pong response"));
        }
        let protocol = result
            .get("protocol")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| io::Error::other("herdr pong omitted a valid protocol version"))?;
        let version = result
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        Ok(Pong { version, protocol })
    }

    fn panes(&mut self) -> io::Result<Vec<HerdrPane>> {
        match self.request("session.snapshot", serde_json::json!({})) {
            Ok(result) => parse_panes_at(&result, &["snapshot", "panes"]),
            Err(err) if protocol_error_code(&err) == Some("unknown_method") => {
                let result = self.request("pane.list", serde_json::json!({}))?;
                parse_panes_at(&result, &["panes"])
            }
            Err(err) => Err(err),
        }
    }

    fn report_agent(&mut self, report: &ProjectedReport) -> io::Result<()> {
        match self.request(
            "pane.report_agent",
            serde_json::json!({
                "pane_id": report.pane_id,
                "source": SOURCE,
                "agent": AGENT,
                "state": report.state,
                "message": report.message,
                "custom_status": report.custom_status,
            }),
        ) {
            Ok(_) => Ok(()),
            Err(err) if protocol_error_code(&err) == Some("pane_not_found") => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn release_agent(&mut self, pane_id: &str) -> io::Result<()> {
        match self.request(
            "pane.release_agent",
            serde_json::json!({
                "pane_id": pane_id,
                "source": SOURCE,
                "agent": AGENT,
            }),
        ) {
            Ok(_) => Ok(()),
            Err(err) if protocol_error_code(&err) == Some("pane_not_found") => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> io::Result<serde_json::Value> {
        let id = format!("nopal-{}", self.next_id);
        self.next_id += 1;
        let request = serde_json::json!({"id": id, "method": method, "params": params});
        serde_json::to_writer(&mut self.writer, &request).map_err(io::Error::other)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;

        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("herdr closed the socket while answering {method}"),
            ));
        }
        let response: serde_json::Value = serde_json::from_str(&line).map_err(|err| {
            io::Error::other(format!("invalid herdr response for {method}: {err}"))
        })?;
        let response_id = response
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if response_id != id {
            return Err(io::Error::other(format!(
                "herdr response id mismatch for {method}: expected {id:?}, got {response_id:?}"
            )));
        }
        if let Some(error) = response.get("error") {
            let code = error
                .get("code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown_error");
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no message");
            return Err(io::Error::other(HerdrProtocolError {
                method: method.to_owned(),
                code: code.to_owned(),
                message: message.to_owned(),
            }));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| io::Error::other(format!("herdr {method} response omitted result")))
    }
}

fn parse_panes_at(result: &serde_json::Value, path: &[&str]) -> io::Result<Vec<HerdrPane>> {
    let mut value = result;
    for key in path {
        value = value.get(*key).ok_or_else(|| {
            io::Error::other(format!("herdr response omitted {}", path.join(".")))
        })?;
    }
    serde_json::from_value(value.clone())
        .map_err(|err| io::Error::other(format!("invalid herdr pane list: {err}")))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::thread;

    use super::*;

    fn field_fixture() -> nopal_feed_client::field::FieldSnapshot {
        nopal_feed_client::field::parse_field(&serde_json::json!({
            "kind": "nopal.field/v1",
            "entries": [
                {
                    "run_id": "run-b",
                    "status": "running",
                    "updated_at": "2026-07-09T12:02:00+00:00",
                    "placement": {"repo": "/work/nopal"},
                    "gates": [{"name": "test", "scope": "repo", "status": "pass"}],
                    "asks": []
                },
                {
                    "run_id": "run-a",
                    "status": "running",
                    "updated_at": "2026-07-09T12:01:00+00:00",
                    "placement": {"repo": "/work/nopal"},
                    "gates": [{"name": "fmt", "scope": "repo", "status": "fail"}],
                    "asks": [{
                        "ask_id": "ask-bound", "state": "pending", "repo": "/work/nopal"
                    }]
                }
            ],
            "asks_unbound": [{
                "ask_id": "ask-unbound", "state": "pending", "repo": "/work/nopal"
            }]
        }))
        .unwrap()
    }

    #[test]
    fn resolves_explicit_then_environment_then_default_socket() {
        assert_eq!(
            resolve_socket_path(
                Some(Path::new("/explicit.sock")),
                Some(Path::new("/env.sock")),
                Some(Path::new("/home/alex")),
                None,
            ),
            PathBuf::from("/explicit.sock")
        );
        assert_eq!(
            resolve_socket_path(
                None,
                Some(Path::new("/env.sock")),
                Some(Path::new("/home/alex")),
                None,
            ),
            PathBuf::from("/env.sock")
        );
        assert_eq!(
            resolve_socket_path(
                None,
                None,
                Some(Path::new("/home/alex")),
                Some(Path::new("/xdg")),
            ),
            PathBuf::from("/xdg/herdr/herdr.sock")
        );
        assert_eq!(
            resolve_socket_path(None, None, Some(Path::new("/home/alex")), None),
            PathBuf::from("/home/alex/.config/herdr/herdr.sock")
        );
    }

    #[test]
    fn projects_all_matching_runs_and_asks_with_attention_precedence() {
        let panes = vec![HerdrPane {
            pane_id: "w1:p1".to_owned(),
            cwd: Some("/work".to_owned()),
            foreground_cwd: Some("/work/nopal/crates/nopal-cli".to_owned()),
        }];

        let reports = project_reports(&field_fixture(), &panes);

        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_eq!(report.pane_id, "w1:p1");
        assert_eq!(report.state, "blocked");
        assert_eq!(report.custom_status, "ask:2 gate:fail");
        assert!(report.custom_status.chars().count() <= 32);
        assert_eq!(report.run_ids, vec!["run-a", "run-b"]);
    }

    #[test]
    fn falls_back_to_pane_cwd_and_ignores_unmatched_panes() {
        let panes = vec![
            HerdrPane {
                pane_id: "matched".to_owned(),
                cwd: Some("/work/nopal".to_owned()),
                foreground_cwd: Some("/tmp".to_owned()),
            },
            HerdrPane {
                pane_id: "unmatched".to_owned(),
                cwd: Some("/work/other".to_owned()),
                foreground_cwd: None,
            },
        ];

        let reports = project_reports(&field_fixture(), &panes);

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].pane_id, "matched");
    }

    #[test]
    fn repo_matched_unbound_ask_reports_blocked_without_a_live_run() {
        let snapshot = nopal_feed_client::field::parse_field(&serde_json::json!({
            "kind": "nopal.field/v1",
            "entries": [],
            "asks_unbound": [{
                "ask_id": "ask-session",
                "state": "pending",
                "repo": "/work/nopal"
            }]
        }))
        .unwrap();
        let panes = vec![HerdrPane {
            pane_id: "w1:p3".to_owned(),
            cwd: Some("/work/nopal/src".to_owned()),
            foreground_cwd: None,
        }];

        let reports = project_reports(&snapshot, &panes);

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].state, "blocked");
        assert_eq!(reports[0].custom_status, "ask:1");
        assert!(reports[0].run_ids.is_empty());
    }

    #[test]
    fn daemon_reconnects_for_socket_restart_errors_but_not_malformed_data() {
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::NotConnected,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::TimedOut,
            io::ErrorKind::WouldBlock,
        ] {
            assert!(socket_interrupted(&io::Error::new(kind, "restart")));
        }
        assert!(!socket_interrupted(&io::Error::other("malformed protocol")));
    }

    fn spawn_protocol_server(
        socket: &Path,
        handler: impl FnOnce(BufReader<std::os::unix::net::UnixStream>) + Send + 'static,
    ) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(socket).unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handler(BufReader::new(stream));
        })
    }

    fn read_request(reader: &mut BufReader<std::os::unix::net::UnixStream>) -> serde_json::Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn reply(reader: &mut BufReader<std::os::unix::net::UnixStream>, value: serde_json::Value) {
        let stream = reader.get_mut();
        let body = serde_json::to_vec(&value).unwrap();
        let split = body.len() / 2;
        stream.write_all(&body[..split]).unwrap();
        stream.flush().unwrap();
        stream.write_all(&body[split..]).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
    }

    #[test]
    fn unix_client_handles_partial_lines_ids_snapshot_report_and_release() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("herdr.sock");
        let server = spawn_protocol_server(&socket, |mut reader| {
            let ping = read_request(&mut reader);
            assert_eq!(ping["method"], "ping");
            reply(
                &mut reader,
                serde_json::json!({
                    "id": ping["id"],
                    "result": {"type": "pong", "version": "0.7.3", "protocol": 16}
                }),
            );

            let snapshot = read_request(&mut reader);
            assert_eq!(snapshot["method"], "session.snapshot");
            reply(
                &mut reader,
                serde_json::json!({
                    "id": snapshot["id"],
                    "result": {
                        "type": "session_snapshot",
                        "snapshot": {
                            "protocol": 16,
                            "panes": [{
                                "pane_id": "w1:p1",
                                "cwd": "/work/nopal",
                                "foreground_cwd": "/work/nopal/src",
                                "future": true
                            }]
                        }
                    }
                }),
            );

            let report = read_request(&mut reader);
            assert_eq!(report["method"], "pane.report_agent");
            assert_eq!(report["params"]["source"], SOURCE);
            assert_eq!(report["params"]["agent"], AGENT);
            assert_eq!(report["params"]["state"], "working");
            reply(
                &mut reader,
                serde_json::json!({"id": report["id"], "result": {"type": "ok"}}),
            );

            let release = read_request(&mut reader);
            assert_eq!(release["method"], "pane.release_agent");
            assert_eq!(release["params"]["source"], SOURCE);
            assert_eq!(release["params"]["agent"], AGENT);
            reply(
                &mut reader,
                serde_json::json!({"id": release["id"], "result": {"type": "ok"}}),
            );
        });

        let mut client = HerdrClient::connect(&socket).unwrap();
        let pong = client.ping().unwrap();
        assert_eq!(pong.protocol, 16);
        let panes = client.panes().unwrap();
        assert_eq!(panes[0].foreground_cwd.as_deref(), Some("/work/nopal/src"));
        client
            .report_agent(&ProjectedReport {
                pane_id: "w1:p1".to_owned(),
                state: "working".to_owned(),
                custom_status: "run:running".to_owned(),
                message: "1 active run".to_owned(),
                run_ids: vec!["run-1".to_owned()],
            })
            .unwrap();
        client.release_agent("w1:p1").unwrap();
        server.join().unwrap();
    }

    #[test]
    fn pane_not_found_is_benign_only_for_pane_mutations_and_codes_stay_structured() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("herdr.sock");
        let server = spawn_protocol_server(&socket, |mut reader| {
            for (expected_method, code) in [
                ("pane.report_agent", "pane_not_found"),
                ("pane.release_agent", "pane_not_found"),
                ("pane.report_agent", "permission_denied"),
            ] {
                let request = read_request(&mut reader);
                assert_eq!(request["method"], expected_method);
                reply(
                    &mut reader,
                    serde_json::json!({
                        "id": request["id"],
                        "error": {"code": code, "message": "mutation rejected"}
                    }),
                );
            }
        });

        let report = ProjectedReport {
            pane_id: "w1:p1".to_owned(),
            state: "working".to_owned(),
            custom_status: "run:running".to_owned(),
            message: "1 active run".to_owned(),
            run_ids: vec!["run-1".to_owned()],
        };
        let mut client = HerdrClient::connect(&socket).unwrap();
        client.report_agent(&report).unwrap();
        client.release_agent("w1:p1").unwrap();
        let err = client.report_agent(&report).unwrap_err();
        assert_eq!(protocol_error_code(&err), Some("permission_denied"));
        assert!(err.to_string().contains("mutation rejected"));
        server.join().unwrap();
    }

    #[test]
    fn unix_client_rejects_mismatched_ids_and_protocol_errors() {
        for response in [
            serde_json::json!({"id": "wrong", "result": {"type": "pong", "protocol": 16}}),
            serde_json::json!({"id": "nopal-1", "error": {"code": "bad", "message": "broken"}}),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let socket = tmp.path().join("herdr.sock");
            let server = spawn_protocol_server(&socket, move |mut reader| {
                let _ = read_request(&mut reader);
                reply(&mut reader, response);
            });
            let mut client = HerdrClient::connect(&socket).unwrap();
            assert!(client.ping().is_err());
            server.join().unwrap();
        }
    }

    #[test]
    fn unix_client_falls_back_to_pane_list_when_snapshot_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let socket = tmp.path().join("herdr.sock");
        let server = spawn_protocol_server(&socket, |mut reader| {
            let snapshot = read_request(&mut reader);
            assert_eq!(snapshot["method"], "session.snapshot");
            reply(
                &mut reader,
                serde_json::json!({
                    "id": snapshot["id"],
                    "error": {"code": "unknown_method", "message": "upgrade herdr"}
                }),
            );
            let list = read_request(&mut reader);
            assert_eq!(list["method"], "pane.list");
            reply(
                &mut reader,
                serde_json::json!({
                    "id": list["id"],
                    "result": {"type": "pane_list", "panes": [{"pane_id": "w1:p2"}]}
                }),
            );
        });

        let mut client = HerdrClient::connect(&socket).unwrap();
        assert_eq!(client.panes().unwrap()[0].pane_id, "w1:p2");
        server.join().unwrap();
    }
}
