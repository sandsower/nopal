//! Bounded Core Field refresh supervision.

use std::fmt;
use std::io;
use std::marker::PhantomData;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};

use nopal_feed_client::field::FieldSnapshot;
use nopal_native_lifecycle::current_field::{FieldGeneration, FieldRefreshTicket};

/// Stable classification of a failed Core Field load.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldLoadErrorKind {
    /// The Core inspection process could not be started.
    Spawn,
    /// Core inspection exited unsuccessfully.
    NonzeroExit,
    /// Core output exceeded the configured bound.
    OutputBound,
    /// Core output was not valid JSON.
    InvalidJson,
    /// JSON carried a different Field contract kind.
    WrongContractKind,
    /// The supervisor was cancelled before another load could begin.
    Cancelled,
    /// A loader-specific failure did not fit a narrower stable class.
    Unavailable,
}

/// Typed, renderer-safe failure from one Core Field load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldLoadError {
    kind: FieldLoadErrorKind,
    message: String,
}

impl FieldLoadError {
    /// Creates a classified failure with an actionable diagnostic.
    pub fn new(kind: FieldLoadErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable failure class.
    pub const fn kind(&self) -> FieldLoadErrorKind {
        self.kind
    }

    /// Returns the actionable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for FieldLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FieldLoadError {}

/// Effectful boundary for obtaining one complete Core Field snapshot.
pub trait FieldLoader: Send + 'static {
    /// Loads and validates one snapshot or returns a typed failure.
    fn load(&mut self) -> Result<FieldSnapshot, FieldLoadError>;
}

/// One bounded refresh result tagged with its acceptance ticket.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldRefreshUpdate {
    /// Core returned a complete candidate snapshot.
    Loaded {
        /// Generation fence to present to current Core authority.
        ticket: FieldRefreshTicket,
        /// Candidate facts, not yet accepted by the presentation coordinator.
        snapshot: FieldSnapshot,
    },
    /// The load failed without replacing the last good snapshot.
    Failed {
        /// Generation whose request failed.
        ticket: FieldRefreshTicket,
        /// Classified failure to expose as freshness state.
        error: FieldLoadError,
    },
}

impl FieldRefreshUpdate {
    /// Returns the generation carried by this result.
    pub const fn generation(&self) -> FieldGeneration {
        match self {
            Self::Loaded { ticket, .. } | Self::Failed { ticket, .. } => ticket.generation(),
        }
    }
}

/// Result of asking the bounded supervisor to load a generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshRequestOutcome {
    /// A worker began this request immediately.
    Started,
    /// One running load remains and this became the sole pending request.
    QueuedLatest {
        /// Pending generation discarded in favor of this newer request.
        replaced: Option<FieldGeneration>,
    },
    /// Shutdown has begun, so no more Core work can start.
    Cancelled,
}

enum WorkerCommand {
    Load(FieldRefreshTicket),
    Cancel,
}

/// One worker, one in-flight load, one latest pending ticket, and bounded results.
pub struct FieldRefresh<L: FieldLoader> {
    commands: Option<Sender<WorkerCommand>>,
    results: Receiver<FieldRefreshUpdate>,
    worker: Option<JoinHandle<()>>,
    in_flight: Option<FieldRefreshTicket>,
    pending: Option<FieldRefreshTicket>,
    result_bound: usize,
    cancelled: bool,
    loader: PhantomData<fn() -> L>,
}

impl<L: FieldLoader> FieldRefresh<L> {
    /// Starts a dedicated serial worker with a bounded result channel.
    ///
    /// A zero bound is tightened to one because at least the active result must
    /// be observable before the next pending request can begin.
    pub fn new(mut loader: L, result_bound: usize) -> io::Result<Self> {
        let result_bound = result_bound.max(1);
        let (commands_tx, commands_rx) = mpsc::channel();
        let (results_tx, results_rx) = mpsc::sync_channel(result_bound);
        let worker = thread::Builder::new()
            .name("nopal-field-refresh".to_owned())
            .spawn(move || worker_loop(&mut loader, commands_rx, results_tx))?;

        Ok(Self {
            commands: Some(commands_tx),
            results: results_rx,
            worker: Some(worker),
            in_flight: None,
            pending: None,
            result_bound,
            cancelled: false,
            loader: PhantomData,
        })
    }

    /// Starts a load or replaces the sole pending ticket with the latest request.
    pub fn request(&mut self, ticket: FieldRefreshTicket) -> RefreshRequestOutcome {
        if self.cancelled {
            return RefreshRequestOutcome::Cancelled;
        }
        if self.in_flight.is_some() {
            let replaced = self.pending.replace(ticket).map(|old| old.generation());
            return RefreshRequestOutcome::QueuedLatest { replaced };
        }
        if self.dispatch(ticket) {
            RefreshRequestOutcome::Started
        } else {
            RefreshRequestOutcome::Cancelled
        }
    }

    /// Drains at most the configured result bound and then starts the latest pending load.
    pub fn drain(&mut self) -> Vec<FieldRefreshUpdate> {
        if self.cancelled {
            while self.results.try_recv().is_ok() {}
            return Vec::new();
        }

        let mut updates = Vec::with_capacity(self.result_bound);
        while updates.len() < self.result_bound {
            match self.results.try_recv() {
                Ok(update) => {
                    self.in_flight = None;
                    updates.push(update);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if self.in_flight.is_none()
            && let Some(ticket) = self.pending.take()
        {
            self.dispatch(ticket);
        }
        updates
    }

    /// Cancels future work, discards pending/results, and joins the worker exactly once.
    pub fn cancel(&mut self) {
        if self.cancelled {
            return;
        }
        self.cancelled = true;
        self.pending = None;
        self.in_flight = None;
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(WorkerCommand::Cancel);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        while self.results.try_recv().is_ok() {}
    }

    fn dispatch(&mut self, ticket: FieldRefreshTicket) -> bool {
        let Some(commands) = self.commands.as_ref() else {
            self.cancelled = true;
            return false;
        };
        if commands.send(WorkerCommand::Load(ticket)).is_err() {
            self.cancelled = true;
            self.commands = None;
            return false;
        }
        self.in_flight = Some(ticket);
        true
    }
}

impl<L: FieldLoader> Drop for FieldRefresh<L> {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn worker_loop<L: FieldLoader>(
    loader: &mut L,
    commands: Receiver<WorkerCommand>,
    results: SyncSender<FieldRefreshUpdate>,
) {
    while let Ok(command) = commands.recv() {
        let ticket = match command {
            WorkerCommand::Load(ticket) => ticket,
            WorkerCommand::Cancel => return,
        };
        let update = match loader.load() {
            Ok(snapshot) => FieldRefreshUpdate::Loaded { ticket, snapshot },
            Err(error) => FieldRefreshUpdate::Failed { ticket, error },
        };
        if results.send(update).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use nopal_feed_client::field::FieldSnapshot;
    use nopal_native_lifecycle::current_field::{FieldGeneration, FieldRefreshTicket};

    use super::{
        FieldLoadError, FieldLoadErrorKind, FieldLoader, FieldRefresh, FieldRefreshUpdate,
        RefreshRequestOutcome,
    };

    struct ControlledLoader {
        starts: Sender<()>,
        results: Arc<Mutex<Receiver<Result<FieldSnapshot, FieldLoadError>>>>,
    }

    impl FieldLoader for ControlledLoader {
        fn load(&mut self) -> Result<FieldSnapshot, FieldLoadError> {
            self.starts.send(()).expect("record load start");
            self.results
                .lock()
                .expect("result receiver lock should remain available")
                .recv()
                .expect("test should provide a load result")
        }
    }

    #[test]
    fn runs_one_load_and_keeps_only_the_latest_pending_ticket() {
        let (starts_tx, starts_rx) = mpsc::channel();
        let (results_tx, results_rx) = mpsc::channel();
        let mut refresh = FieldRefresh::new(
            ControlledLoader {
                starts: starts_tx,
                results: Arc::new(Mutex::new(results_rx)),
            },
            1,
        )
        .expect("refresh worker should start");
        let first = ticket(1);
        let superseded = ticket(2);
        let latest = ticket(3);

        assert_eq!(refresh.request(first), RefreshRequestOutcome::Started);
        starts_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first load should start");
        assert_eq!(
            refresh.request(superseded),
            RefreshRequestOutcome::QueuedLatest { replaced: None }
        );
        assert_eq!(
            refresh.request(latest),
            RefreshRequestOutcome::QueuedLatest {
                replaced: Some(superseded.generation()),
            }
        );
        assert!(starts_rx.try_recv().is_err());

        results_tx
            .send(Ok(field("plot-first")))
            .expect("release first load");
        assert_eq!(
            wait_for_update(&mut refresh),
            FieldRefreshUpdate::Loaded {
                ticket: first,
                snapshot: field("plot-first"),
            }
        );
        starts_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("latest pending load should start after drain");
        results_tx
            .send(Ok(field("plot-latest")))
            .expect("release latest load");
        assert_eq!(
            wait_for_update(&mut refresh),
            FieldRefreshUpdate::Loaded {
                ticket: latest,
                snapshot: field("plot-latest"),
            }
        );
    }

    #[test]
    fn failed_load_is_typed_and_drain_never_exceeds_the_result_bound() {
        let (starts_tx, starts_rx) = mpsc::channel();
        let (results_tx, results_rx) = mpsc::channel();
        let mut refresh = FieldRefresh::new(
            ControlledLoader {
                starts: starts_tx,
                results: Arc::new(Mutex::new(results_rx)),
            },
            1,
        )
        .expect("refresh worker should start");
        let requested = ticket(7);
        refresh.request(requested);
        starts_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("load should start");
        results_tx
            .send(Err(FieldLoadError::new(
                FieldLoadErrorKind::InvalidJson,
                "Core returned malformed Field JSON",
            )))
            .expect("release failed load");

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let updates = refresh.drain();
            assert!(updates.len() <= 1);
            if let Some(update) = updates.into_iter().next() {
                assert_eq!(
                    update,
                    FieldRefreshUpdate::Failed {
                        ticket: requested,
                        error: FieldLoadError::new(
                            FieldLoadErrorKind::InvalidJson,
                            "Core returned malformed Field JSON",
                        ),
                    }
                );
                break;
            }
            assert!(Instant::now() < deadline, "failed refresh should arrive");
            std::thread::yield_now();
        }
    }

    #[test]
    fn cancellation_joins_once_discards_pending_and_rejects_new_requests() {
        let (starts_tx, starts_rx) = mpsc::channel();
        let (results_tx, results_rx) = mpsc::channel();
        let mut refresh = FieldRefresh::new(
            ControlledLoader {
                starts: starts_tx,
                results: Arc::new(Mutex::new(results_rx)),
            },
            1,
        )
        .expect("refresh worker should start");
        refresh.request(ticket(1));
        starts_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("load should start");
        refresh.request(ticket(2));
        results_tx
            .send(Ok(field("plot-first")))
            .expect("allow active load to finish");

        refresh.cancel();
        refresh.cancel();

        assert_eq!(refresh.request(ticket(3)), RefreshRequestOutcome::Cancelled);
        assert!(refresh.drain().is_empty());
        assert!(starts_rx.try_recv().is_err());
    }

    fn wait_for_update(refresh: &mut FieldRefresh<ControlledLoader>) -> FieldRefreshUpdate {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(update) = refresh.drain().into_iter().next() {
                return update;
            }
            assert!(Instant::now() < deadline, "refresh update should arrive");
            std::thread::yield_now();
        }
    }

    fn ticket(generation: u64) -> FieldRefreshTicket {
        FieldRefreshTicket::new(FieldGeneration::new(generation))
    }

    fn field(plot_id: &str) -> FieldSnapshot {
        serde_json::from_value(serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": [{ "kind": "nopal.plot/v1", "plot_id": plot_id }],
            "entries": [],
        }))
        .expect("fixture should satisfy the Field contract")
    }
}
