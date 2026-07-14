use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use nopal_feed_client::session::{
    DURABLE_SESSION_EVENT_KIND, DurableSessionEvent, SessionEventPayload,
};

use crate::model::SelectedSessionContext;
use crate::terminal::TerminalSurface;
use crate::timeline::{ReplayState, SessionTimelineStore};

const REPRESENTATIVE_SESSION_EVENTS: usize = 10_000;
const MAXIMUM_SESSION_EVENTS: usize = 100_000;
const SELECTION_CYCLES: usize = 10_000;

pub struct BenchmarkReport {
    pub bytes: usize,
    pub parse_and_damage_mib_per_second: f64,
    pub snapshot_handoff_p50_microseconds: u128,
    pub snapshot_handoff_p99_microseconds: u128,
    pub runs_per_snapshot: usize,
}

impl BenchmarkReport {
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "kind": "nopal.desktop-terminal-benchmark/v1",
            "bytes": self.bytes,
            "parse_and_damage_mib_per_second": self.parse_and_damage_mib_per_second,
            "snapshot_handoff_p50_microseconds": self.snapshot_handoff_p50_microseconds,
            "snapshot_handoff_p99_microseconds": self.snapshot_handoff_p99_microseconds,
            "runs_per_snapshot": self.runs_per_snapshot,
        })
        .to_string()
    }
}

pub fn run() -> BenchmarkReport {
    let workload = workload(8 * 1024 * 1024);
    let mut terminal = TerminalSurface::new(200, 60);
    let parse_start = Instant::now();
    for chunk in workload.chunks(8192) {
        terminal.apply_output(chunk);
    }
    let parse_seconds = parse_start.elapsed().as_secs_f64().max(f64::EPSILON);

    let mut samples = Vec::with_capacity(1_000);
    let mut runs_per_snapshot = 0;
    for _ in 0..1_000 {
        let started = Instant::now();
        let snapshot = terminal.snapshot();
        samples.push(started.elapsed().as_micros());
        runs_per_snapshot = snapshot.rows.iter().map(|row| row.runs.len()).sum();
    }
    samples.sort_unstable();

    BenchmarkReport {
        bytes: workload.len(),
        parse_and_damage_mib_per_second: workload.len() as f64 / (1024.0 * 1024.0) / parse_seconds,
        snapshot_handoff_p50_microseconds: percentile(&samples, 50),
        snapshot_handoff_p99_microseconds: percentile(&samples, 99),
        runs_per_snapshot,
    }
}

#[derive(Debug, Clone)]
pub struct SessionReplayBenchmarkReport {
    kind: &'static str,
    memory_proxy: MemoryProxyReport,
    workloads: Vec<SessionReplayWorkloadReport>,
    selection_a_b_a: SelectionBenchmarkReport,
}

impl SessionReplayBenchmarkReport {
    pub fn to_json(&self) -> String {
        let workloads = self
            .workloads
            .iter()
            .map(|workload| {
                serde_json::json!({
                    "event_count": workload.event_count,
                    "serialized_history_bytes": workload.serialized_history_bytes,
                    "cold_staged_replay_nanoseconds": workload.cold_staged_replay_nanoseconds,
                    "cold_staged_replay_events_per_second": workload.cold_staged_replay_events_per_second,
                    "midpoint_resume_from_sequence": workload.midpoint_resume_from_sequence,
                    "midpoint_resume_event_count": workload.midpoint_resume_event_count,
                    "midpoint_resume_nanoseconds": workload.midpoint_resume_nanoseconds,
                    "midpoint_resume_events_per_second": workload.midpoint_resume_events_per_second,
                    "exact_head_replay_nanoseconds": workload.exact_head_replay_nanoseconds,
                    "exact_head_validated": workload.exact_head_validated,
                    "duplicate_overlap_nanoseconds": workload.duplicate_overlap_nanoseconds,
                    "duplicate_overlap_validated": workload.duplicate_overlap_validated,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "kind": self.kind,
            "memory_proxy": {
                "kind": self.memory_proxy.kind,
                "scope": self.memory_proxy.scope,
                "includes_lf_framing": self.memory_proxy.includes_lf_framing,
                "not_process_rss": self.memory_proxy.not_process_rss,
            },
            "workloads": workloads,
            "selection_a_b_a": {
                "resident_events_per_session": self.selection_a_b_a.resident_events_per_session,
                "cycles": self.selection_a_b_a.cycles,
                "transitions": self.selection_a_b_a.transitions,
                "total_nanoseconds": self.selection_a_b_a.total_nanoseconds,
                "average_nanoseconds_per_transition": self.selection_a_b_a.average_nanoseconds_per_transition,
            },
        })
        .to_string()
    }
}

#[derive(Debug, Clone)]
struct MemoryProxyReport {
    kind: &'static str,
    scope: &'static str,
    includes_lf_framing: bool,
    not_process_rss: bool,
}

#[derive(Debug, Clone)]
struct SessionReplayWorkloadReport {
    event_count: usize,
    serialized_history_bytes: usize,
    cold_staged_replay_nanoseconds: u64,
    cold_staged_replay_events_per_second: u64,
    midpoint_resume_from_sequence: usize,
    midpoint_resume_event_count: usize,
    midpoint_resume_nanoseconds: u64,
    midpoint_resume_events_per_second: u64,
    exact_head_replay_nanoseconds: u64,
    exact_head_validated: bool,
    duplicate_overlap_nanoseconds: u64,
    duplicate_overlap_validated: bool,
}

#[derive(Debug, Clone)]
struct SelectionBenchmarkReport {
    resident_events_per_session: usize,
    cycles: usize,
    transitions: usize,
    total_nanoseconds: u64,
    average_nanoseconds_per_transition: u64,
}

pub fn run_session_replay() -> SessionReplayBenchmarkReport {
    run_session_replay_with_config(
        &[REPRESENTATIVE_SESSION_EVENTS, MAXIMUM_SESSION_EVENTS],
        SELECTION_CYCLES,
    )
}

fn run_session_replay_with_config(
    event_counts: &[usize],
    selection_cycles: usize,
) -> SessionReplayBenchmarkReport {
    assert!(
        !event_counts.is_empty() && event_counts.iter().all(|count| *count >= 2),
        "Session replay benchmark needs event counts of at least two"
    );
    let workloads = event_counts
        .iter()
        .map(|event_count| measure_session_replay(*event_count))
        .collect::<Vec<_>>();
    let resident_events = event_counts[0];
    let selection_a_b_a = measure_selection_a_b_a(resident_events, selection_cycles);

    SessionReplayBenchmarkReport {
        kind: "nopal.desktop-session-replay-benchmark/v1",
        memory_proxy: MemoryProxyReport {
            kind: "serialized_ndjson_history_bytes",
            scope: "serialized durable input history only",
            includes_lf_framing: true,
            not_process_rss: true,
        },
        workloads,
        selection_a_b_a,
    }
}

fn measure_session_replay(event_count: usize) -> SessionReplayWorkloadReport {
    let context = benchmark_context("a");
    let events = session_replay_workload(event_count, &context.plot_id, &context.session_id);
    let serialized_history_bytes = serialized_history_bytes(&events);
    let head = benchmark_some(events.last(), "Session replay benchmark head");

    let cold_started = Instant::now();
    let mut cold = SessionTimelineStore::default();
    cold.select_session(Some(&context));
    benchmark_must(cold.begin_replay(None), "begin cold staged replay");
    for event in &events {
        benchmark_must(
            cold.ingest_durable(event.clone()),
            "ingest cold staged replay event",
        );
    }
    benchmark_must(
        cold.complete_replay(Some(&head.cursor), event_count as u64),
        "complete cold staged replay",
    );
    let cold_elapsed = cold_started.elapsed();
    assert_replay_result(&cold, event_count, &head.cursor);

    let midpoint = event_count / 2;
    let midpoint_head = benchmark_some(events.get(midpoint - 1), "midpoint replay head");
    let mut resumed = SessionTimelineStore::default();
    resumed.select_session(Some(&context));
    benchmark_must(resumed.begin_replay(None), "begin midpoint prefix replay");
    for event in &events[..midpoint] {
        benchmark_must(
            resumed.ingest_durable(event.clone()),
            "ingest midpoint prefix event",
        );
    }
    benchmark_must(
        resumed.complete_replay(Some(&midpoint_head.cursor), midpoint as u64),
        "complete midpoint prefix replay",
    );
    let resume_started = Instant::now();
    benchmark_must(
        resumed.begin_replay(Some(&midpoint_head.cursor)),
        "begin midpoint resume",
    );
    for event in &events[midpoint..] {
        benchmark_must(
            resumed.ingest_durable(event.clone()),
            "ingest midpoint resume event",
        );
    }
    benchmark_must(
        resumed.complete_replay(Some(&head.cursor), (event_count - midpoint) as u64),
        "complete midpoint resume",
    );
    let resume_elapsed = resume_started.elapsed();
    assert_replay_result(&resumed, event_count, &head.cursor);

    let exact_head_started = Instant::now();
    benchmark_must(
        resumed.begin_replay(Some(&head.cursor)),
        "begin exact-head replay",
    );
    benchmark_must(
        resumed.complete_replay(Some(&head.cursor), 0),
        "complete exact-head replay",
    );
    let exact_head_elapsed = exact_head_started.elapsed();
    let exact_head_validated = replay_matches(&resumed, event_count, &head.cursor);

    let duplicate_started = Instant::now();
    benchmark_must(
        resumed.begin_replay(Some(&head.cursor)),
        "begin duplicate-overlap replay",
    );
    benchmark_must(
        resumed.ingest_durable(head.clone()),
        "ingest duplicate head overlap",
    );
    benchmark_must(
        resumed.complete_replay(Some(&head.cursor), 1),
        "complete duplicate-overlap replay",
    );
    let duplicate_elapsed = duplicate_started.elapsed();
    let duplicate_overlap_validated = replay_matches(&resumed, event_count, &head.cursor);
    assert!(
        exact_head_validated && duplicate_overlap_validated,
        "Session overlap benchmark changed verified history"
    );

    SessionReplayWorkloadReport {
        event_count,
        serialized_history_bytes,
        cold_staged_replay_nanoseconds: duration_nanoseconds(cold_elapsed),
        cold_staged_replay_events_per_second: events_per_second(event_count, cold_elapsed),
        midpoint_resume_from_sequence: midpoint,
        midpoint_resume_event_count: event_count - midpoint,
        midpoint_resume_nanoseconds: duration_nanoseconds(resume_elapsed),
        midpoint_resume_events_per_second: events_per_second(
            event_count - midpoint,
            resume_elapsed,
        ),
        exact_head_replay_nanoseconds: duration_nanoseconds(exact_head_elapsed),
        exact_head_validated,
        duplicate_overlap_nanoseconds: duration_nanoseconds(duplicate_elapsed),
        duplicate_overlap_validated,
    }
}

fn measure_selection_a_b_a(event_count: usize, cycles: usize) -> SelectionBenchmarkReport {
    let context_a = benchmark_context("selection-a");
    let context_b = benchmark_context("selection-b");
    let mut timelines = SessionTimelineStore::default();
    for context in [&context_a, &context_b] {
        let events = session_replay_workload(event_count, &context.plot_id, &context.session_id);
        let head_cursor = benchmark_some(events.last(), "selection benchmark head")
            .cursor
            .clone();
        timelines.select_session(Some(context));
        benchmark_must(
            timelines.begin_replay(None),
            "begin selection resident history",
        );
        for event in events {
            benchmark_must(
                timelines.ingest_durable(event),
                "ingest selection resident history",
            );
        }
        benchmark_must(
            timelines.complete_replay(Some(&head_cursor), event_count as u64),
            "complete selection resident history",
        );
    }

    let started = Instant::now();
    for _ in 0..cycles {
        timelines.select_session(Some(&context_a));
        timelines.select_session(Some(&context_b));
        timelines.select_session(Some(&context_a));
    }
    let elapsed = started.elapsed();
    assert_eq!(
        timelines.current_events().len(),
        event_count,
        "A/B/A selection lost resident Session history"
    );
    std::hint::black_box(&timelines);
    let transitions = cycles.saturating_mul(3);
    let total_nanoseconds = duration_nanoseconds(elapsed);

    SelectionBenchmarkReport {
        resident_events_per_session: event_count,
        cycles,
        transitions,
        total_nanoseconds,
        average_nanoseconds_per_transition: if transitions == 0 {
            0
        } else {
            total_nanoseconds / transitions as u64
        },
    }
}

fn session_replay_workload(
    event_count: usize,
    plot_id: &str,
    session_id: &str,
) -> Vec<DurableSessionEvent> {
    (1..=event_count)
        .map(|sequence| {
            let cursor = format!("cursor-benchmark-sequence-{sequence:06}");
            let command_id = format!("command-benchmark-turn-{:06}", sequence.div_ceil(2));
            let event = if sequence % 2 == 1 {
                SessionEventPayload::UserMessage {
                    text: format!(
                        "Benchmark user turn {} with deterministic structured Session content",
                        sequence.div_ceil(2)
                    ),
                    extra: BTreeMap::new(),
                }
            } else {
                SessionEventPayload::AssistantMessage {
                    text: format!(
                        "Benchmark assistant turn {} with deterministic replay content",
                        sequence / 2
                    ),
                    extra: BTreeMap::new(),
                }
            };
            DurableSessionEvent {
                kind: DURABLE_SESSION_EVENT_KIND.to_owned(),
                event_id: format!("event-benchmark-{sequence:06}"),
                plot_id: plot_id.to_owned(),
                session_id: session_id.to_owned(),
                stream_id: "stream-benchmark-v1".to_owned(),
                sequence: sequence as u64,
                previous_cursor: (sequence > 1)
                    .then(|| format!("cursor-benchmark-sequence-{:06}", sequence - 1)),
                cursor,
                command_id: Some(command_id),
                event,
                extra: BTreeMap::new(),
            }
        })
        .collect()
}

fn benchmark_context(suffix: &str) -> SelectedSessionContext {
    SelectedSessionContext {
        plot_id: format!("plot-benchmark-{suffix}"),
        session_id: format!("session-benchmark-{suffix}"),
        host_pane: None,
        protocol: None,
    }
}

fn serialized_history_bytes(events: &[DurableSessionEvent]) -> usize {
    events
        .iter()
        .map(|event| {
            serde_json::to_vec(event)
                .unwrap_or_else(|error| panic!("cannot serialize benchmark event: {error}"))
                .len()
                + 1
        })
        .sum()
}

fn replay_matches(timeline: &SessionTimelineStore, event_count: usize, cursor: &str) -> bool {
    timeline.current_events().len() == event_count
        && timeline.current_sequence() == Some(event_count as u64)
        && timeline.current_cursor() == Some(cursor)
        && timeline.current_replay_state() == ReplayState::Live
}

fn assert_replay_result(timeline: &SessionTimelineStore, event_count: usize, cursor: &str) {
    assert!(
        replay_matches(timeline, event_count, cursor),
        "Session replay benchmark did not produce an exact verified timeline"
    );
}

fn duration_nanoseconds(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn events_per_second(event_count: usize, duration: Duration) -> u64 {
    (event_count as f64 / duration.as_secs_f64().max(f64::EPSILON)) as u64
}

fn benchmark_must<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
    result.unwrap_or_else(|error| panic!("{context}: {error:?}"))
}

fn benchmark_some<T>(value: Option<T>, context: &str) -> T {
    value.unwrap_or_else(|| panic!("{context}"))
}

fn workload(target_bytes: usize) -> Vec<u8> {
    let lines = [
        b"\x1b[38;2;88;166;255mINFO\x1b[0m compiling nopal desktop surface\r\n".as_slice(),
        b"\x1b[33mWARN\x1b[0m evidence source is temporarily unavailable\r\n".as_slice(),
        "unicode: nopal 界 e\u{301} 🌵\r\n".as_bytes(),
        b"\x1b[2K\rprogress [========================] 100%\r\n".as_slice(),
    ];
    let mut output = Vec::with_capacity(target_bytes + 128);
    let mut index = 0;
    while output.len() < target_bytes {
        output.extend_from_slice(lines[index % lines.len()]);
        index += 1;
    }
    output
}

fn percentile(samples: &[u128], percentile: usize) -> u128 {
    let index = (samples.len().saturating_sub(1) * percentile) / 100;
    samples.get(index).copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{percentile, run_session_replay_with_config, session_replay_workload, workload};

    #[test]
    fn replay_workload_exercises_color_unicode_and_cursor_motion() {
        let workload = workload(1024);
        let text = String::from_utf8_lossy(&workload);

        assert!(workload.len() >= 1024);
        assert!(text.contains("\u{1b}[38;2;88;166;255m"));
        assert!(text.contains("界"));
        assert!(text.contains("\rprogress"));
    }

    #[test]
    fn percentile_selection_is_stable_and_bounded() {
        let samples = [1, 2, 3, 4, 100];

        assert_eq!(percentile(&samples, 50), 3);
        assert_eq!(percentile(&samples, 99), 4);
    }

    #[test]
    fn session_replay_workload_has_a_deterministic_contiguous_cursor_chain() {
        let events = session_replay_workload(4, "plot-benchmark-a", "session-benchmark-a");

        assert_eq!(events.len(), 4);
        for (index, event) in events.iter().enumerate() {
            assert_eq!(event.sequence, index as u64 + 1);
            assert_eq!(
                event.previous_cursor.as_deref(),
                index
                    .checked_sub(1)
                    .map(|previous| events[previous].cursor.as_str())
            );
            assert!(event.cursor.contains(&format!("sequence-{:06}", index + 1)));
        }
    }

    #[test]
    fn session_replay_report_has_a_stable_machine_readable_v1_shape() {
        let report = run_session_replay_with_config(&[8, 16], 4);
        let json: Value = serde_json::from_str(&report.to_json()).unwrap();

        assert_eq!(
            json.pointer("/kind").and_then(Value::as_str),
            Some("nopal.desktop-session-replay-benchmark/v1")
        );
        assert_eq!(
            json.pointer("/memory_proxy/kind").and_then(Value::as_str),
            Some("serialized_ndjson_history_bytes")
        );
        assert_eq!(
            json.pointer("/memory_proxy/not_process_rss")
                .and_then(Value::as_bool),
            Some(true)
        );
        let workloads = json
            .pointer("/workloads")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(workloads.len(), 2);
        assert_eq!(workloads[0]["event_count"], 8);
        assert_eq!(workloads[1]["event_count"], 16);
        for workload in workloads {
            assert!(workload["serialized_history_bytes"].as_u64().unwrap() > 0);
            assert_eq!(workload["exact_head_validated"], true);
            assert_eq!(workload["duplicate_overlap_validated"], true);
        }
        assert_eq!(json["selection_a_b_a"]["cycles"], 4);
        assert_eq!(json["selection_a_b_a"]["transitions"], 12);
    }
}
