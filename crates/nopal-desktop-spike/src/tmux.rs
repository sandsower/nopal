use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::JoinHandle;

use async_channel::Receiver;

use crate::source::CommandRunner;

fn resolve_tmux_program_from(
    override_program: Option<&OsStr>,
    search_path: Option<&OsStr>,
    fallbacks: &[PathBuf],
) -> OsString {
    if let Some(program) = override_program.filter(|program| !program.is_empty()) {
        return program.to_os_string();
    }
    if let Some(program) = search_path
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("tmux"))
        .find(|candidate| is_executable_file(candidate))
    {
        return program.into_os_string();
    }
    if let Some(program) = fallbacks
        .iter()
        .find(|candidate| is_executable_file(candidate))
    {
        return program.as_os_str().to_os_string();
    }
    OsString::from("tmux")
}

fn resolve_tmux_program() -> OsString {
    let mut fallbacks = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        fallbacks.push(home.join(".local/bin/tmux"));
        fallbacks.push(home.join(".nix-profile/bin/tmux"));
    }
    fallbacks.extend([
        PathBuf::from("/opt/homebrew/bin/tmux"),
        PathBuf::from("/usr/local/bin/tmux"),
        PathBuf::from("/opt/local/bin/tmux"),
        PathBuf::from("/nix/var/nix/profiles/default/bin/tmux"),
    ]);
    resolve_tmux_program_from(
        std::env::var_os("NOPAL_TMUX_BIN").as_deref(),
        std::env::var_os("PATH").as_deref(),
        &fallbacks,
    )
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub struct OwnedDemoSession {
    name: String,
    pane_id: String,
    tmux_program: OsString,
}

impl OwnedDemoSession {
    pub fn start() -> Result<Self, String> {
        let name = format!("nopal-clean-demo-{}", std::process::id());
        let tmux_program = resolve_tmux_program();
        let args = clean_demo_args(&name);
        let output = Command::new(&tmux_program)
            .args(&args)
            .output()
            .map_err(|error| format!("cannot launch clean demo Session: {error}"))?;
        if !output.status.success() {
            return Err(command_failure(
                "tmux new-session",
                &String::from_utf8_lossy(&output.stderr),
            ));
        }
        let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        validate_pane_id(&pane_id)?;
        Ok(Self {
            name,
            pane_id,
            tmux_program,
        })
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }
}

impl Drop for OwnedDemoSession {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux_program)
            .args(["kill-session", "-t", &self.name])
            .output();
    }
}

fn clean_demo_args(session: &str) -> Vec<String> {
    vec![
        "new-session".to_owned(),
        "-d".to_owned(),
        "-P".to_owned(),
        "-F".to_owned(),
        "#{pane_id}".to_owned(),
        "-s".to_owned(),
        session.to_owned(),
        "-x".to_owned(),
        "100".to_owned(),
        "-y".to_owned(),
        "30".to_owned(),
        "/usr/bin/env".to_owned(),
        "-i".to_owned(),
        "HOME=/tmp".to_owned(),
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        "TERM=xterm-256color".to_owned(),
        "PS1=nopal> ".to_owned(),
        "/bin/zsh".to_owned(),
        "-f".to_owned(),
    ]
}

pub trait PaneTransport {
    fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String>;
    fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String>;
}

impl<T> PaneTransport for Box<T>
where
    T: PaneTransport + ?Sized,
{
    fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String> {
        (**self).send_input(pane_id, bytes)
    }

    fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String> {
        (**self).resize_pane(pane_id, columns, rows)
    }
}

pub struct TmuxTransport<R> {
    runner: R,
    program: OsString,
}

impl<R> TmuxTransport<R>
where
    R: CommandRunner,
{
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            program: OsString::from("tmux"),
        }
    }

    pub fn production(runner: R) -> Self {
        Self {
            runner,
            program: resolve_tmux_program(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<crate::source::CommandOutput, String> {
        let program = self
            .program
            .to_str()
            .ok_or_else(|| "tmux executable path is not valid UTF-8".to_owned())?;
        self.runner.run(program, args)
    }

    pub fn capture(&self, pane_id: &str) -> Result<Vec<u8>, String> {
        validate_pane_id(pane_id)?;
        let output = self.run(&["capture-pane", "-e", "-p", "-S", "-5000", "-t", pane_id])?;
        if !output.success {
            return Err(command_failure("tmux capture-pane", &output.stderr));
        }
        Ok(normalize_capture(output.stdout.as_bytes()))
    }

    pub fn pane_size(&self, pane_id: &str) -> Result<(usize, usize), String> {
        validate_pane_id(pane_id)?;
        let output = self.run(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{pane_width} #{pane_height}",
        ])?;
        if !output.success {
            return Err(command_failure("tmux display-message", &output.stderr));
        }
        let mut dimensions = output.stdout.split_whitespace();
        let columns = dimensions.next().and_then(|value| value.parse().ok());
        let rows = dimensions.next().and_then(|value| value.parse().ok());
        match (columns, rows) {
            (Some(columns), Some(rows)) if columns > 0 && rows > 0 => Ok((columns, rows)),
            _ => Err("invalid tmux pane dimensions".to_owned()),
        }
    }

    pub fn pane_process_id(&self, pane_id: &str) -> Result<u32, String> {
        validate_pane_id(pane_id)?;
        let output = self.run(&["display-message", "-p", "-t", pane_id, "#{pane_pid}"])?;
        if !output.success {
            return Err(command_failure("tmux display-message", &output.stderr));
        }
        output
            .stdout
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|process_id| *process_id > 0)
            .ok_or_else(|| "invalid tmux pane process identity".to_owned())
    }

    pub fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String> {
        validate_pane_id(pane_id)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let mut args = vec![
            "send-keys".to_owned(),
            "-H".to_owned(),
            "-t".to_owned(),
            pane_id.to_owned(),
        ];
        args.extend(bytes.iter().map(|byte| format!("{byte:02X}")));
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let output = self.run(&arg_refs)?;
        if !output.success {
            return Err(command_failure("tmux send-keys", &output.stderr));
        }
        Ok(())
    }

    pub fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String> {
        validate_pane_id(pane_id)?;
        if columns < 2 || rows == 0 {
            return Err("invalid tmux pane dimensions".to_owned());
        }
        let layout = self.run(&[
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{window_panes} #{window_height} #{pane_height}",
        ])?;
        if !layout.success {
            return Err(command_failure("tmux display-message", &layout.stderr));
        }
        let mut values = layout.stdout.split_whitespace();
        let pane_count = values.next().and_then(|value| value.parse::<usize>().ok());
        let window_height = values.next().and_then(|value| value.parse::<usize>().ok());
        let pane_height = values.next().and_then(|value| value.parse::<usize>().ok());
        let (pane_count, window_height, pane_height) =
            match (pane_count, window_height, pane_height, values.next()) {
                (Some(pane_count), Some(window_height), Some(pane_height), None)
                    if pane_count > 0
                        && window_height > 0
                        && pane_height > 0
                        && window_height >= pane_height =>
                {
                    (pane_count, window_height, pane_height)
                }
                _ => return Err("invalid tmux pane layout".to_owned()),
            };
        if pane_count != 1 {
            return Err(
                "shared tmux window has multiple panes; native resize is unavailable".to_owned(),
            );
        }
        let columns = columns.to_string();
        let reserved_rows = window_height - pane_height;
        let window_rows = rows.saturating_add(reserved_rows).to_string();
        let output = self.run(&[
            "resize-window",
            "-t",
            pane_id,
            "-x",
            &columns,
            "-y",
            &window_rows,
        ])?;
        if !output.success {
            return Err(command_failure("tmux resize-window", &output.stderr));
        }
        Ok(())
    }
}

impl<R> PaneTransport for TmuxTransport<R>
where
    R: CommandRunner,
{
    fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String> {
        TmuxTransport::send_input(self, pane_id, bytes)
    }

    fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String> {
        TmuxTransport::resize_pane(self, pane_id, columns, rows)
    }
}

fn validate_pane_id(pane_id: &str) -> Result<(), String> {
    let valid = pane_id.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    });
    if valid {
        Ok(())
    } else {
        Err("invalid tmux pane identity".to_owned())
    }
}

fn command_failure(command: &str, stderr: &str) -> String {
    let detail = stderr.trim();
    if detail.is_empty() {
        format!("{command} failed")
    } else {
        format!("{command} failed: {detail}")
    }
}

pub struct LivePipe {
    pane_id: String,
    fifo_path: PathBuf,
    tmux_program: OsString,
    reader: Option<JoinHandle<()>>,
}

impl LivePipe {
    pub fn start(pane_id: &str) -> Result<(Self, Receiver<Vec<u8>>), String> {
        validate_pane_id(pane_id)?;
        let fifo_path = std::env::temp_dir()
            .join(format!("nopal-desktop-spike-{}", std::process::id()))
            .join(format!("pane-{}.fifo", &pane_id[1..]));
        if let Some(parent) = fifo_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create tmux pipe directory: {error}"))?;
        }
        if fifo_path.exists() {
            std::fs::remove_file(&fifo_path)
                .map_err(|error| format!("cannot replace stale tmux pipe: {error}"))?;
        }
        let mkfifo = Command::new("mkfifo")
            .arg(&fifo_path)
            .output()
            .map_err(|error| format!("cannot run mkfifo: {error}"))?;
        if !mkfifo.status.success() {
            return Err(command_failure(
                "mkfifo",
                &String::from_utf8_lossy(&mkfifo.stderr),
            ));
        }

        let sink = format!(
            "cat >> {}",
            shell_single_quote(&fifo_path.to_string_lossy())
        );
        let tmux_program = resolve_tmux_program();
        let pipe = Command::new(&tmux_program)
            .args(["pipe-pane", "-O", "-t", pane_id, &sink])
            .output()
            .map_err(|error| format!("cannot start tmux pipe-pane: {error}"))?;
        if !pipe.status.success() {
            let _ = std::fs::remove_file(&fifo_path);
            return Err(command_failure(
                "tmux pipe-pane",
                &String::from_utf8_lossy(&pipe.stderr),
            ));
        }

        let (sender, receiver) = async_channel::bounded(128);
        let reader_path = fifo_path.clone();
        let reader = std::thread::spawn(move || {
            let Ok(mut file) = File::open(reader_path) else {
                return;
            };
            let mut buffer = [0u8; 8192];
            loop {
                match file.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if sender.send_blocking(buffer[..count].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                pane_id: pane_id.to_owned(),
                fifo_path,
                tmux_program,
                reader: Some(reader),
            },
            receiver,
        ))
    }
}

impl Drop for LivePipe {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux_program)
            .args(["pipe-pane", "-t", &self.pane_id])
            .output();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = std::fs::remove_file(&self.fifo_path);
    }
}

fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn normalize_capture(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = b"\x1b[H\x1b[2J".to_vec();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            normalized.extend_from_slice(b"\r\n");
        }
        normalized.extend_from_slice(line);
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use crate::source::{CommandOutput, CommandRunner, ProcessRunner};

    use super::{LivePipe, TmuxTransport, clean_demo_args, resolve_tmux_program_from};

    #[test]
    fn native_tmux_resolution_survives_a_gui_path_without_homebrew() {
        let directory = tempfile::tempdir().expect("create executable fixture directory");
        let tmux = directory.path().join("tmux");
        std::fs::write(&tmux, b"fixture").expect("create executable fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700))
                .expect("make executable fixture runnable");
        }

        let resolved = resolve_tmux_program_from(
            None,
            Some(OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
            std::slice::from_ref(&tmux),
        );

        assert_eq!(resolved, tmux.as_os_str());
    }

    #[test]
    fn clean_demo_uses_an_isolated_shell_without_user_startup_files() {
        let args = clean_demo_args("nopal-clean-demo-test");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-s", "nopal-clean-demo-test"])
        );
        assert!(args.windows(2).any(|pair| pair == ["/usr/bin/env", "-i"]));
        assert!(args.ends_with(&["/bin/zsh".to_owned(), "-f".to_owned()]));
        assert!(args.contains(&"PS1=nopal> ".to_owned()));
    }

    struct StubRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        output: CommandOutput,
    }

    impl CommandRunner for StubRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, String> {
            self.calls.borrow_mut().push((
                program.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            Ok(self.output.clone())
        }
    }

    fn transport(stdout: &str) -> TmuxTransport<StubRunner> {
        TmuxTransport::new(StubRunner {
            calls: RefCell::new(Vec::new()),
            output: CommandOutput {
                success: true,
                stdout: stdout.to_owned(),
                stderr: String::new(),
            },
        })
    }

    #[test]
    fn captures_only_an_explicit_tmux_pane_identity() {
        let transport = transport("\u{1b}[31mhello\u{1b}[0m\n");

        let bytes = transport.capture("%17").expect("valid pane capture");

        assert_eq!(bytes, b"\x1b[H\x1b[2J\x1b[31mhello\x1b[0m\r\n");
        assert_eq!(
            transport.runner.calls.into_inner(),
            vec![(
                "tmux".to_owned(),
                vec![
                    "capture-pane".to_owned(),
                    "-e".to_owned(),
                    "-p".to_owned(),
                    "-S".to_owned(),
                    "-5000".to_owned(),
                    "-t".to_owned(),
                    "%17".to_owned()
                ]
            )]
        );
    }

    #[test]
    fn refuses_labels_and_shell_fragments_as_pane_identity() {
        let transport = transport("");

        assert_eq!(
            transport
                .capture("nopal; kill-server")
                .expect_err("must refuse"),
            "invalid tmux pane identity"
        );
        assert!(transport.runner.calls.into_inner().is_empty());
    }

    #[test]
    fn sends_terminal_bytes_as_exact_hex_tokens() {
        let transport = transport("");

        transport
            .send_input("%4", b"A\r\x1b[A")
            .expect("valid pane input");

        assert_eq!(
            transport.runner.calls.into_inner(),
            vec![(
                "tmux".to_owned(),
                vec![
                    "send-keys".to_owned(),
                    "-H".to_owned(),
                    "-t".to_owned(),
                    "%4".to_owned(),
                    "41".to_owned(),
                    "0D".to_owned(),
                    "1B".to_owned(),
                    "5B".to_owned(),
                    "41".to_owned()
                ]
            )]
        );
    }

    #[test]
    fn reads_the_real_pane_geometry_without_resizing_it() {
        let transport = transport("132 41\n");

        assert_eq!(transport.pane_size("%9").expect("pane geometry"), (132, 41));
        assert_eq!(
            transport.runner.calls.into_inner(),
            vec![(
                "tmux".to_owned(),
                vec![
                    "display-message".to_owned(),
                    "-p".to_owned(),
                    "-t".to_owned(),
                    "%9".to_owned(),
                    "#{pane_width} #{pane_height}".to_owned()
                ]
            )]
        );
    }

    #[test]
    fn resizes_only_the_explicit_pane_to_valid_dimensions() {
        let transport = transport("1 31 30\n");

        transport.resize_pane("%12", 100, 30).expect("resize");

        assert_eq!(
            transport.runner.calls.into_inner(),
            vec![
                (
                    "tmux".to_owned(),
                    vec![
                        "display-message".to_owned(),
                        "-p".to_owned(),
                        "-t".to_owned(),
                        "%12".to_owned(),
                        "#{window_panes} #{window_height} #{pane_height}".to_owned(),
                    ]
                ),
                (
                    "tmux".to_owned(),
                    vec![
                        "resize-window".to_owned(),
                        "-t".to_owned(),
                        "%12".to_owned(),
                        "-x".to_owned(),
                        "100".to_owned(),
                        "-y".to_owned(),
                        "31".to_owned()
                    ]
                )
            ]
        );
    }

    #[test]
    fn resize_does_not_invent_a_status_row_when_tmux_has_none() {
        let transport = transport("1 30 30\n");

        transport.resize_pane("%12", 100, 30).expect("resize");

        assert_eq!(
            transport.runner.calls.into_inner(),
            vec![
                (
                    "tmux".to_owned(),
                    vec![
                        "display-message".to_owned(),
                        "-p".to_owned(),
                        "-t".to_owned(),
                        "%12".to_owned(),
                        "#{window_panes} #{window_height} #{pane_height}".to_owned(),
                    ]
                ),
                (
                    "tmux".to_owned(),
                    vec![
                        "resize-window".to_owned(),
                        "-t".to_owned(),
                        "%12".to_owned(),
                        "-x".to_owned(),
                        "100".to_owned(),
                        "-y".to_owned(),
                        "30".to_owned()
                    ]
                )
            ]
        );
    }

    #[test]
    fn quotes_fifo_paths_as_one_shell_argument() {
        assert_eq!(
            super::shell_single_quote("/tmp/nopal's pipe"),
            "'/tmp/nopal'\\''s pipe'"
        );
    }

    #[test]
    fn normalizes_tmux_capture_rows_to_terminal_cursor_home_lines() {
        assert_eq!(
            super::normalize_capture(b"\x1b[31mone\x1b[0m\ntwo\n"),
            b"\x1b[H\x1b[2J\x1b[31mone\x1b[0m\r\ntwo\r\n"
        );
    }

    #[test]
    fn live_pipe_observes_output_from_exact_input_to_a_real_tmux_pane() {
        let session = format!("nopal-desktop-spike-test-{}", std::process::id());
        let created = Command::new("tmux")
            .args(["new-session", "-d", "-s", &session])
            .output()
            .expect("tmux must be installed for the native Session spike");
        assert!(
            created.status.success(),
            "cannot create isolated tmux fixture: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        let _cleanup = SessionCleanup(session.clone());
        let listed = Command::new("tmux")
            .args(["list-panes", "-t", &session, "-F", "#{pane_id}"])
            .output()
            .expect("list fixture pane");
        assert!(listed.status.success(), "cannot list isolated tmux fixture");
        let pane_id = String::from_utf8_lossy(&listed.stdout).trim().to_owned();
        let (_pipe, receiver) = LivePipe::start(&pane_id).expect("attach live pipe");

        TmuxTransport::new(ProcessRunner)
            .send_input(&pane_id, b"printf 'nopal-live-proof\\n'\r")
            .expect("route exact terminal input");

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut observed = Vec::new();
        while Instant::now() < deadline && !observed.windows(16).any(|w| w == b"nopal-live-proof") {
            match receiver.try_recv() {
                Ok(chunk) => observed.extend(chunk),
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(async_channel::TryRecvError::Closed) => break,
            }
        }
        assert!(
            observed.windows(16).any(|w| w == b"nopal-live-proof"),
            "live pipe did not observe fixture output: {:?}",
            String::from_utf8_lossy(&observed)
        );
    }

    struct SessionCleanup(String);

    impl Drop for SessionCleanup {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &self.0])
                .output();
        }
    }
}
