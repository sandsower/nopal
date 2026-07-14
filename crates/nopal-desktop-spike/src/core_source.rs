//! Bounded production Core Field snapshot source.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nopal_feed_client::field::{FieldSnapshot, parse_field};
use nopal_field_presentation::field_refresh::{FieldLoadError, FieldLoadErrorKind, FieldLoader};
use nopal_native_lifecycle::application::CoreFieldSnapshotSource;
use nopal_native_lifecycle::supervisor::NativeApplicationUnavailable;

const DEFAULT_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreCommandOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_overflowed: bool,
    pub stderr_overflowed: bool,
}

/// Stable classification of a failure before a Core process returns output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreCommandErrorKind {
    /// The Core inspection process could not be started.
    Spawn,
    /// The process could not be supervised to a bounded completion.
    Unavailable,
}

/// Renderer-safe process-boundary failure without captured payload content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreCommandError {
    pub kind: CoreCommandErrorKind,
    pub message: String,
}

impl CoreCommandError {
    fn new(kind: CoreCommandErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub trait CoreCommandRunner {
    fn run(
        &self,
        executable: &Path,
        arguments: &[OsString],
        current_dir: Option<&Path>,
        output_limit: usize,
        query_timeout: Duration,
    ) -> Result<CoreCommandOutput, CoreCommandError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessCoreCommandRunner;

#[derive(Clone, Debug)]
pub struct CliCoreFieldSnapshotSource<R = ProcessCoreCommandRunner> {
    runner: R,
    executable: PathBuf,
    arguments: Vec<OsString>,
    current_dir: Option<PathBuf>,
    output_limit: usize,
    query_timeout: Duration,
}

impl CliCoreFieldSnapshotSource<ProcessCoreCommandRunner> {
    pub fn production(executable: impl Into<PathBuf>) -> Self {
        Self::new(ProcessCoreCommandRunner, executable)
    }
}

impl<R> CliCoreFieldSnapshotSource<R> {
    pub fn new(runner: R, executable: impl Into<PathBuf>) -> Self {
        Self {
            runner,
            executable: executable.into(),
            arguments: vec!["field".into(), "inspect".into(), "--json".into()],
            current_dir: None,
            output_limit: DEFAULT_OUTPUT_LIMIT,
            query_timeout: DEFAULT_QUERY_TIMEOUT,
        }
    }

    pub fn with_current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }

    /// Keeps the native Field and its Core inspection on one explicit state root.
    pub fn with_state_dir(mut self, state_dir: impl Into<PathBuf>) -> Self {
        self.arguments.push("--state-dir".into());
        self.arguments.push(state_dir.into().into_os_string());
        self
    }

    pub fn with_output_limit(mut self, output_limit: usize) -> Self {
        self.output_limit = output_limit;
        self
    }

    pub fn with_query_timeout(mut self, query_timeout: Duration) -> Self {
        self.query_timeout = query_timeout;
        self
    }
}

impl CoreCommandRunner for ProcessCoreCommandRunner {
    fn run(
        &self,
        executable: &Path,
        arguments: &[OsString],
        current_dir: Option<&Path>,
        output_limit: usize,
        query_timeout: Duration,
    ) -> Result<CoreCommandOutput, CoreCommandError> {
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }
        let mut child = command.spawn().map_err(|error| {
            CoreCommandError::new(
                CoreCommandErrorKind::Spawn,
                format!("cannot start Core Field query: {error}"),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            CoreCommandError::new(
                CoreCommandErrorKind::Unavailable,
                "Core Field query stdout was not captured",
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            CoreCommandError::new(
                CoreCommandErrorKind::Unavailable,
                "Core Field query stderr was not captured",
            )
        })?;
        let stdout_reader = thread::spawn(move || collect_bounded(stdout, output_limit));
        let stderr_reader = thread::spawn(move || collect_bounded(stderr, output_limit));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|error| {
                CoreCommandError::new(
                    CoreCommandErrorKind::Unavailable,
                    format!("cannot wait for Core Field query: {error}"),
                )
            })? {
                break status;
            }
            if started.elapsed() >= query_timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_reader(stdout_reader, "stdout");
                let _ = join_reader(stderr_reader, "stderr");
                return Err(CoreCommandError::new(
                    CoreCommandErrorKind::Unavailable,
                    "Core Field query exceeded its time allowance",
                ));
            }
            thread::sleep(Duration::from_millis(1));
        };
        let (stdout, stdout_overflowed) = join_reader(stdout_reader, "stdout")
            .map_err(|message| CoreCommandError::new(CoreCommandErrorKind::Unavailable, message))?;
        let (stderr, stderr_overflowed) = join_reader(stderr_reader, "stderr")
            .map_err(|message| CoreCommandError::new(CoreCommandErrorKind::Unavailable, message))?;
        Ok(CoreCommandOutput {
            success: status.success(),
            stdout,
            stderr,
            stdout_overflowed,
            stderr_overflowed,
        })
    }
}

fn collect_bounded(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut collected = Vec::with_capacity(limit.min(64 * 1024));
    let mut overflowed = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(collected.len());
        let retained = remaining.min(read);
        collected.extend_from_slice(&buffer[..retained]);
        overflowed |= retained < read;
    }
    Ok((collected, overflowed))
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
    stream: &str,
) -> Result<(Vec<u8>, bool), String> {
    reader
        .join()
        .map_err(|_| format!("Core Field query {stream} reader panicked"))?
        .map_err(|error| format!("cannot read Core Field query {stream}: {error}"))
}

impl<R> CoreFieldSnapshotSource for CliCoreFieldSnapshotSource<R>
where
    R: CoreCommandRunner,
{
    fn load_field_snapshot(&self) -> Result<FieldSnapshot, NativeApplicationUnavailable> {
        self.load_classified()
            .map_err(|error| NativeApplicationUnavailable::new(error.message().to_owned()))
    }
}

impl<R> CliCoreFieldSnapshotSource<R>
where
    R: CoreCommandRunner,
{
    fn load_classified(&self) -> Result<FieldSnapshot, FieldLoadError> {
        let output = self
            .runner
            .run(
                &self.executable,
                &self.arguments,
                self.current_dir.as_deref(),
                self.output_limit,
                self.query_timeout,
            )
            .map_err(|error| {
                let kind = match error.kind {
                    CoreCommandErrorKind::Spawn => FieldLoadErrorKind::Spawn,
                    CoreCommandErrorKind::Unavailable => FieldLoadErrorKind::Unavailable,
                };
                FieldLoadError::new(kind, error.message)
            })?;
        if output.stdout_overflowed || output.stderr_overflowed {
            return Err(FieldLoadError::new(
                FieldLoadErrorKind::OutputBound,
                "Core Field query exceeded its bounded output allowance",
            ));
        }
        if !output.success {
            return Err(FieldLoadError::new(
                FieldLoadErrorKind::NonzeroExit,
                "Core Field query exited unsuccessfully",
            ));
        }
        let stdout = std::str::from_utf8(&output.stdout).map_err(|_| {
            FieldLoadError::new(
                FieldLoadErrorKind::InvalidJson,
                "Core Field query returned non-UTF-8 output",
            )
        })?;
        let value: serde_json::Value = serde_json::from_str(stdout).map_err(|_| {
            FieldLoadError::new(
                FieldLoadErrorKind::InvalidJson,
                "Core Field query returned invalid JSON",
            )
        })?;
        parse_field(&value).map_err(|_| {
            FieldLoadError::new(
                FieldLoadErrorKind::WrongContractKind,
                "Core Field query did not satisfy the nopal.field/v1 contract",
            )
        })
    }
}

impl<R> FieldLoader for CliCoreFieldSnapshotSource<R>
where
    R: CoreCommandRunner + Send + 'static,
{
    fn load(&mut self) -> Result<FieldSnapshot, FieldLoadError> {
        self.load_classified()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Duration;

    use nopal_field_presentation::field_refresh::{FieldLoadErrorKind, FieldLoader};
    use nopal_native_lifecycle::application::CoreFieldSnapshotSource;

    use super::{
        CliCoreFieldSnapshotSource, CoreCommandError, CoreCommandErrorKind, CoreCommandOutput,
        CoreCommandRunner,
    };

    type RunnerCall = (PathBuf, Vec<OsString>, Option<PathBuf>, usize, Duration);

    struct StubRunner {
        calls: RefCell<Vec<RunnerCall>>,
        result: Result<CoreCommandOutput, CoreCommandError>,
    }

    impl CoreCommandRunner for StubRunner {
        fn run(
            &self,
            executable: &std::path::Path,
            arguments: &[OsString],
            current_dir: Option<&std::path::Path>,
            output_limit: usize,
            query_timeout: Duration,
        ) -> Result<CoreCommandOutput, CoreCommandError> {
            self.calls.borrow_mut().push((
                executable.to_path_buf(),
                arguments.to_vec(),
                current_dir.map(std::path::Path::to_path_buf),
                output_limit,
                query_timeout,
            ));
            self.result.clone()
        }
    }

    fn output(stdout: &[u8]) -> CoreCommandOutput {
        CoreCommandOutput {
            success: true,
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
            stdout_overflowed: false,
            stderr_overflowed: false,
        }
    }

    #[test]
    fn loads_one_exact_versioned_snapshot_through_the_bounded_cli_contract() {
        let runner = StubRunner {
            calls: RefCell::new(Vec::new()),
            result: Ok(output(
                br#"{"kind":"nopal.field/v1","plots":[],"entries":[]}"#,
            )),
        };
        let source = CliCoreFieldSnapshotSource::new(runner, "/opt/nopal")
            .with_current_dir("/repo")
            .with_state_dir("/state")
            .with_output_limit(4096);

        let snapshot = source.load_field_snapshot().expect("valid Field snapshot");

        assert_eq!(snapshot.kind, "nopal.field/v1");
        assert_eq!(
            source.runner.calls.into_inner(),
            vec![(
                PathBuf::from("/opt/nopal"),
                vec![
                    "field".into(),
                    "inspect".into(),
                    "--json".into(),
                    "--state-dir".into(),
                    "/state".into(),
                ],
                Some(PathBuf::from("/repo")),
                4096,
                super::DEFAULT_QUERY_TIMEOUT,
            )]
        );
    }

    #[test]
    fn rejects_overflow_invalid_utf8_nonzero_and_wrong_contract_without_echoing_payloads() {
        let cases = [
            CoreCommandOutput {
                success: true,
                stdout: b"secret".to_vec(),
                stderr: Vec::new(),
                stdout_overflowed: true,
                stderr_overflowed: false,
            },
            output(&[0xff]),
            CoreCommandOutput {
                success: false,
                stdout: b"secret".to_vec(),
                stderr: b"credential=secret".to_vec(),
                stdout_overflowed: false,
                stderr_overflowed: false,
            },
            output(br#"{"kind":"wrong/v1","plots":[],"entries":[]}"#),
        ];

        for command_output in cases {
            let source = CliCoreFieldSnapshotSource::new(
                StubRunner {
                    calls: RefCell::new(Vec::new()),
                    result: Ok(command_output),
                },
                "nopal",
            );
            let error = source
                .load_field_snapshot()
                .expect_err("unsafe output must fail closed")
                .to_string();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn refresh_loader_preserves_process_failure_classification() {
        let mut source = CliCoreFieldSnapshotSource::new(
            StubRunner {
                calls: RefCell::new(Vec::new()),
                result: Err(CoreCommandError::new(
                    CoreCommandErrorKind::Spawn,
                    "cannot start Core Field query",
                )),
            },
            "missing-nopal",
        );

        let error = FieldLoader::load(&mut source).expect_err("missing Core must fail");

        assert_eq!(error.kind(), FieldLoadErrorKind::Spawn);
        assert_eq!(error.message(), "cannot start Core Field query");
    }

    #[cfg(unix)]
    #[test]
    fn production_runner_crosses_a_real_bounded_process_boundary() {
        let mut source =
            super::CliCoreFieldSnapshotSource::production("/bin/sh").with_output_limit(4096);
        source.arguments = vec![
            "-c".into(),
            "printf '%s' '{\"kind\":\"nopal.field/v1\",\"plots\":[],\"entries\":[]}'".into(),
        ];

        let snapshot = source
            .load_field_snapshot()
            .expect("bounded production process snapshot");

        assert_eq!(snapshot.kind, "nopal.field/v1");
    }

    #[cfg(unix)]
    #[test]
    fn production_runner_kills_a_query_that_exceeds_its_total_time_allowance() {
        use std::time::{Duration, Instant};

        let mut source = super::CliCoreFieldSnapshotSource::production("/bin/sh")
            .with_query_timeout(Duration::from_millis(25));
        source.arguments = vec!["-c".into(), "exec sleep 5".into()];
        let started = Instant::now();

        let error = source
            .load_field_snapshot()
            .expect_err("slow query must time out");

        assert_eq!(
            error.to_string(),
            "Core Field query exceeded its time allowance"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
