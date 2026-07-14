//! tmux integration tests: real server, throwaway sessions.
//!
//! Ignored by default (they need a runnable tmux); run with
//! `cargo test -p nopal-field -- --ignored`.

use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock, mpsc};
use std::time::{Duration, Instant};

use nopal_field::AppEvent;
use nopal_field::embed::Embed;
use nopal_field::state::{
    App, Plot, PlotActivityKey, PlotExecution, PlotExecutionEvidence, PlotSession,
};
use nopal_field::tmux::{Backend, sidecar::Sidecar};
use nopal_field::ui;

static TMUX_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn serialize_tmux_tests() -> MutexGuard<'static, ()> {
    let mutex = TMUX_TEST_LOCK.get_or_init(|| Mutex::new(()));
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn pump_until(
    rx: &mpsc::Receiver<AppEvent>,
    app: &mut App,
    timeout: Duration,
    done: impl Fn(&App) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while !done(app) {
        if Instant::now() > deadline {
            return false;
        }
        if let Ok(AppEvent::Tmux(notification)) = rx.recv_timeout(Duration::from_millis(20)) {
            app.reduce_tmux(&notification);
        }
    }
    true
}

#[test]
#[ignore = "needs a runnable tmux server"]
fn backend_and_sidecar_round_trip() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-test-it-{}", std::process::id());
    let backend = Backend::new(session.clone());
    let _ = backend.kill_session();

    backend.create_session("sleep 60").unwrap();
    let seat_pane = backend
        .spawn_seat("alpha", "rondo", None, Some("sleep 60"))
        .unwrap();

    let (tx, rx) = mpsc::channel();
    let mut sidecar = Sidecar::attach(&session, tx).unwrap();
    sidecar.reconcile().unwrap();

    let mut app = App::new("%none".to_owned(), session.clone());
    // Field pane + slot shell + seat = 3 panes; the field pane is
    // excluded from seats, so 2 seat rows in THIS session (the inventory
    // is server-wide and may carry the operator's other sessions).
    let in_session = |app: &App| {
        app.seats
            .values()
            .filter(|s| s.session_name == session)
            .count()
    };
    let seen = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.focused_seat().is_some()
            && in_session(app) == 2
            && app.seats.values().any(|seat| {
                seat.session_name == session && seat.name == "alpha" && seat.repo == "rondo"
            })
    });
    assert!(seen, "seat inventory never converged: {:?}", app.seats);

    // Focus the seat: it must land in the field window (the slot).
    let slot = app.focused_seat().map(|seat| seat.pane_id.clone()).unwrap();
    let vacated = app.seats[&seat_pane].window_id.clone();
    let vacated_name = app.seats[&seat_pane].window_name.clone();
    backend
        .focus_seat(&seat_pane, &slot, &vacated, &vacated_name, "shell")
        .unwrap();
    sidecar.reconcile().unwrap();
    let focused = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.focused_seat()
            .map(|seat| seat.pane_id == seat_pane)
            .unwrap_or(false)
    });
    assert!(focused, "focused seat never became {seat_pane}");

    // Killing the seat's pane drops it from the inventory after the
    // reconcile the app loop issues on pane-topology notifications.
    backend.kill_pane(&seat_pane).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    sidecar.reconcile().unwrap();
    let gone = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        !app.seats.contains_key(&seat_pane)
    });
    assert!(gone, "killed pane still present: {:?}", app.seats);

    backend.kill_session().unwrap();
}

#[test]
#[ignore = "needs a runnable tmux server"]
fn plot_identity_round_trips_through_the_sidecar() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-plot-it-{}", std::process::id());
    let backend = Backend::new(session.clone());
    let _ = backend.kill_session();

    backend.create_session("sleep 60").unwrap();
    Backend::stamp_plot_identity(&session, "plot-1", "session-1").unwrap();

    let (tx, rx) = mpsc::channel();
    let mut sidecar = Sidecar::attach(&session, tx).unwrap();
    sidecar.reconcile().unwrap();
    let mut app = App::new("%none".to_owned(), session.clone());
    let seen = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.seats.values().any(|seat| {
            seat.session_name == session
                && seat.plot_id.as_deref() == Some("plot-1")
                && seat.plot_session_id.as_deref() == Some("session-1")
        })
    });
    assert!(
        seen,
        "Plot identity never reached the Field: {:?}",
        app.seats
    );

    backend.kill_session().unwrap();
}

/// Real tmux proof for the Plot activity stage boundary.
///
/// Durable activity identity stays selected when its Session pane vanishes,
/// the renderer degrades to an honest unavailable Session, and the sibling
/// execution remains inspectable without exposing an embedded input grid.
#[test]
#[ignore = "needs a runnable tmux server"]
fn plot_activity_stage_survives_pane_loss_and_renders_realistic_sizes() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-activity-it-{}", std::process::id());
    let backend = Backend::new(session.clone());
    let _ = backend.kill_session();

    backend.create_session("sleep 60").unwrap();
    Backend::stamp_plot_identity(&session, "plot-activity", "session-live").unwrap();

    let (tx, rx) = mpsc::channel();
    let mut sidecar = Sidecar::attach(&session, tx.clone()).unwrap();
    sidecar.reconcile().unwrap();
    let mut app = App::new("%none".to_owned(), session.clone());
    let seen = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.seats.values().any(|seat| {
            seat.session_name == session
                && seat.plot_id.as_deref() == Some("plot-activity")
                && seat.plot_session_id.as_deref() == Some("session-live")
        })
    });
    assert!(seen, "Plot Session seat never converged: {:?}", app.seats);
    let pane_id = app
        .seats
        .values()
        .find(|seat| seat.plot_session_id.as_deref() == Some("session-live"))
        .map(|seat| seat.pane_id.clone())
        .unwrap();

    app.plots.insert(
        "plot-activity".to_owned(),
        Plot {
            plot_id: "plot-activity".to_owned(),
            title: "Activity dogfood".to_owned(),
            provisional: false,
            progress: "active".to_owned(),
            conditions: vec!["Keep facts independent".to_owned()],
            seed_source: "test".to_owned(),
            seed_text: "Exercise the real tmux boundary".to_owned(),
            intent: "Prove Session and execution siblings".to_owned(),
            fruit_state: "absent".to_owned(),
            executions: vec![PlotExecution {
                service_id: "rondo-core".to_owned(),
                repo_id: "repo-activity".to_owned(),
                run_id: "run-activity".to_owned(),
                manifest_sha256: "a".repeat(64),
                status: "completed".to_owned(),
                outcome: Some("completed".to_owned()),
                event_cursor: "rondo.core/v1:2".to_owned(),
                evidence: vec![PlotExecutionEvidence {
                    artifact_kind: "final_report".to_owned(),
                    uri: "rondo-run://run-activity/artifacts/final-report.json".to_owned(),
                }],
                created_at: "2026-07-12T10:00:00Z".to_owned(),
                updated_at: "2026-07-12T10:01:00Z".to_owned(),
            }],
            sessions: vec![PlotSession {
                session_id: "session-live".to_owned(),
                mode: "interactive".to_owned(),
                host: "pi".to_owned(),
                host_session: session.clone(),
                host_pane: Some(pane_id.clone()),
                state: "active".to_owned(),
                workspace: Some("workspace-activity".to_owned()),
            }],
            selected_session_id: Some("session-live".to_owned()),
            establishment: None,
            repositories: Vec::new(),
            workspaces: Vec::new(),
        },
    );
    app.selected_plot_id = Some("plot-activity".to_owned());
    app.selected_plot_activity = Some(PlotActivityKey::Session("session-live".to_owned()));
    app.stage_open = true;
    app.embed = Some(Embed::open(&pane_id, "activity", tx).unwrap());

    for (width, height) in [(160, 45), (120, 36), (80, 24)] {
        let terminal_backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(terminal_backend).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(
            app.hit.embed_grid.is_some(),
            "missing live grid at {width}x{height}"
        );
        assert!(
            app.hit.main.is_some(),
            "missing activity region at {width}x{height}"
        );
    }

    backend.kill_pane(&pane_id).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    sidecar.reconcile().unwrap();
    let gone = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        !app.seats.contains_key(&pane_id)
    });
    assert!(gone, "killed Session pane still present: {:?}", app.seats);
    app.embed = None;

    let terminal_backend = ratatui::backend::TestBackend::new(160, 45);
    let mut terminal = ratatui::Terminal::new(terminal_backend).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let unavailable = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(app.stage_open);
    assert_eq!(
        app.selected_plot_activity,
        Some(PlotActivityKey::Session("session-live".to_owned()))
    );
    assert!(unavailable.contains("Session unavailable"));
    assert!(app.hit.embed_grid.is_none());

    app.select_plot_activity(PlotActivityKey::Execution {
        service_id: "rondo-core".to_owned(),
        repo_id: "repo-activity".to_owned(),
        run_id: "run-activity".to_owned(),
    });
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let execution = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(execution.contains("run-activity"));
    assert!(execution.contains("rondo-core / repo-activity / run-activity"));
    assert!(app.hit.embed_grid.is_none());
    assert!(app.hit.panel.is_none());

    backend.kill_session().unwrap();
}

/// End-to-end proof of pane-slot management against a real tmux server:
/// `join_seat_split` moves a seat pane into the field window as a real
/// split, stamping `@nopal_role=split` and re-asserting the sidebar's fixed
/// width; `break_seat_out` reverses it, landing the pane back in a fresh
/// `seat:<name>` window with the marker cleared and reporting that window's
/// id. `App::focused_seat` must keep resolving the true slot pane
/// throughout - never the joined split, even though both share the field
/// window's id - which is the slot-coherence property the design doc calls
/// out as the core of this pass.
#[test]
#[ignore = "needs a runnable tmux server"]
fn join_and_break_seat_split() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-split-it-{}", std::process::id());
    let backend = Backend::new(session.clone());
    let _ = backend.kill_session();

    backend.create_session("sleep 60").unwrap();
    let seat_pane = backend
        .spawn_seat("alpha", "rondo", None, Some("sleep 60"))
        .unwrap();

    let (tx, rx) = mpsc::channel();
    let mut sidecar = Sidecar::attach(&session, tx).unwrap();
    sidecar.reconcile().unwrap();

    // `App::new` takes a placeholder pane id here (this test process is not
    // literally running inside the field's own pane, unlike the real UI -
    // see `backend_and_sidecar_round_trip` above), so the real field pane
    // id `join_seat_split` needs to resize has to come straight from tmux.
    let field_pane = String::from_utf8(
        Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &session,
                "-f",
                "#{==:#{@nopal_role},field}",
                "-F",
                "#{pane_id}",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    assert!(!field_pane.is_empty(), "could not locate the field pane");

    let mut app = App::new("%none".to_owned(), session.clone());
    let seen = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.focused_seat().is_some()
            && app.seats.values().any(|seat| {
                seat.session_name == session && seat.name == "alpha" && !seat.is_split()
            })
    });
    assert!(seen, "seat never converged before split: {:?}", app.seats);

    let slot = app.focused_seat().map(|s| s.pane_id.clone()).unwrap();
    assert_ne!(slot, seat_pane, "the seat is not the slot before joining");

    Backend::join_seat_split(&seat_pane, &slot, &field_pane, true, false).unwrap();
    sidecar.reconcile().unwrap();
    let joined = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.seats.get(&seat_pane).is_some_and(|s| s.is_split())
    });
    assert!(
        joined,
        "seat never picked up the split marker: {:?}",
        app.seats
    );
    // The slot must still resolve to the ORIGINAL slot pane, never the
    // freshly joined split - the coherence property under test.
    assert_eq!(
        app.focused_seat().map(|s| s.pane_id.clone()),
        Some(slot.clone()),
        "focused_seat must skip a joined split"
    );

    let window_id = Backend::break_seat_out(&seat_pane, "alpha").unwrap();
    assert!(
        window_id.starts_with('@'),
        "break-pane must report a window id: {window_id}"
    );
    sidecar.reconcile().unwrap();
    let broke = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.seats.get(&seat_pane).is_some_and(|s| !s.is_split())
    });
    assert!(
        broke,
        "seat never cleared the split marker: {:?}",
        app.seats
    );

    backend.kill_session().unwrap();
}

/// The `before: true` leg of `join_seat_split` (row-drag
/// left/top edge drops, `join-pane -b`) against a real server: the pass 1
/// test above already covers the trailing-side (`before: false`) flag this
/// module's own unit test can't reach without a live tmux, so this only
/// needs to prove the `-b` argv actually joins rather than erroring or
/// silently dropping the flag - slot-coherence is already proven above and
/// does not depend on which side the join lands on.
#[test]
#[ignore = "needs a runnable tmux server"]
fn join_seat_split_before_flag_reaches_tmux() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-split-before-it-{}", std::process::id());
    let backend = Backend::new(session.clone());
    let _ = backend.kill_session();

    backend.create_session("sleep 60").unwrap();
    let seat_pane = backend
        .spawn_seat("alpha", "rondo", None, Some("sleep 60"))
        .unwrap();

    let (tx, rx) = mpsc::channel();
    let mut sidecar = Sidecar::attach(&session, tx).unwrap();
    sidecar.reconcile().unwrap();

    let field_pane = String::from_utf8(
        Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &session,
                "-f",
                "#{==:#{@nopal_role},field}",
                "-F",
                "#{pane_id}",
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    assert!(!field_pane.is_empty(), "could not locate the field pane");

    let mut app = App::new("%none".to_owned(), session.clone());
    let seen = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.focused_seat().is_some()
            && app.seats.values().any(|seat| {
                seat.session_name == session && seat.name == "alpha" && !seat.is_split()
            })
    });
    assert!(seen, "seat never converged before split: {:?}", app.seats);

    let slot = app.focused_seat().map(|s| s.pane_id.clone()).unwrap();
    Backend::join_seat_split(&seat_pane, &slot, &field_pane, true, true).unwrap();
    sidecar.reconcile().unwrap();
    let joined = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.seats.get(&seat_pane).is_some_and(|s| s.is_split())
    });
    assert!(
        joined,
        "a `-b` join must still land and pick up the split marker: {:?}",
        app.seats
    );
    assert_eq!(
        app.focused_seat().map(|s| s.pane_id.clone()),
        Some(slot),
        "focused_seat must skip the joined split regardless of which side it landed on"
    );

    backend.kill_session().unwrap();
}

/// End-to-end proof of the `w`/"swap into slot" control against a real
/// server: [`Backend::swap_seat_into_slot`] must land the seat in the slot
/// (`App::focused_seat` picks up the new occupant) exactly like
/// `focus_seat` does, but - unlike `focus_seat` - must NOT also take
/// tmux's own active-pane focus there; the whole point of factoring it out
/// of `focus_seat` is that `w` leaves the operator's tmux focus wherever
/// it already was (the field pane) while the sidebar mirror shows the
/// new occupant.
#[test]
#[ignore = "needs a runnable tmux server"]
fn swap_seat_into_slot_does_not_take_focus() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-swap-it-{}", std::process::id());
    let backend = Backend::new(session.clone());
    let _ = backend.kill_session();

    backend.create_session("sleep 60").unwrap();
    let seat_pane = backend
        .spawn_seat("alpha", "rondo", None, Some("sleep 60"))
        .unwrap();

    let (tx, rx) = mpsc::channel();
    let mut sidecar = Sidecar::attach(&session, tx).unwrap();
    sidecar.reconcile().unwrap();

    let mut app = App::new("%none".to_owned(), session.clone());
    let seen = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.focused_seat().is_some()
            && app.seats.values().any(|seat| {
                seat.session_name == session && seat.name == "alpha" && !seat.is_split()
            })
    });
    assert!(seen, "seat never converged before swap: {:?}", app.seats);

    let slot = app.focused_seat().map(|s| s.pane_id.clone()).unwrap();
    assert_ne!(slot, seat_pane, "the seat is not the slot before swapping");
    let vacated = app.seats[&seat_pane].window_id.clone();
    let vacated_name = app.seats[&seat_pane].window_name.clone();

    let active_pane = || -> String {
        String::from_utf8(
            Command::new("tmux")
                .args([
                    "list-panes",
                    "-t",
                    &session,
                    "-f",
                    "#{pane_active}",
                    "-F",
                    "#{pane_id}",
                ])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned()
    };
    let active_before = active_pane();

    Backend::swap_seat_into_slot(&seat_pane, &slot, &vacated, &vacated_name, "shell").unwrap();
    sidecar.reconcile().unwrap();
    let swapped = pump_until(&rx, &mut app, Duration::from_secs(5), |app| {
        app.focused_seat()
            .map(|seat| seat.pane_id == seat_pane)
            .unwrap_or(false)
    });
    assert!(
        swapped,
        "focused seat never became {seat_pane}: {:?}",
        app.seats
    );
    assert_eq!(
        active_pane(),
        active_before,
        "swap_seat_into_slot must not move tmux's active-pane focus"
    );

    backend.kill_session().unwrap();
}

/// Render the embed's *visible* screen into one string per row, in the same
/// display-offset-aware coordinate space `ui::draw_embed` uses: row =
/// `point.line + display_offset`, so a scrolled-back view yields the older
/// rows it is actually showing.
fn visible_rows(embed: &Embed) -> Vec<String> {
    let term = embed.term();
    let display_offset = term.grid().display_offset() as i32;
    let mut rows = vec![String::new(); embed.rows as usize];
    for indexed in term.renderable_content().display_iter {
        let row = indexed.point.line.0 + display_offset;
        if let Some(slot) = usize::try_from(row).ok().and_then(|r| rows.get_mut(r)) {
            slot.push(indexed.cell.c);
        }
    }
    rows.iter().map(|r| r.trim_end().to_owned()).collect()
}

/// The set of `LINEnnnn` markers currently visible, as their integer suffix.
fn visible_line_numbers(embed: &Embed) -> Vec<u32> {
    visible_rows(embed)
        .iter()
        .filter_map(|r| r.strip_prefix("LINE"))
        .filter_map(|n| n.parse::<u32>().ok())
        .collect()
}

/// End-to-end proof of embedded-pane interaction against a real tmux pane: the backfill
/// captures a pane's scrollback into the alacritty grid, `scroll_lines`
/// reveals older rows, a drag selection copies the exact cells, and mouse
/// reporting is detected so the wheel can be forwarded instead of scrolled.
#[test]
#[ignore = "needs a runnable tmux server (and pbcopy/pbpaste for the copy leg)"]
fn embed_scrollback_select_and_mouse_mode() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-embed-it-{}", std::process::id());
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .output();

    // A pane holding 500 uniquely-numbered lines of scrollback, then parked
    // on `sleep` so the pane stays alive and quiescent while we mirror it.
    Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "80",
            "-y",
            "24",
            "sh -c 'seq -f LINE%04g 1 500; sleep 300'",
        ])
        .status()
        .unwrap();
    let pane_id = String::from_utf8(
        Command::new("tmux")
            .args(["list-panes", "-t", &session, "-F", "#{pane_id}"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    // Let `seq` finish painting before we snapshot the scrollback.
    std::thread::sleep(Duration::from_millis(500));

    let (tx, _rx) = mpsc::channel();
    let mut embed = Embed::open(&pane_id, "alpha", tx).unwrap();

    // A plain shell never turns on mouse reporting: the wheel scrolls our
    // local scrollback rather than being forwarded to the seat.
    assert!(
        !embed.mouse_reporting(),
        "plain shell pane should not report mouse mode"
    );

    // Opening shows the live tail: the last printed line is visible, older
    // history is not.
    let tail = visible_line_numbers(&embed);
    assert!(
        tail.contains(&500),
        "tail should show LINE0500, saw {tail:?}"
    );
    assert!(
        !tail.contains(&450),
        "tail should not already show LINE0450, saw {tail:?}"
    );
    let tail_min = *tail.iter().min().unwrap();

    // Drag-select the whole `LINE0500` token on its row, then confirm the
    // copied text is exactly those cells.
    let row_500 = visible_rows(&embed)
        .iter()
        .position(|r| r == "LINE0500")
        .expect("LINE0500 should occupy a full visible row") as u16;
    embed.begin_selection(0, row_500, false);
    embed.update_selection(7, row_500); // "LINE0500" is 8 cells, cols 0..=7
    assert_eq!(
        embed.selection_text().as_deref(),
        Some("LINE0500"),
        "drag selection should copy the exact token"
    );

    // The copy leg actually reaches the system clipboard (pbcopy) and reads
    // back byte-identical (pbpaste). Save and restore the operator's real
    // clipboard around the write so the test leaves no trace.
    if let Some(saved) = pbpaste() {
        nopal_field::embed::copy_to_clipboard("LINE0500").unwrap();
        assert_eq!(
            pbpaste().as_deref(),
            Some("LINE0500"),
            "copy_to_clipboard should round-trip through the system clipboard"
        );
        restore_clipboard(&saved);
    }

    // Wheel toward history: `scroll_lines` moves the display offset and older
    // rows appear that the tail never showed.
    embed.scroll_lines(50);
    assert_eq!(
        embed.term().grid().display_offset(),
        50,
        "scroll_lines should move the display offset by the requested amount"
    );
    let scrolled = visible_line_numbers(&embed);
    let scrolled_min = *scrolled.iter().min().unwrap();
    assert!(
        scrolled_min < tail_min,
        "scrolling back should reveal older lines: tail min {tail_min}, scrolled min {scrolled_min}"
    );

    // Scrolling back to the live tail restores the offset to zero.
    embed.scroll_lines(-1000);
    assert_eq!(
        embed.term().grid().display_offset(),
        0,
        "scrolling past the tail should clamp at the live screen"
    );

    drop(embed);
    Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status()
        .unwrap();
}

/// A pane whose program has turned on mouse reporting is detected, so the
/// wheel is forwarded to the seat rather than moving local scrollback.
#[test]
#[ignore = "needs a runnable tmux server"]
fn embed_detects_mouse_reporting() {
    let _serial = serialize_tmux_tests();
    let session = format!("nopal-field-mouse-it-{}", std::process::id());
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .output();

    // Enable cell-motion mouse tracking (DECSET 1002), the middle of the
    // three MOUSE_MODE bits, then park - an alt-screen TUI like pi/vim would
    // set one of these.
    Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "80",
            "-y",
            "24",
            "sh -c 'printf \"\\033[?1002h\"; sleep 300'",
        ])
        .status()
        .unwrap();
    let pane_id = String::from_utf8(
        Command::new("tmux")
            .args(["list-panes", "-t", &session, "-F", "#{pane_id}"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    std::thread::sleep(Duration::from_millis(300));

    let (tx, _rx) = mpsc::channel();
    let embed = Embed::open(&pane_id, "beta", tx).unwrap();
    assert!(
        embed.mouse_reporting(),
        "a pane with DECSET 1002 set should report mouse mode"
    );

    drop(embed);
    Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status()
        .unwrap();
}

fn pbpaste() -> Option<String> {
    let out = Command::new("pbpaste").output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn restore_clipboard(text: &str) {
    use std::io::Write;
    if let Ok(mut child) = Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}
