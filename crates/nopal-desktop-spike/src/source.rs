use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nopal_feed_client::field::FieldSnapshot;
use nopal_feed_client::field::parse_field;

pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub struct ProcessRunner;

#[derive(Clone, Copy, Debug)]
pub struct TimedProcessRunner {
    timeout: Duration,
}

const PROCESS_TIMEOUT: Duration = Duration::from_secs(3);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(5);

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String> {
        run_process(program, args, PROCESS_TIMEOUT)
    }
}

impl TimedProcessRunner {
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl CommandRunner for TimedProcessRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String> {
        run_process(program, args, self.timeout)
    }
}

fn run_process(program: &str, args: &[&str], timeout: Duration) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("cannot capture {program} stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("cannot capture {program} stderr"))?;
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("cannot wait for {program}: {error}"))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            terminate_process_tree(&mut child);
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(format!(
                "{program} exceeded its {} millisecond process deadline",
                timeout.as_millis()
            ));
        }
        std::thread::sleep(PROCESS_POLL_INTERVAL);
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| format!("{program} stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| format!("{program} stderr reader panicked"))??;
    Ok(CommandOutput {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_tree(child: &mut std::process::Child) {
    if let Ok(process_group) = i32::try_from(child.id()) {
        // SAFETY: the child was created as the leader of a new process group, and a negative PID
        // targets only that group. SIGKILL is used after the command's total deadline expires.
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(not(unix))]
fn terminate_process_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

fn read_all(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read child process output: {error}"))?;
    Ok(bytes)
}

pub struct FieldSource<R> {
    runner: R,
}

impl<R> FieldSource<R>
where
    R: CommandRunner,
{
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn load(&self) -> Result<FieldSnapshot, String> {
        let output = self.runner.run("nopal", &["field", "inspect", "--json"])?;
        if !output.success {
            let detail = output.stderr.trim();
            return Err(if detail.is_empty() {
                "nopal field inspect failed".to_owned()
            } else {
                format!("nopal field inspect failed: {detail}")
            });
        }
        let value: serde_json::Value = serde_json::from_str(&output.stdout)
            .map_err(|error| format!("invalid nopal field JSON: {error}"))?;
        parse_field(&value)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use super::{CommandOutput, CommandRunner, FieldSource, run_process};

    struct StubRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        output: Result<CommandOutput, String>,
    }

    impl CommandRunner for StubRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String> {
            self.calls.borrow_mut().push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            self.output.clone()
        }
    }

    #[test]
    fn loads_the_versioned_field_contract_through_the_existing_cli_boundary() {
        let runner = StubRunner {
            calls: RefCell::new(Vec::new()),
            output: Ok(CommandOutput {
                success: true,
                stdout: serde_json::json!({
                    "kind": "nopal.field/v1",
                    "plots": [],
                    "entries": []
                })
                .to_string(),
                stderr: String::new(),
            }),
        };
        let source = FieldSource::new(runner);

        let snapshot = source.load().expect("valid snapshot");

        assert_eq!(snapshot.kind, "nopal.field/v1");
        assert_eq!(
            source.runner.calls.into_inner(),
            vec![(
                "nopal".to_owned(),
                vec![
                    "field".to_owned(),
                    "inspect".to_owned(),
                    "--json".to_owned()
                ]
            )]
        );
    }

    #[test]
    fn reports_command_failure_without_treating_stderr_as_contract_data() {
        let source = FieldSource::new(StubRunner {
            calls: RefCell::new(Vec::new()),
            output: Ok(CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "field unavailable".to_owned(),
            }),
        });

        assert_eq!(
            source.load().expect_err("failed command must degrade"),
            "nopal field inspect failed: field unavailable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn production_process_boundary_kills_a_hung_command_at_its_total_deadline() {
        let started = Instant::now();
        let error = run_process(
            "/bin/sh",
            &["-c", "sleep 5 & wait"],
            Duration::from_millis(25),
        )
        .expect_err("hung process tree must be bounded");

        assert!(error.contains("25 millisecond"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
