//! Scriptable feel benchmarks (survey benchmarks 7, 8, and 10, plus the
//! no-output firehose isolation that underwrites benchmarks 1 and 2).
//!
//! Runs headless against a throwaway tmux session named
//! `nopal-field-test-bench-<pid>`: real tmux server, real control-mode
//! sidecar, real state reduction, ratatui renders into an in-memory
//! `TestBackend`. Manual benchmarks and pass criteria live in
//! `crates/nopal-field/BENCHMARKS.md`.

use std::io;
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::embed::{self, Embed};
use crate::state::App;
use crate::tmux::{Backend, sidecar::Sidecar};
use crate::{AppEvent, ui};

#[derive(clap::Args, Debug)]
pub struct BenchArgs {
    /// Seats to spawn for the cold-open benchmark
    #[arg(long, default_value_t = 20)]
    seats: usize,

    /// Latency samples to collect
    #[arg(long, default_value_t = 50)]
    iterations: usize,

    /// Seconds of full-tilt seat output for the firehose benchmark
    #[arg(long, default_value_t = 3)]
    firehose_secs: u64,
}

pub fn run(args: &BenchArgs) -> io::Result<ExitCode> {
    let session = format!("nopal-field-test-bench-{}", std::process::id());
    let backend = Backend::new(session.clone());
    if backend.session_exists() {
        backend.kill_session()?;
    }
    let result = run_benchmarks(args, &backend);
    let _ = backend.kill_session();
    result.map(|()| ExitCode::SUCCESS)
}

fn run_benchmarks(args: &BenchArgs, backend: &Backend) -> io::Result<()> {
    println!("nopal field bench: session {}", backend.session);
    println!("tmux: {}", tmux_version()?);

    // Setup: session at history-limit 10000 with N sleeping seats.
    let setup_started = Instant::now();
    backend.create_session("sleep 600")?;
    // Note: set-option rejects the `=` exact-match prefix on session
    // targets (tmux 3.6a), unlike has-session/new-window.
    tmux_ok(&[
        "set-option",
        "-t",
        &backend.session,
        "history-limit",
        "10000",
    ])?;
    for index in 0..args.seats {
        backend.spawn_seat(
            &format!("seat-{index:02}"),
            "bench",
            None,
            Some("sleep 600"),
        )?;
    }
    println!(
        "setup: {} seats spawned in {:?}",
        args.seats,
        setup_started.elapsed()
    );

    cold_open(args, backend)?;
    latency(args, backend)?;
    embed_echo(args, backend)?;
    firehose(args, backend)?;
    idle(backend)?;
    Ok(())
}

/// Embedded-view echo latency (Feature 3): keystroke sent with
/// `send-keys -H` -> tty echo -> per-pane `pipe-pane` fifo -> VT parse ->
/// grid ready to render. This is an honest new metric: it will not beat raw
/// tmux (the flagship full-focus path stays for that), because it takes a
/// full server->fifo->parse round trip. A `cat` pane echoes typed bytes
/// immediately via the tty line discipline.
fn embed_echo(args: &BenchArgs, backend: &Backend) -> io::Result<()> {
    let pane = backend.spawn_seat("embed-echo", "bench", None, Some("cat"))?;
    std::thread::sleep(Duration::from_millis(200)); // let cat claim the pty

    let (tx, rx) = std::sync::mpsc::channel();
    let mut app = App::new("%none".to_owned(), backend.session.clone());
    app.embed = Some(Embed::open(&pane, "embed-echo", tx)?);
    let mut terminal = Terminal::new(TestBackend::new(200, 50)).map_err(io::Error::other)?;

    let mut samples = Vec::with_capacity(args.iterations);
    for index in 0..args.iterations {
        // A distinct visible ASCII char per iteration; we wait for its count
        // in the grid to rise, so the 90-char cycle repeating is harmless.
        let ch = (b'!' + (index % 90) as u8) as char;
        let before = grid_char_count(&app, ch);
        let started = Instant::now();
        embed::send_key(&pane, KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if grid_char_count(&app, ch) > before {
                break;
            }
            if Instant::now() > deadline {
                return Err(io::Error::other("embed echo timed out"));
            }
            if let Ok(AppEvent::Embed(chunk)) = rx.recv_timeout(Duration::from_millis(20))
                && let Some(embed) = &mut app.embed
                && embed.pane_id == chunk.pane_id
            {
                embed.advance(&chunk.data);
            }
        }
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .map_err(io::Error::other)?;
        samples.push(started.elapsed());
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[(samples.len() * 99 / 100).min(samples.len() - 1)];
    println!(
        "embedded-view echo over {} samples: p50 {:?} p99 {:?} (honest; full-focus `f` remains the zero-overhead path)",
        samples.len(),
        p50,
        p99
    );
    app.embed = None; // stop the pipe before the pane is killed
    backend.kill_pane(&pane)?;
    Ok(())
}

/// Count cells in the embedded grid holding `ch`.
fn grid_char_count(app: &App, ch: char) -> usize {
    let Some(embed) = &app.embed else {
        return 0;
    };
    embed
        .term()
        .renderable_content()
        .display_iter
        .filter(|indexed| indexed.cell.c == ch)
        .count()
}

/// Benchmark 7: field start to all sidebar rows live, < 1.5s @ 20 seats.
fn cold_open(args: &BenchArgs, backend: &Backend) -> io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    let mut sidecar = Sidecar::attach(&backend.session, tx)?;
    sidecar.reconcile()?;
    let mut app = App::new("%none".to_owned(), backend.session.clone());
    // +1: the focused-seat slot pane is a seat row too. The inventory is
    // server-wide now, so count only this benchmark session's panes.
    let wanted = args.seats + 1;
    let session = backend.session.clone();
    let deadline = Instant::now() + Duration::from_secs(10);
    pump_until(&rx, &mut app, deadline, |app| {
        bench_seats(app, &session) >= wanted
    })?;
    let mut terminal = test_terminal()?;
    terminal
        .draw(|frame| ui::draw(frame, &mut app))
        .map_err(io::Error::other)?;
    println!(
        "cold-open: attach -> {} sidebar rows live + first frame in {:?} (gate < 1.5s)",
        bench_seats(&app, &backend.session),
        started.elapsed()
    );
    Ok(())
}

/// Benchmark 8 proxy: tmux state change -> reduced state -> rendered frame.
fn latency(args: &BenchArgs, backend: &Backend) -> io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut sidecar = Sidecar::attach(&backend.session, tx)?;
    sidecar.reconcile()?;
    let mut app = App::new("%none".to_owned(), backend.session.clone());
    let session = backend.session.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    pump_until(&rx, &mut app, deadline, |app| {
        bench_seats(app, &session) > 0
    })?;
    let target_window = app
        .seats
        .values()
        .find(|seat| seat.session_name == backend.session)
        .map(|seat| seat.window_id.clone())
        .ok_or_else(|| io::Error::other("no seats to rename"))?;

    let mut terminal = test_terminal()?;
    let mut samples = Vec::with_capacity(args.iterations);
    for index in 0..args.iterations {
        let marker = format!("lat-{index:04}");
        let started = Instant::now();
        sidecar.send(&format!("rename-window -t {target_window} {marker}"))?;
        let deadline = Instant::now() + Duration::from_secs(2);
        pump_until(&rx, &mut app, deadline, |app| {
            app.seats.values().any(|seat| seat.window_name == marker)
        })?;
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .map_err(io::Error::other)?;
        samples.push(started.elapsed());
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[(samples.len() * 99 / 100).min(samples.len() - 1)];
    println!(
        "event->render latency over {} samples: p50 {:?} p99 {:?} (gate: sidebar <= 1s behind)",
        samples.len(),
        p50,
        p99
    );
    Ok(())
}

/// The design point behind benchmarks 1/2/10: with `-f no-output`, a seat
/// streaming full tilt sends the field nothing, so field CPU stays
/// ~0 and seat input paths are untouched (they never pass through us).
fn firehose(args: &BenchArgs, backend: &Backend) -> io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut sidecar = Sidecar::attach(&backend.session, tx)?;
    sidecar.reconcile()?;
    let mut app = App::new("%none".to_owned(), backend.session.clone());
    let session = backend.session.clone();
    pump_until(
        &rx,
        &mut app,
        Instant::now() + Duration::from_secs(5),
        |app| bench_seats(app, &session) > 0,
    )?;

    let firehose_pane = backend.spawn_seat("firehose", "bench", None, Some("yes firehose"))?;
    let cpu_before = self_cpu_ms()?;
    let events_before = app.events_reduced;
    let window = Duration::from_secs(args.firehose_secs);
    let end = Instant::now() + window;
    while Instant::now() < end {
        pump_once(&rx, &mut app, Duration::from_millis(50))?;
    }
    let cpu_after = self_cpu_ms()?;
    let events = app.events_reduced - events_before;
    println!(
        "firehose: {}s of `yes` in a seat -> {} notifications reached the field, field CPU delta {}ms (gate: ~0; output never crosses the control client)",
        args.firehose_secs,
        events,
        cpu_after.saturating_sub(cpu_before)
    );
    backend.kill_pane(&firehose_pane)?;
    Ok(())
}

/// Benchmark 10: idle cost ~0.
fn idle(backend: &Backend) -> io::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut sidecar = Sidecar::attach(&backend.session, tx)?;
    sidecar.reconcile()?;
    let mut app = App::new("%none".to_owned(), backend.session.clone());
    let session = backend.session.clone();
    pump_until(
        &rx,
        &mut app,
        Instant::now() + Duration::from_secs(5),
        |app| bench_seats(app, &session) > 0,
    )?;
    let cpu_before = self_cpu_ms()?;
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        pump_once(&rx, &mut app, Duration::from_millis(100))?;
    }
    let cpu_after = self_cpu_ms()?;
    println!(
        "idle: 3s attached -> field CPU delta {}ms (gate: ~0)",
        cpu_after.saturating_sub(cpu_before)
    );
    Ok(())
}

/// Panes belonging to the benchmark session only; the inventory is
/// server-wide and the operator's real sessions must not skew gates.
fn bench_seats(app: &App, session: &str) -> usize {
    app.seats
        .values()
        .filter(|seat| seat.session_name == session)
        .count()
}

fn test_terminal() -> io::Result<Terminal<TestBackend>> {
    Terminal::new(TestBackend::new(44, 60)).map_err(io::Error::other)
}

fn pump_once(rx: &Receiver<AppEvent>, app: &mut App, timeout: Duration) -> io::Result<()> {
    match rx.recv_timeout(timeout) {
        Ok(AppEvent::Tmux(notification)) => {
            app.reduce_tmux(&notification);
            Ok(())
        }
        Ok(_) | Err(RecvTimeoutError::Timeout) => Ok(()),
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("sidecar hung up")),
    }
}

fn pump_until(
    rx: &Receiver<AppEvent>,
    app: &mut App,
    deadline: Instant,
    done: impl Fn(&App) -> bool,
) -> io::Result<()> {
    while !done(app) {
        if Instant::now() > deadline {
            return Err(io::Error::other("benchmark condition timed out"));
        }
        pump_once(rx, app, Duration::from_millis(20))?;
    }
    Ok(())
}

/// Cumulative CPU time of this process in milliseconds, via `ps` (portable
/// across macOS/Linux without a libc dependency; 10ms resolution).
fn self_cpu_ms() -> io::Result<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "cputime=", "-p", &std::process::id().to_string()])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_cputime(text.trim())
        .ok_or_else(|| io::Error::other(format!("unparseable ps cputime {text:?}")))
}

/// Parse `[[hh:]mm:]ss.cc` into milliseconds.
fn parse_cputime(text: &str) -> Option<u64> {
    let mut total_ms = 0u64;
    for part in text.split(':') {
        let seconds: f64 = part.parse().ok()?;
        total_ms = total_ms * 60 + (seconds * 1000.0) as u64;
    }
    Some(total_ms)
}

fn tmux_version() -> io::Result<String> {
    let output = std::process::Command::new("tmux").arg("-V").output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn tmux_ok(args: &[&str]) -> io::Result<()> {
    let output = std::process::Command::new("tmux").args(args).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "tmux {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::parse_cputime;

    #[test]
    fn parses_ps_cputime_formats() {
        assert_eq!(parse_cputime("0:00.12"), Some(120));
        assert_eq!(parse_cputime("1:02.50"), Some(62_500));
        assert_eq!(parse_cputime("1:01:01.00"), Some(3_661_000));
        assert_eq!(parse_cputime("junk"), None);
    }
}
