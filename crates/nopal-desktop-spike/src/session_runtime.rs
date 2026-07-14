use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver as CommandReceiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_channel::{Receiver, TryRecvError};
use nopal_feed_client::session::{
    SessionAgentState, SessionEvent, SessionModelDescriptor, SessionModelState,
};
use nopal_native_lifecycle::session_bindings::{
    SessionHostProcessIdentity, TerminalProcessIdentity,
};
use uuid::Uuid;

use crate::interaction::TerminalController;
use crate::model::{DesktopActivityKey, DesktopField, DesktopPlot, SelectedSessionContext};
use crate::session_client::ProductionFeedTransport;
use crate::session_feed::{
    FeedResumePoint, FeedState, FeedTransport, FeedUpdate, SessionFeed, SessionFeedContext,
};
use crate::source::{CommandRunner, TimedProcessRunner};
use crate::terminal::{TerminalSnapshot, TerminalSurface};
use crate::timeline::{ReplayState, SessionTimelineStore};
use crate::tmux::{PaneTransport, TmuxTransport};
use crate::workspace::{DesktopWorkspace, SelectionError};

pub trait TerminalSessionBinding {
    fn pane_id(&self) -> &str;
    fn process_identity(&self) -> TerminalProcessIdentity;
    fn poll_output(&mut self) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
    fn apply_output(&mut self, bytes: &[u8]);
    fn close(&mut self);
}

pub struct TerminalConnection<T> {
    pub binding: T,
    pub output: Receiver<Vec<u8>>,
}

pub enum ConnectorTask<T> {
    Ready(Result<T, String>),
    Pending(Receiver<Result<T, String>>),
}

pub trait RuntimeConnector {
    type FeedTransport: FeedTransport;
    type Terminal: TerminalSessionBinding;

    fn feed_transport(
        &self,
        context: &SelectedSessionContext,
    ) -> Result<Self::FeedTransport, String>;

    fn session_host_process(
        &self,
        context: &SelectedSessionContext,
    ) -> ConnectorTask<SessionHostProcessIdentity>;

    fn bind_terminal(
        &self,
        context: &SelectedSessionContext,
    ) -> ConnectorTask<TerminalConnection<Self::Terminal>>;
}

type LiveTerminalController = TerminalController<Box<dyn PaneTransport + Send + Sync>>;

const TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TERMINAL_COMMAND_TIMEOUT: Duration = Duration::from_millis(250);
const TERMINAL_COMMAND_CAPACITY: usize = 64;

#[derive(Default)]
struct PaneOwnership {
    epoch: u64,
}

#[derive(Clone)]
struct PaneOwnershipLease {
    owner: Arc<Mutex<PaneOwnership>>,
    epoch: u64,
}

impl PaneOwnershipLease {
    fn with_current<T>(&self, operation: impl FnOnce() -> T) -> Result<Option<T>, String> {
        let owner = self
            .owner
            .lock()
            .map_err(|_| "Terminal pane ownership is poisoned".to_owned())?;
        if owner.epoch != self.epoch {
            return Ok(None);
        }
        Ok(Some(operation()))
    }
}

static TERMINAL_PANE_OWNERS: OnceLock<Mutex<BTreeMap<String, Weak<Mutex<PaneOwnership>>>>> =
    OnceLock::new();
static NATIVE_BACKGROUND_TASKS: OnceLock<Mutex<Vec<JoinHandle<()>>>> = OnceLock::new();
static NATIVE_BACKGROUND_TASK_PANICKED: AtomicBool = AtomicBool::new(false);

fn terminal_pane_owner(pane_id: &str) -> Arc<Mutex<PaneOwnership>> {
    let registry = TERMINAL_PANE_OWNERS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut owners = lock_recovering(registry);
    owners.retain(|_, owner| owner.strong_count() > 0);
    if let Some(owner) = owners.get(pane_id).and_then(Weak::upgrade) {
        return owner;
    }
    let owner = Arc::new(Mutex::new(PaneOwnership::default()));
    owners.insert(pane_id.to_owned(), Arc::downgrade(&owner));
    owner
}

fn establish_terminal_ownership<T>(
    pane_id: &str,
    handshake: impl FnOnce() -> Result<T, String>,
) -> Result<(T, PaneOwnershipLease), String> {
    let owner = terminal_pane_owner(pane_id);
    let mut guard = owner
        .lock()
        .map_err(|_| "Terminal pane ownership is poisoned".to_owned())?;
    let established = handshake()?;
    let epoch = guard
        .epoch
        .checked_add(1)
        .ok_or_else(|| "Terminal pane ownership epoch is exhausted".to_owned())?;
    guard.epoch = epoch;
    drop(guard);
    Ok((established, PaneOwnershipLease { owner, epoch }))
}

fn lock_recovering<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

enum TerminalWorkerCommand {
    SendInput(Vec<u8>),
    Resize { columns: usize, rows: usize },
}

struct TerminalWorkerContext {
    pane_id: String,
    expected_process: TerminalProcessIdentity,
    last_capture: Vec<u8>,
    commands: CommandReceiver<TerminalWorkerCommand>,
    stop: Arc<AtomicBool>,
    original_size: (usize, usize),
    latest_capture: Arc<Mutex<Option<Vec<u8>>>>,
    failure: Arc<Mutex<Option<String>>>,
    ownership: PaneOwnershipLease,
}

#[derive(Clone)]
struct TerminalWorkerTransport {
    pane_id: String,
    commands: SyncSender<TerminalWorkerCommand>,
}

impl PaneTransport for TerminalWorkerTransport {
    fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String> {
        if pane_id != self.pane_id {
            return Err("Terminal input pane identity changed".to_owned());
        }
        self.commands
            .try_send(TerminalWorkerCommand::SendInput(bytes.to_vec()))
            .map_err(|error| format!("Terminal input worker is unavailable: {error}"))
    }

    fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String> {
        if pane_id != self.pane_id {
            return Err("Terminal resize pane identity changed".to_owned());
        }
        self.commands
            .try_send(TerminalWorkerCommand::Resize { columns, rows })
            .map_err(|error| format!("Terminal resize worker is unavailable: {error}"))
    }
}

struct TerminalWorker {
    commands: SyncSender<TerminalWorkerCommand>,
    latest_capture: Arc<Mutex<Option<Vec<u8>>>>,
    failure: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TerminalWorker {
    fn start(
        pane_id: String,
        expected_process: TerminalProcessIdentity,
        initial_capture: Vec<u8>,
        original_size: (usize, usize),
        ownership: PaneOwnershipLease,
    ) -> Self {
        let (commands, command_receiver) = std::sync::mpsc::sync_channel(TERMINAL_COMMAND_CAPACITY);
        let latest_capture = Arc::new(Mutex::new(None));
        let worker_capture = Arc::clone(&latest_capture);
        let failure = Arc::new(Mutex::new(None));
        let worker_failure = Arc::clone(&failure);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_pane = pane_id.clone();
        let thread = std::thread::spawn(move || {
            run_terminal_worker(TerminalWorkerContext {
                pane_id: worker_pane,
                expected_process,
                last_capture: initial_capture,
                commands: command_receiver,
                stop: worker_stop,
                original_size,
                latest_capture: worker_capture,
                failure: worker_failure,
                ownership,
            });
        });
        Self {
            commands,
            latest_capture,
            failure,
            stop,
            thread: Some(thread),
        }
    }

    fn transport(&self, pane_id: String) -> TerminalWorkerTransport {
        TerminalWorkerTransport {
            pane_id,
            commands: self.commands.clone(),
        }
    }

    fn poll(&self) -> Result<Option<Vec<u8>>, String> {
        if let Some(error) = self
            .failure
            .lock()
            .map_err(|_| "Terminal worker failure state is poisoned".to_owned())?
            .take()
        {
            return Err(error);
        }
        let capture = self
            .latest_capture
            .lock()
            .map_err(|_| "Terminal worker capture state is poisoned".to_owned())?
            .take();
        if capture.is_none() && self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return Err("Terminal worker stopped unexpectedly".to_owned());
        }
        Ok(capture)
    }

    fn close(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            reap_terminal_worker(thread);
        }
    }
}

fn reap_terminal_worker(thread: JoinHandle<()>) {
    track_native_background_task(thread);
}

fn track_native_background_task(thread: JoinHandle<()>) {
    reap_finished_native_background_tasks();
    lock_recovering(NATIVE_BACKGROUND_TASKS.get_or_init(|| Mutex::new(Vec::new()))).push(thread);
}

fn reap_finished_native_background_tasks() {
    let registry = NATIVE_BACKGROUND_TASKS.get_or_init(|| Mutex::new(Vec::new()));
    let mut handles = lock_recovering(registry);
    let mut index = 0;
    while index < handles.len() {
        if handles[index].is_finished() {
            let handle = handles.swap_remove(index);
            if handle.join().is_err() {
                NATIVE_BACKGROUND_TASK_PANICKED.store(true, Ordering::Release);
            }
        } else {
            index += 1;
        }
    }
}

pub(crate) fn drain_native_background_tasks() -> Result<(), String> {
    let registry = NATIVE_BACKGROUND_TASKS.get_or_init(|| Mutex::new(Vec::new()));
    let mut panicked = false;
    loop {
        let handles = std::mem::take(&mut *lock_recovering(registry));
        if handles.is_empty() {
            break;
        }
        for handle in handles {
            if handle.join().is_err() {
                panicked = true;
            }
        }
    }
    if panicked || NATIVE_BACKGROUND_TASK_PANICKED.swap(false, Ordering::AcqRel) {
        Err("native background worker panicked during shutdown".to_owned())
    } else {
        Ok(())
    }
}

trait TerminalWorkerBackend {
    fn pane_process_id(&self, pane_id: &str) -> Result<u32, String>;
    fn capture(&self, pane_id: &str) -> Result<Vec<u8>, String>;
    fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String>;
    fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String>;
}

impl<R> TerminalWorkerBackend for TmuxTransport<R>
where
    R: CommandRunner,
{
    fn pane_process_id(&self, pane_id: &str) -> Result<u32, String> {
        TmuxTransport::pane_process_id(self, pane_id)
    }

    fn capture(&self, pane_id: &str) -> Result<Vec<u8>, String> {
        TmuxTransport::capture(self, pane_id)
    }

    fn send_input(&self, pane_id: &str, bytes: &[u8]) -> Result<(), String> {
        TmuxTransport::send_input(self, pane_id, bytes)
    }

    fn resize_pane(&self, pane_id: &str, columns: usize, rows: usize) -> Result<(), String> {
        TmuxTransport::resize_pane(self, pane_id, columns, rows)
    }
}

fn run_terminal_worker(context: TerminalWorkerContext) {
    let transport = TmuxTransport::production(TimedProcessRunner::new(TERMINAL_COMMAND_TIMEOUT));
    run_terminal_worker_with_transport(context, &transport);
}

fn run_terminal_worker_with_transport(
    context: TerminalWorkerContext,
    transport: &impl TerminalWorkerBackend,
) {
    let TerminalWorkerContext {
        pane_id,
        expected_process,
        mut last_capture,
        commands,
        stop,
        original_size,
        latest_capture,
        failure,
        ownership,
    } = context;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let action = match commands.recv_timeout(TERMINAL_POLL_INTERVAL) {
            Ok(action) => Some(action),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        let result = match ownership.with_current(|| {
            let current_process = transport.pane_process_id(&pane_id)?;
            if current_process != expected_process.get() {
                return Err(format!(
                    "Terminal process identity changed: expected {}, found {current_process}",
                    expected_process.get()
                ));
            }
            match action {
                Some(TerminalWorkerCommand::SendInput(bytes)) => {
                    transport.send_input(&pane_id, &bytes).map(|()| None)
                }
                Some(TerminalWorkerCommand::Resize { columns, rows }) => transport
                    .resize_pane(&pane_id, columns, rows)
                    .map(|()| None),
                None => transport.capture(&pane_id).map(|capture| {
                    (capture != last_capture).then(|| {
                        last_capture.clone_from(&capture);
                        capture
                    })
                }),
            }
        }) {
            Ok(Some(result)) => result,
            Ok(None) => break,
            Err(error) => Err(error),
        };
        match result {
            Ok(Some(capture)) => {
                if let Ok(mut latest) = latest_capture.lock() {
                    *latest = Some(capture);
                } else {
                    record_terminal_failure(
                        &failure,
                        "Terminal worker capture state is poisoned".to_owned(),
                    );
                    break;
                }
            }
            Ok(None) => {}
            Err(error) => {
                record_terminal_failure(&failure, error);
                break;
            }
        }
    }
    let _ = ownership.with_current(|| {
        restore_terminal_size(transport, &pane_id, expected_process, original_size);
    });
}

fn record_terminal_failure(failure: &Arc<Mutex<Option<String>>>, error: String) {
    if let Ok(mut slot) = failure.lock() {
        *slot = Some(error);
    }
}

fn restore_terminal_size(
    transport: &impl TerminalWorkerBackend,
    pane_id: &str,
    expected_process: TerminalProcessIdentity,
    original_size: (usize, usize),
) {
    if transport.pane_process_id(pane_id).ok() == Some(expected_process.get()) {
        let _ = transport.resize_pane(pane_id, original_size.0, original_size.1);
    }
}

pub struct LiveTerminalBinding {
    pane_id: String,
    process_identity: TerminalProcessIdentity,
    controller: Option<LiveTerminalController>,
    worker: Option<TerminalWorker>,
}

impl LiveTerminalBinding {
    pub fn controller(&self) -> Option<&LiveTerminalController> {
        self.controller.as_ref()
    }

    pub fn controller_mut(&mut self) -> Option<&mut LiveTerminalController> {
        self.controller.as_mut()
    }

    pub fn snapshot(&self) -> Option<TerminalSnapshot> {
        self.controller.as_ref().map(TerminalController::snapshot)
    }
}

impl TerminalSessionBinding for LiveTerminalBinding {
    fn pane_id(&self) -> &str {
        &self.pane_id
    }

    fn process_identity(&self) -> TerminalProcessIdentity {
        self.process_identity
    }

    fn poll_output(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.worker.as_ref().map_or(Ok(None), TerminalWorker::poll)
    }

    fn apply_output(&mut self, bytes: &[u8]) {
        if let Some(controller) = self.controller.as_mut() {
            controller.apply_output(bytes);
        }
    }

    fn close(&mut self) {
        self.controller.take();
        if let Some(mut worker) = self.worker.take() {
            worker.close();
        }
    }
}

impl Drop for LiveTerminalBinding {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProductionRuntimeConnector;

impl RuntimeConnector for ProductionRuntimeConnector {
    type FeedTransport = ProductionFeedTransport;
    type Terminal = LiveTerminalBinding;

    fn feed_transport(
        &self,
        _context: &SelectedSessionContext,
    ) -> Result<Self::FeedTransport, String> {
        Ok(ProductionFeedTransport)
    }

    fn session_host_process(
        &self,
        context: &SelectedSessionContext,
    ) -> ConnectorTask<SessionHostProcessIdentity> {
        let context = context.clone();
        spawn_connector_task("nopal-terminal-host", move || {
            resolve_production_host_process(&context)
        })
    }

    fn bind_terminal(
        &self,
        context: &SelectedSessionContext,
    ) -> ConnectorTask<TerminalConnection<Self::Terminal>> {
        let context = context.clone();
        spawn_connector_task("nopal-terminal-bind", move || {
            bind_production_terminal(&context)
        })
    }
}

fn spawn_connector_task<T>(
    name: &'static str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> ConnectorTask<T>
where
    T: Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    match std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _ = sender.send_blocking(work());
        }) {
        Ok(thread) => {
            track_native_background_task(thread);
            ConnectorTask::Pending(receiver)
        }
        Err(error) => ConnectorTask::Ready(Err(format!("cannot start {name} worker: {error}"))),
    }
}

fn resolve_production_host_process(
    context: &SelectedSessionContext,
) -> Result<SessionHostProcessIdentity, String> {
    let pane_id = context
        .host_pane
        .as_ref()
        .ok_or_else(|| "selected Session has no live terminal pane".to_owned())?;
    let process_id = TmuxTransport::production(TimedProcessRunner::new(TERMINAL_COMMAND_TIMEOUT))
        .pane_process_id(pane_id)?;
    SessionHostProcessIdentity::new(process_id)
        .map_err(|error| format!("invalid Session host process identity: {error}"))
}

fn bind_production_terminal(
    context: &SelectedSessionContext,
) -> Result<TerminalConnection<LiveTerminalBinding>, String> {
    let pane_id = context
        .host_pane
        .as_ref()
        .ok_or_else(|| "selected Session has no live terminal pane".to_owned())?;
    let transport = TmuxTransport::production(TimedProcessRunner::new(TERMINAL_COMMAND_TIMEOUT));
    let (handshake, ownership) =
        establish_terminal_ownership(pane_id, || capture_terminal_handshake(&transport, pane_id))?;
    let process_identity = TerminalProcessIdentity::new(handshake.process_id)
        .map_err(|error| format!("invalid Terminal process identity: {error}"))?;
    let (columns, rows) = handshake.size;
    let bytes = handshake.capture;
    let mut terminal = TerminalSurface::new(columns, rows);
    terminal.apply_output(&bytes);
    let worker = TerminalWorker::start(
        pane_id.clone(),
        process_identity,
        bytes.clone(),
        (columns, rows),
        ownership,
    );
    let controller = TerminalController::new(
        pane_id.clone(),
        terminal,
        Box::new(worker.transport(pane_id.clone())) as Box<dyn PaneTransport + Send + Sync>,
        (columns, rows),
    );
    let (_sender, output) = async_channel::unbounded();
    Ok(TerminalConnection {
        binding: LiveTerminalBinding {
            pane_id: pane_id.clone(),
            process_identity,
            controller: Some(controller),
            worker: Some(worker),
        },
        output,
    })
}

struct TerminalHandshake {
    process_id: u32,
    size: (usize, usize),
    capture: Vec<u8>,
}

fn capture_terminal_handshake<R>(
    transport: &TmuxTransport<R>,
    pane_id: &str,
) -> Result<TerminalHandshake, String>
where
    R: CommandRunner,
{
    let process_id = transport.pane_process_id(pane_id)?;
    let size = transport.pane_size(pane_id)?;
    let capture = transport.capture(pane_id)?;
    let confirmed_process = transport.pane_process_id(pane_id)?;
    if confirmed_process != process_id {
        return Err(format!(
            "Terminal process identity changed during attachment: expected {process_id}, found {confirmed_process}"
        ));
    }
    Ok(TerminalHandshake {
        process_id,
        size,
        capture,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RuntimePresentation {
    #[default]
    Output,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    Ready,
    StructuredOnly { terminal_error: String },
    TerminalOnly { structured_error: String },
    ExecutionSelected,
    Unavailable { reason: String },
    Degraded { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetargetOutcome {
    pub generation: u64,
    pub changed: bool,
    pub context: Option<SelectedSessionContext>,
    pub status: RuntimeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionTarget {
    pub plot_id: String,
    pub activity: Option<DesktopActivityKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    Selection(SelectionError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitOutcome {
    Sent { command_id: String },
    RestoreText { text: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainOutcome {
    pub generation: u64,
    pub events_applied: usize,
    pub terminal_chunks_applied: usize,
    pub visible_changed: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SessionKey {
    plot_id: String,
    session_id: String,
}

impl From<&SelectedSessionContext> for SessionKey {
    fn from(context: &SelectedSessionContext) -> Self {
        Self {
            plot_id: context.plot_id.clone(),
            session_id: context.session_id.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct PendingPrompt {
    command_id: String,
    text: String,
    needs_send: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingModelSwitch {
    request_id: String,
    provider: String,
    model_id: String,
}

type ConnectorReceiver<T> = Receiver<Result<T, String>>;
type GenerationTask<T> = (u64, ConnectorReceiver<T>);

pub struct SessionRuntime<C>
where
    C: RuntimeConnector,
{
    connector: C,
    workspace: DesktopWorkspace,
    timelines: SessionTimelineStore,
    feed: Option<SessionFeed<C::FeedTransport>>,
    terminal: Option<C::Terminal>,
    terminal_output: Option<Receiver<Vec<u8>>>,
    session_host_process: Option<SessionHostProcessIdentity>,
    host_process_task: Option<GenerationTask<SessionHostProcessIdentity>>,
    terminal_task: Option<GenerationTask<TerminalConnection<C::Terminal>>>,
    terminal_error: Option<String>,
    forced_terminal: bool,
    pending_prompts: BTreeMap<SessionKey, PendingPrompt>,
    model_state: Option<SessionModelState>,
    pending_model_switch: Option<PendingModelSwitch>,
    model_error: Option<String>,
    confirmed_model_switch: Option<SessionModelDescriptor>,
    rejected_generation: Option<u64>,
    generation: u64,
    presentation: RuntimePresentation,
    status: RuntimeStatus,
    started_at: Instant,
}

pub type LiveSessionRuntime = SessionRuntime<ProductionRuntimeConnector>;

impl<C> SessionRuntime<C>
where
    C: RuntimeConnector,
{
    pub fn new(field: DesktopField, connector: C) -> Self {
        let mut runtime = Self {
            connector,
            workspace: DesktopWorkspace::new(field),
            timelines: SessionTimelineStore::default(),
            feed: None,
            terminal: None,
            terminal_output: None,
            session_host_process: None,
            host_process_task: None,
            terminal_task: None,
            terminal_error: None,
            forced_terminal: false,
            pending_prompts: BTreeMap::new(),
            model_state: None,
            pending_model_switch: None,
            model_error: None,
            confirmed_model_switch: None,
            rejected_generation: None,
            generation: 0,
            presentation: RuntimePresentation::default(),
            status: RuntimeStatus::Unavailable {
                reason: "Session runtime has not been bound".to_owned(),
            },
            started_at: Instant::now(),
        };
        runtime.bind_selected();
        runtime
    }

    pub fn prepare(field: DesktopField, connector: C) -> Result<Self, String> {
        let mut runtime = Self::new(field, connector);
        runtime.drain();
        if matches!(runtime.status, RuntimeStatus::Unavailable { .. }) {
            Err(runtime.status_detail("Session bindings are unavailable"))
        } else {
            Ok(runtime)
        }
    }

    pub fn field(&self) -> &DesktopField {
        self.workspace.field()
    }

    pub fn selected_plot(&self) -> Option<&DesktopPlot> {
        self.workspace.selected_plot()
    }

    pub fn selected_activity(&self) -> Option<&DesktopActivityKey> {
        self.workspace.selected_activity()
    }

    pub fn selected_session_context(&self) -> Option<SelectedSessionContext> {
        self.workspace.selected_session_context()
    }

    pub fn current_events(&self) -> &[SessionEvent] {
        self.timelines.current_events()
    }

    pub fn composer_draft(&self) -> &str {
        self.timelines.current_draft()
    }

    pub fn set_composer_draft(&mut self, draft: impl Into<String>) {
        self.timelines.set_current_draft(draft);
    }

    pub fn terminal_binding(&self) -> Option<&C::Terminal> {
        self.terminal.as_ref()
    }

    pub fn terminal_binding_mut(&mut self) -> Option<&mut C::Terminal> {
        self.terminal.as_mut()
    }

    pub fn status(&self) -> &RuntimeStatus {
        &self.status
    }

    pub fn feed_state(&self) -> Option<&FeedState> {
        self.feed.as_ref().map(SessionFeed::state)
    }

    pub fn replay_state(&self) -> ReplayState {
        self.timelines.current_replay_state()
    }

    pub fn can_submit(&self) -> bool {
        let Some(context) = self.selected_session_context() else {
            return false;
        };
        self.feed.as_ref().is_some_and(SessionFeed::can_submit)
            && matches!(self.replay_state(), ReplayState::Live)
            && !self
                .pending_prompts
                .contains_key(&SessionKey::from(&context))
    }

    pub fn model_state(&self) -> Option<&SessionModelState> {
        self.model_state.as_ref()
    }

    pub fn model_error(&self) -> Option<&str> {
        self.model_error.as_deref()
    }

    pub fn model_switch_pending(&self) -> bool {
        self.pending_model_switch.is_some()
    }

    pub fn take_confirmed_model_switch(&mut self) -> Option<SessionModelDescriptor> {
        self.confirmed_model_switch.take()
    }

    pub fn can_switch_model(&self) -> bool {
        self.feed.as_ref().is_some_and(SessionFeed::can_submit)
            && self.pending_model_switch.is_none()
            && self.model_state.as_ref().is_some_and(|state| {
                state.agent_state == SessionAgentState::Idle && state.available.len() > 1
            })
    }

    pub fn refresh_models(&mut self) -> Result<(), String> {
        let request_id = format!("model-refresh-desktop-{}", Uuid::new_v4());
        let now_ms = self.now_ms();
        self.feed
            .as_mut()
            .ok_or_else(|| "Session model control is unavailable".to_owned())?
            .request_models(request_id, now_ms)
            .map_err(|error| error.message)
    }

    pub fn switch_model(&mut self, model: &SessionModelDescriptor) -> Result<String, String> {
        if !self.can_switch_model() {
            return Err("Session model control is not ready".to_owned());
        }
        let is_available = self.model_state.as_ref().is_some_and(|state| {
            state
                .available
                .iter()
                .any(|choice| choice.provider == model.provider && choice.id == model.id)
        });
        if !is_available {
            return Err("Pi did not report that model as available".to_owned());
        }
        let request_id = format!("model-switch-desktop-{}", Uuid::new_v4());
        self.pending_model_switch = Some(PendingModelSwitch {
            request_id: request_id.clone(),
            provider: model.provider.clone(),
            model_id: model.id.clone(),
        });
        self.model_error = None;
        let now_ms = self.now_ms();
        let result = self
            .feed
            .as_mut()
            .ok_or_else(|| "Session model control is unavailable".to_owned())?
            .switch_model(&request_id, &model.provider, &model.id, now_ms)
            .map_err(|error| error.message);
        if result.is_err() {
            self.pending_model_switch = None;
        }
        result.map(|()| request_id)
    }

    pub fn retry_now(&mut self) -> bool {
        let retried = self.feed.as_mut().is_some_and(SessionFeed::retry_now);
        if retried
            && let Some(context) = self.selected_session_context()
            && let Some(pending) = self.pending_prompts.get_mut(&SessionKey::from(&context))
        {
            pending.needs_send = true;
        }
        retried || self.rebuild_structured_feed()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn presentation(&self) -> RuntimePresentation {
        self.presentation
    }

    pub fn set_presentation(&mut self, presentation: RuntimePresentation) {
        match presentation {
            RuntimePresentation::Output => {
                self.presentation = RuntimePresentation::Output;
                self.forced_terminal = false;
            }
            RuntimePresentation::Terminal => {
                self.forced_terminal = false;
                match self.ensure_terminal() {
                    Ok(_) => self.presentation = RuntimePresentation::Terminal,
                    Err(error) => {
                        self.presentation = RuntimePresentation::Output;
                        self.terminal_error = Some(error);
                    }
                }
            }
        }
        self.refresh_status();
    }

    pub fn select_plot(&mut self, plot_id: &str) -> Result<RetargetOutcome, RuntimeError> {
        self.retarget(SelectionTarget {
            plot_id: plot_id.to_owned(),
            activity: None,
        })
    }

    pub fn select_activity(
        &mut self,
        activity: DesktopActivityKey,
    ) -> Result<RetargetOutcome, RuntimeError> {
        let plot_id = self
            .field()
            .selected_plot_id
            .clone()
            .ok_or(RuntimeError::Selection(SelectionError::UnknownPlot))?;
        self.retarget(SelectionTarget {
            plot_id,
            activity: Some(activity),
        })
    }

    pub fn retarget(&mut self, target: SelectionTarget) -> Result<RetargetOutcome, RuntimeError> {
        let Some(target_plot) = self
            .field()
            .plots
            .iter()
            .find(|plot| plot.plot_id == target.plot_id)
        else {
            return Err(RuntimeError::Selection(SelectionError::UnknownPlot));
        };
        if let Some(activity) = &target.activity
            && !target_plot.activity_keys().contains(activity)
        {
            return Err(RuntimeError::Selection(SelectionError::UnknownActivity));
        }
        let same_plot = self.field().selected_plot_id.as_deref() == Some(&target.plot_id);
        if same_plot
            && target
                .activity
                .as_ref()
                .is_none_or(|activity| self.selected_activity() == Some(activity))
        {
            return Ok(self.outcome(false));
        }

        self.close_bindings();
        self.workspace
            .select_plot(&target.plot_id)
            .map_err(RuntimeError::Selection)?;
        if let Some(activity) = target.activity {
            self.workspace
                .select_activity(activity)
                .map_err(RuntimeError::Selection)?;
        }
        self.bind_selected();
        Ok(self.outcome(true))
    }

    pub fn submit_prompt(&mut self, text: &str) -> SubmitOutcome {
        let Some(context) = self.selected_session_context() else {
            return self.restore(text, "select a Session before sending");
        };
        if text.trim().is_empty() {
            return self.restore(text, "instruction is empty");
        }
        if !self.feed.as_ref().is_some_and(SessionFeed::can_submit) {
            return self.restore(
                text,
                &self.status_detail("structured Session feed is not live"),
            );
        }
        let key = SessionKey::from(&context);
        if let Some(pending) = self.pending_prompts.get(&key) {
            if pending.text == text {
                return SubmitOutcome::Sent {
                    command_id: pending.command_id.clone(),
                };
            }
            return self.restore(text, "wait for the previous instruction to be acknowledged");
        }
        let command_id = format!("command-desktop-{}", Uuid::new_v4());
        self.pending_prompts.insert(
            key.clone(),
            PendingPrompt {
                command_id: command_id.clone(),
                text: text.to_owned(),
                needs_send: true,
            },
        );
        match self.send_pending(&key, self.now_ms()) {
            Ok(()) => SubmitOutcome::Sent { command_id },
            Err(reason) => self.restore(text, &reason),
        }
    }

    pub fn take_terminal_output_receiver(&mut self) -> Option<(u64, Receiver<Vec<u8>>)> {
        self.terminal_output
            .take()
            .map(|receiver| (self.generation, receiver))
    }

    pub fn apply_feed_update(&mut self, generation: u64, update: FeedUpdate) -> bool {
        if generation != self.generation
            || update_generation(&update) != self.generation
            || self.rejected_generation == Some(generation)
        {
            return false;
        }
        match update {
            FeedUpdate::Event { event, .. } => {
                let replaying = matches!(
                    self.timelines.current_replay_state(),
                    ReplayState::Restoring { .. }
                );
                let acknowledged = event.user_message_command_id().map(str::to_owned);
                match self.timelines.ingest_durable(*event) {
                    Ok(_) => {
                        if !replaying && let Some(command_id) = acknowledged {
                            self.clear_acknowledged(&command_id);
                        }
                        true
                    }
                    Err(error) => {
                        self.fail_contract(format!(
                            "durable Session timeline rejected event: {error:?}"
                        ));
                        false
                    }
                }
            }
            FeedUpdate::ReplayComplete { complete, .. } => {
                match self.timelines.complete_durable_replay(&complete) {
                    Ok(()) => {
                        self.clear_pending_if_durable();
                        self.refresh_status();
                        true
                    }
                    Err(error) => {
                        self.fail_contract(format!("durable Session replay rejected: {error:?}"));
                        false
                    }
                }
            }
            FeedUpdate::State { state, .. } => {
                if matches!(state, FeedState::Restoring { .. } | FeedState::Live) {
                    self.refresh_session_host_process();
                }
                match &state {
                    FeedState::Restoring { after_cursor, .. } => {
                        self.model_state = None;
                        if self.pending_model_switch.take().is_some() {
                            self.model_error = Some(
                                "The Session reconnected before Pi confirmed the model switch"
                                    .to_owned(),
                            );
                        }
                        if let Err(error) = self.timelines.begin_replay(after_cursor.as_deref()) {
                            self.fail_contract(format!(
                                "cannot stage durable Session replay: {error:?}"
                            ));
                            return false;
                        }
                    }
                    FeedState::Backoff {
                        attempt, reason, ..
                    } => {
                        self.model_state = None;
                        if self.pending_model_switch.take().is_some() {
                            self.model_error = Some(
                                "The connection closed before Pi confirmed the model switch"
                                    .to_owned(),
                            );
                        }
                        self.timelines.mark_reconnecting(*attempt, reason);
                        self.mark_pending_for_retry();
                        self.force_terminal_fallback();
                    }
                    FeedState::Fatal { code, message } => {
                        self.model_state = None;
                        self.pending_model_switch = None;
                        self.timelines.fail_feed(code, message);
                        self.resolve_fatal_pending(code);
                        self.force_terminal_fallback();
                    }
                    _ => {}
                }
                self.apply_feed_status(Some(&state));
                true
            }
            FeedUpdate::Error {
                code,
                message,
                retryable,
                ..
            } => {
                self.model_state = None;
                if self.pending_model_switch.take().is_some() {
                    self.model_error = Some(
                        "The Session feed failed before Pi confirmed the model switch".to_owned(),
                    );
                }
                if retryable {
                    self.mark_pending_for_retry();
                } else {
                    self.timelines.fail_feed(&code, &message);
                    self.resolve_fatal_pending(&code);
                    self.rejected_generation = Some(generation);
                }
                self.force_terminal_fallback();
                self.status = self.terminal_fallback_status(message);
                true
            }
            FeedUpdate::ModelState { state, .. } => {
                if let Some(previous) = &self.model_state
                    && previous.state_epoch == state.state_epoch
                    && previous.revision > state.revision
                {
                    return false;
                }
                self.model_error = None;
                if let Some(pending) = &self.pending_model_switch
                    && state.request_id.as_deref() == Some(&pending.request_id)
                {
                    let confirmed = state.current.as_ref().is_some_and(|current| {
                        current.provider == pending.provider && current.id == pending.model_id
                    });
                    if confirmed {
                        self.confirmed_model_switch = state.current.clone();
                        self.pending_model_switch = None;
                    } else {
                        self.model_error = Some(
                            "Pi acknowledged the model request without selecting its target"
                                .to_owned(),
                        );
                        self.pending_model_switch = None;
                    }
                }
                self.model_state = Some(state);
                true
            }
            FeedUpdate::ModelError { error, .. } => {
                if self
                    .pending_model_switch
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == error.request_id)
                {
                    self.pending_model_switch = None;
                }
                self.model_error = Some(error.message);
                true
            }
        }
    }

    pub fn apply_terminal_output(&mut self, generation: u64, bytes: &[u8]) -> bool {
        if generation != self.generation {
            return false;
        }
        let Some(terminal) = self.terminal.as_mut() else {
            return false;
        };
        terminal.apply_output(bytes);
        true
    }

    pub fn drain(&mut self) -> DrainOutcome {
        self.drain_at(self.now_ms())
    }

    pub fn drain_at(&mut self, now_ms: u64) -> DrainOutcome {
        reap_finished_native_background_tasks();
        let generation = self.generation;
        let mut outcome = DrainOutcome {
            generation,
            events_applied: 0,
            terminal_chunks_applied: 0,
            visible_changed: false,
            errors: Vec::new(),
        };
        self.poll_connector_tasks(&mut outcome);
        if let Some(feed) = self.feed.as_mut() {
            feed.poll(now_ms);
        }
        let updates = self
            .feed
            .as_mut()
            .map(SessionFeed::take_updates)
            .unwrap_or_default();
        for update in updates {
            let is_event = matches!(update, FeedUpdate::Event { .. });
            if let FeedUpdate::Error { message, .. } = &update {
                outcome.errors.push(message.clone());
            }
            if self.apply_feed_update(generation, update) {
                outcome.visible_changed = true;
                if is_event {
                    outcome.events_applied += 1;
                }
            }
        }
        if self.feed.as_ref().is_some_and(SessionFeed::can_submit) {
            let key = self
                .selected_session_context()
                .as_ref()
                .map(SessionKey::from);
            if let Some(key) = key
                && self
                    .pending_prompts
                    .get(&key)
                    .is_some_and(|pending| pending.needs_send)
                && let Err(error) = self.send_pending(&key, now_ms)
            {
                outcome.errors.push(error);
            }
        }
        loop {
            let next = self
                .terminal_output
                .as_ref()
                .map(Receiver::try_recv)
                .unwrap_or(Err(TryRecvError::Closed));
            match next {
                Ok(bytes) => {
                    if self.apply_terminal_output(generation, &bytes) {
                        outcome.terminal_chunks_applied += 1;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        let polled_terminal = self
            .terminal
            .as_mut()
            .map(TerminalSessionBinding::poll_output);
        match polled_terminal {
            Some(Ok(Some(bytes))) => {
                if self.apply_terminal_output(generation, &bytes) {
                    outcome.terminal_chunks_applied += 1;
                }
            }
            Some(Err(error)) => {
                outcome.errors.push(error.clone());
                self.invalidate_terminal(error);
            }
            Some(Ok(None)) | None => {}
        }
        outcome
    }

    fn bind_selected(&mut self) {
        let context = self.selected_session_context();
        self.model_state = None;
        self.pending_model_switch = None;
        self.model_error = None;
        self.confirmed_model_switch = None;
        self.timelines.select_session(context.as_ref());
        self.session_host_process = None;
        self.host_process_task = None;
        self.terminal_task = None;
        self.terminal_error = None;
        let Some(context) = context else {
            self.generation = self.generation.saturating_add(1);
            self.status = if matches!(
                self.selected_activity(),
                Some(DesktopActivityKey::Execution { .. })
            ) {
                RuntimeStatus::ExecutionSelected
            } else {
                RuntimeStatus::Unavailable {
                    reason: "selected activity is not a Session".to_owned(),
                }
            };
            return;
        };

        let generation = self.generation.saturating_add(1);
        self.generation = generation;
        self.rejected_generation = None;
        self.begin_host_process_lookup(&context);
        let resume = FeedResumePoint {
            stream_id: self.timelines.current_stream_id().map(str::to_owned),
            sequence: self.timelines.current_sequence().unwrap_or(0),
            cursor: self.timelines.current_cursor().map(str::to_owned),
        };
        if let Err(error) = self.timelines.begin_replay(resume.cursor.as_deref()) {
            self.force_terminal_fallback();
            self.status = self.terminal_fallback_status(format!(
                "cannot restore verified Session timeline: {error:?}"
            ));
            return;
        }

        let feed_error = match self.build_feed(generation, &context, resume) {
            Ok(feed) => {
                self.feed = Some(feed);
                None
            }
            Err(error) => Some(error),
        };
        match feed_error {
            None => {
                if self.presentation == RuntimePresentation::Terminal
                    && let Err(error) = self.ensure_terminal()
                {
                    self.presentation = RuntimePresentation::Output;
                    self.terminal_error = Some(error);
                }
                self.status = RuntimeStatus::Degraded {
                    detail: "restoring durable Session history".to_owned(),
                };
            }
            Some(structured_error) => {
                self.force_terminal_fallback();
                self.status = self.terminal_fallback_status(structured_error);
            }
        }
    }

    fn ensure_terminal(&mut self) -> Result<bool, String> {
        let context = self
            .selected_session_context()
            .ok_or_else(|| "selected activity is not a Session".to_owned())?;
        let expected = context
            .host_pane
            .as_deref()
            .ok_or_else(|| "selected Session has no live terminal pane".to_owned())?;
        let Some(expected_process) = self.session_host_process else {
            if self.host_process_task.is_none() {
                self.begin_host_process_lookup(&context);
            }
            if self.host_process_task.is_some() {
                return Ok(false);
            }
            return Err(self
                .terminal_error
                .clone()
                .unwrap_or_else(|| "Session host process identity is unavailable".to_owned()));
        };
        if let Some(binding) = self.terminal.as_ref() {
            if binding.pane_id() == expected
                && binding.process_identity().get() == expected_process.get()
            {
                self.terminal_error = None;
                return Ok(true);
            }
            self.invalidate_terminal("Terminal binding identity became stale".to_owned());
        }
        if self.terminal_task.is_some() {
            return Ok(false);
        }
        match self.connector.bind_terminal(&context) {
            ConnectorTask::Ready(result) => {
                self.adopt_terminal(result?)?;
                Ok(true)
            }
            ConnectorTask::Pending(receiver) => {
                self.terminal_task = Some((self.generation, receiver));
                self.terminal_error = None;
                Ok(false)
            }
        }
    }

    fn adopt_terminal(
        &mut self,
        mut connection: TerminalConnection<C::Terminal>,
    ) -> Result<(), String> {
        let context = self
            .selected_session_context()
            .ok_or_else(|| "selected activity is not a Session".to_owned())?;
        let expected = context
            .host_pane
            .as_deref()
            .ok_or_else(|| "selected Session has no live terminal pane".to_owned())?;
        let expected_process = self.session_host_process.ok_or_else(|| {
            "Session host process identity changed while Terminal was attaching".to_owned()
        })?;
        if connection.binding.pane_id() != expected {
            let actual = connection.binding.pane_id().to_owned();
            connection.binding.close();
            return Err(format!(
                "terminal pane mismatch: expected {expected}, bound {actual}"
            ));
        }
        if connection.binding.process_identity().get() != expected_process.get() {
            let actual = connection.binding.process_identity().get();
            connection.binding.close();
            return Err(format!(
                "terminal process mismatch: expected existing Session host {}, bound {actual}",
                expected_process.get()
            ));
        }
        self.terminal = Some(connection.binding);
        self.terminal_output = Some(connection.output);
        self.terminal_error = None;
        Ok(())
    }

    fn refresh_session_host_process(&mut self) {
        let Some(context) = self.selected_session_context() else {
            return;
        };
        self.begin_host_process_lookup(&context);
    }

    fn begin_host_process_lookup(&mut self, context: &SelectedSessionContext) {
        self.host_process_task = None;
        match self.connector.session_host_process(context) {
            ConnectorTask::Ready(result) => self.apply_host_process_result(result),
            ConnectorTask::Pending(receiver) => {
                self.host_process_task = Some((self.generation, receiver));
            }
        }
    }

    fn apply_host_process_result(&mut self, result: Result<SessionHostProcessIdentity, String>) {
        match result {
            Ok(process) => {
                let stale_terminal = self
                    .terminal
                    .as_ref()
                    .is_some_and(|binding| binding.process_identity().get() != process.get());
                self.session_host_process = Some(process);
                if stale_terminal {
                    self.invalidate_terminal(
                        "Session host process changed; Terminal must attach again".to_owned(),
                    );
                } else {
                    self.terminal_error = None;
                }
            }
            Err(error) => {
                if self.terminal.is_some() {
                    self.invalidate_terminal(error);
                } else {
                    self.session_host_process = None;
                    self.terminal_error = Some(error);
                }
            }
        }
    }

    fn poll_connector_tasks(&mut self, outcome: &mut DrainOutcome) {
        let host_result = self
            .host_process_task
            .as_ref()
            .map(|(generation, receiver)| (*generation, receiver.try_recv()));
        match host_result {
            Some((generation, Ok(result))) => {
                self.host_process_task = None;
                if generation == self.generation {
                    if let Err(error) = &result {
                        outcome.errors.push(error.clone());
                    }
                    self.apply_host_process_result(result);
                    if self.presentation == RuntimePresentation::Terminal
                        && let Err(error) = self.ensure_terminal()
                    {
                        outcome.errors.push(error.clone());
                        self.terminal_error = Some(error);
                    }
                } else {
                    self.reconcile_terminal_intent(outcome);
                }
            }
            Some((generation, Err(TryRecvError::Closed))) => {
                self.host_process_task = None;
                if generation == self.generation {
                    let error = "Session host identity worker stopped unexpectedly".to_owned();
                    outcome.errors.push(error.clone());
                    self.apply_host_process_result(Err(error));
                }
            }
            Some((_, Err(TryRecvError::Empty))) | None => {}
        }

        let terminal_result = self
            .terminal_task
            .as_ref()
            .map(|(generation, receiver)| (*generation, receiver.try_recv()));
        match terminal_result {
            Some((generation, Ok(result))) => {
                self.terminal_task = None;
                if generation == self.generation {
                    match result.and_then(|connection| self.adopt_terminal(connection)) {
                        Ok(()) => self.refresh_status(),
                        Err(error) => {
                            outcome.errors.push(error.clone());
                            self.terminal_error = Some(error);
                            self.refresh_status();
                        }
                    }
                } else {
                    self.reconcile_terminal_intent(outcome);
                }
            }
            Some((generation, Err(TryRecvError::Closed))) => {
                self.terminal_task = None;
                if generation == self.generation {
                    let error = "Terminal attachment worker stopped unexpectedly".to_owned();
                    outcome.errors.push(error.clone());
                    self.terminal_error = Some(error);
                    self.refresh_status();
                }
            }
            Some((_, Err(TryRecvError::Empty))) | None => {}
        }
    }

    fn reconcile_terminal_intent(&mut self, outcome: &mut DrainOutcome) {
        if self.presentation == RuntimePresentation::Terminal
            && self.terminal.is_none()
            && let Err(error) = self.ensure_terminal()
        {
            outcome.errors.push(error.clone());
            self.terminal_error = Some(error);
            self.refresh_status();
        }
    }

    fn invalidate_terminal(&mut self, error: String) {
        if let Some(mut terminal) = self.terminal.take() {
            terminal.close();
        }
        self.terminal_output.take();
        self.terminal_error = Some(error);
        if self.presentation == RuntimePresentation::Terminal {
            self.presentation = RuntimePresentation::Output;
        }
        self.refresh_status();
    }

    fn force_terminal_fallback(&mut self) {
        if self.presentation != RuntimePresentation::Terminal {
            self.forced_terminal = true;
        }
        match self.ensure_terminal() {
            Ok(_) => self.presentation = RuntimePresentation::Terminal,
            Err(error) => self.terminal_error = Some(error),
        }
    }

    fn build_feed(
        &self,
        generation: u64,
        context: &SelectedSessionContext,
        resume: FeedResumePoint,
    ) -> Result<SessionFeed<C::FeedTransport>, String> {
        let protocol = context
            .protocol
            .as_ref()
            .ok_or_else(|| "structured Session protocol is unavailable".to_owned())?;
        if protocol.transport != "unix" {
            return Err(format!(
                "unsupported Session transport {:?}",
                protocol.transport
            ));
        }
        let transport = self.connector.feed_transport(context)?;
        Ok(SessionFeed::new(
            generation,
            SessionFeedContext {
                plot_id: context.plot_id.clone(),
                session_id: context.session_id.clone(),
                endpoint_kind: protocol.kind.clone(),
                endpoint_address: protocol.address.clone(),
            },
            resume,
            transport,
        ))
    }

    fn rebuild_structured_feed(&mut self) -> bool {
        let Some(context) = self.selected_session_context() else {
            return false;
        };
        if let Err(error) = self.timelines.retry_replay() {
            let detail = format!("cannot retry durable Session replay: {error:?}");
            self.force_terminal_fallback();
            self.status = self.terminal_fallback_status(detail);
            return false;
        }
        let resume = FeedResumePoint {
            stream_id: self.timelines.current_stream_id().map(str::to_owned),
            sequence: self.timelines.current_sequence().unwrap_or(0),
            cursor: self.timelines.current_cursor().map(str::to_owned),
        };
        if let Some(mut feed) = self.feed.take() {
            feed.close();
        }
        let generation = self.generation.saturating_add(1);
        self.generation = generation;
        self.rejected_generation = None;
        self.session_host_process = None;
        self.host_process_task = None;
        self.terminal_task = None;
        self.begin_host_process_lookup(&context);
        if self.presentation == RuntimePresentation::Terminal
            && self.terminal.is_none()
            && let Err(error) = self.ensure_terminal()
        {
            self.terminal_error = Some(error);
        }
        match self.build_feed(generation, &context, resume) {
            Ok(feed) => {
                self.feed = Some(feed);
                self.mark_pending_for_retry();
                self.status = RuntimeStatus::Degraded {
                    detail: "restoring durable Session history".to_owned(),
                };
                true
            }
            Err(error) => {
                self.force_terminal_fallback();
                self.status = self.terminal_fallback_status(error);
                false
            }
        }
    }

    fn send_pending(&mut self, key: &SessionKey, now_ms: u64) -> Result<(), String> {
        let Some(pending) = self.pending_prompts.get(key).cloned() else {
            return Ok(());
        };
        let Some(feed) = self.feed.as_mut() else {
            return Err("structured Session feed is unavailable".to_owned());
        };
        match feed.submit_prompt(&pending.command_id, &pending.text, now_ms) {
            Ok(()) => {
                if let Some(pending) = self.pending_prompts.get_mut(key) {
                    pending.needs_send = false;
                }
                Ok(())
            }
            Err(error) => {
                if let Some(pending) = self.pending_prompts.get_mut(key) {
                    pending.needs_send = true;
                }
                self.force_terminal_fallback();
                self.status = self.terminal_fallback_status(error.message.clone());
                Err(error.message)
            }
        }
    }

    fn clear_acknowledged(&mut self, command_id: &str) {
        let Some(context) = self.selected_session_context() else {
            return;
        };
        let key = SessionKey::from(&context);
        if self
            .pending_prompts
            .get(&key)
            .is_some_and(|pending| pending.command_id == command_id)
        {
            self.pending_prompts.remove(&key);
        }
    }

    fn clear_pending_if_durable(&mut self) {
        let Some(context) = self.selected_session_context() else {
            return;
        };
        let key = SessionKey::from(&context);
        let acknowledged = self
            .pending_prompts
            .get(&key)
            .is_some_and(|pending| self.timelines.current_contains_command(&pending.command_id));
        if acknowledged {
            self.pending_prompts.remove(&key);
        }
    }

    fn clear_current_pending(&mut self) {
        if let Some(context) = self.selected_session_context() {
            self.pending_prompts.remove(&SessionKey::from(&context));
        }
    }

    fn mark_pending_for_retry(&mut self) {
        if let Some(context) = self.selected_session_context()
            && let Some(pending) = self.pending_prompts.get_mut(&SessionKey::from(&context))
        {
            pending.needs_send = true;
        }
    }

    fn fail_contract(&mut self, detail: String) {
        self.mark_pending_for_retry();
        self.rejected_generation = Some(self.generation);
        if let Some(feed) = self.feed.as_mut() {
            feed.close();
        }
        self.force_terminal_fallback();
        self.status = self.terminal_fallback_status(detail);
    }

    fn refresh_status(&mut self) {
        let state = self.feed_state().cloned();
        self.apply_feed_status(state.as_ref());
    }

    fn apply_feed_status(&mut self, state: Option<&FeedState>) {
        if matches!(state, Some(FeedState::Live)) && self.forced_terminal {
            self.presentation = RuntimePresentation::Output;
            self.forced_terminal = false;
        }
        self.status = match state {
            Some(FeedState::Live) if self.terminal.is_none() => self
                .terminal_error
                .clone()
                .map_or(RuntimeStatus::Ready, |terminal_error| {
                    RuntimeStatus::StructuredOnly { terminal_error }
                }),
            Some(FeedState::Live) => RuntimeStatus::Ready,
            Some(FeedState::Backoff { reason, .. }) => {
                self.terminal_fallback_status(reason.clone())
            }
            Some(FeedState::Fatal { message, .. }) => {
                self.terminal_fallback_status(message.clone())
            }
            Some(state) => RuntimeStatus::Degraded {
                detail: format!("structured Session feed is {state:?}"),
            },
            None => self.terminal_error.clone().map_or_else(
                || {
                    self.terminal_fallback_status(
                        "structured Session feed is unavailable".to_owned(),
                    )
                },
                |terminal_error| RuntimeStatus::Unavailable {
                    reason: format!(
                        "structured Session unavailable; terminal unavailable: {terminal_error}"
                    ),
                },
            ),
        };
    }

    fn terminal_fallback_status(&self, structured_error: String) -> RuntimeStatus {
        if self.terminal.is_some() {
            RuntimeStatus::TerminalOnly { structured_error }
        } else if self.terminal_task.is_some() || self.host_process_task.is_some() {
            RuntimeStatus::Degraded {
                detail: format!(
                    "structured Session unavailable: {structured_error}; attaching Terminal"
                ),
            }
        } else {
            RuntimeStatus::Unavailable {
                reason: self.terminal_error.as_ref().map_or_else(
                    || structured_error.clone(),
                    |terminal_error| {
                        format!(
                            "structured Session unavailable: {structured_error}; terminal unavailable: {terminal_error}"
                        )
                    },
                ),
            }
        }
    }

    fn close_bindings(&mut self) {
        self.mark_pending_for_retry();
        self.host_process_task = None;
        self.terminal_task = None;
        if let Some(mut feed) = self.feed.take() {
            feed.close();
        }
        if let Some(mut terminal) = self.terminal.take() {
            terminal.close();
        }
        self.terminal_output.take();
    }

    fn resolve_fatal_pending(&mut self, code: &str) {
        if code == "command_conflict" {
            self.clear_current_pending();
        } else {
            self.mark_pending_for_retry();
        }
    }

    fn outcome(&self, changed: bool) -> RetargetOutcome {
        RetargetOutcome {
            generation: self.generation,
            changed,
            context: self.selected_session_context(),
            status: self.status.clone(),
        }
    }

    fn status_detail(&self, fallback: &str) -> String {
        match &self.status {
            RuntimeStatus::StructuredOnly { terminal_error } => terminal_error.clone(),
            RuntimeStatus::TerminalOnly { structured_error } => structured_error.clone(),
            RuntimeStatus::Unavailable { reason } => reason.clone(),
            RuntimeStatus::Degraded { detail } => detail.clone(),
            RuntimeStatus::Ready | RuntimeStatus::ExecutionSelected => fallback.to_owned(),
        }
    }

    fn restore(&self, text: &str, reason: &str) -> SubmitOutcome {
        SubmitOutcome::RestoreText {
            text: text.to_owned(),
            reason: reason.to_owned(),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl<C> Drop for SessionRuntime<C>
where
    C: RuntimeConnector,
{
    fn drop(&mut self) {
        self.close_bindings();
    }
}

fn update_generation(update: &FeedUpdate) -> u64 {
    match update {
        FeedUpdate::State { generation, .. }
        | FeedUpdate::Event { generation, .. }
        | FeedUpdate::ReplayComplete { generation, .. }
        | FeedUpdate::Error { generation, .. }
        | FeedUpdate::ModelState { generation, .. }
        | FeedUpdate::ModelError { generation, .. } => *generation,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};
    use std::rc::Rc;

    use nopal_feed_client::session::{
        DURABLE_SESSION_EVENT_KIND, DurableSessionEvent, SESSION_REPLAY_COMPLETE_KIND,
        SessionEventPayload, SessionReplayComplete,
    };
    use nopal_feed_client::session_activity::{
        DURABLE_SESSION_ACTIVITY_EVENT_KIND, DurableSessionActivityEvent,
        SessionActivityEventPayload,
    };

    use super::*;
    use crate::activity::VerifiedSessionEvent;
    use crate::model::{DesktopActivity, DesktopSessionProtocol};
    use crate::session_feed::{ClientFeedFrame, FeedConnection, FeedError, SessionFeedServerFrame};
    use crate::source::CommandOutput;

    #[derive(Default)]
    struct Harness {
        incoming: VecDeque<SessionFeedServerFrame>,
        sent: Vec<ClientFeedFrame>,
        log: Vec<String>,
        fail_next_prompt: bool,
        connections: usize,
        terminal_bindings: usize,
    }

    #[derive(Clone, Default)]
    struct FakeTransport(Rc<RefCell<Harness>>);

    struct FakeConnection(Rc<RefCell<Harness>>);

    impl FeedConnection for FakeConnection {
        fn send(&mut self, frame: ClientFeedFrame) -> Result<(), FeedError> {
            let mut harness = self.0.borrow_mut();
            if let ClientFeedFrame::Subscribe(subscribe) = &frame {
                for incoming in &mut harness.incoming {
                    if let SessionFeedServerFrame::ReplayComplete(complete) = incoming {
                        complete.request_id.clone_from(&subscribe.request_id);
                    }
                }
            }
            if matches!(frame, ClientFeedFrame::Prompt(_)) && harness.fail_next_prompt {
                harness.fail_next_prompt = false;
                return Err(FeedError::io("send interrupted"));
            }
            harness.sent.push(frame);
            Ok(())
        }

        fn try_receive(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError> {
            Ok(self.0.borrow_mut().incoming.pop_front())
        }

        fn close(&mut self) {
            self.0.borrow_mut().log.push("feed-close".to_owned());
        }
    }

    impl FeedTransport for FakeTransport {
        type Connection = FakeConnection;

        fn connect(
            &mut self,
            _context: &SessionFeedContext,
        ) -> Result<Self::Connection, FeedError> {
            self.0.borrow_mut().connections += 1;
            Ok(FakeConnection(self.0.clone()))
        }
    }

    struct FakeTerminal {
        pane_id: String,
        process_identity: TerminalProcessIdentity,
        chunks: Rc<RefCell<Vec<Vec<u8>>>>,
        log: Rc<RefCell<Harness>>,
        identity_valid: Rc<Cell<bool>>,
    }

    impl TerminalSessionBinding for FakeTerminal {
        fn pane_id(&self) -> &str {
            &self.pane_id
        }

        fn process_identity(&self) -> TerminalProcessIdentity {
            self.process_identity
        }

        fn apply_output(&mut self, bytes: &[u8]) {
            self.chunks.borrow_mut().push(bytes.to_vec());
        }

        fn poll_output(&mut self) -> Result<Option<Vec<u8>>, String> {
            self.identity_valid
                .get()
                .then_some(None)
                .ok_or_else(|| "Terminal process identity changed".to_owned())
        }

        fn close(&mut self) {
            self.log.borrow_mut().log.push("terminal-close".to_owned());
        }
    }

    type FakeTerminalResult = Result<TerminalConnection<FakeTerminal>, String>;
    type FakeTerminalSender = async_channel::Sender<FakeTerminalResult>;

    #[derive(Clone)]
    struct FakeConnector {
        transport: FakeTransport,
        terminal_chunks: Rc<RefCell<Vec<Vec<u8>>>>,
        terminal_senders: Rc<RefCell<Vec<FakeTerminalSender>>>,
        feed_available: bool,
        terminal_available: bool,
        defer_terminal: bool,
        wrong_pane: bool,
        wrong_process: bool,
        host_process: Rc<Cell<u32>>,
        terminal_identity_valid: Rc<Cell<bool>>,
    }

    impl RuntimeConnector for FakeConnector {
        type FeedTransport = FakeTransport;
        type Terminal = FakeTerminal;

        fn feed_transport(
            &self,
            _context: &SelectedSessionContext,
        ) -> Result<Self::FeedTransport, String> {
            self.feed_available
                .then(|| self.transport.clone())
                .ok_or_else(|| "feed unavailable".to_owned())
        }

        fn session_host_process(
            &self,
            _context: &SelectedSessionContext,
        ) -> ConnectorTask<SessionHostProcessIdentity> {
            ConnectorTask::Ready(
                SessionHostProcessIdentity::new(self.host_process.get())
                    .map_err(|error| format!("invalid fixture host process: {error}")),
            )
        }

        fn bind_terminal(
            &self,
            context: &SelectedSessionContext,
        ) -> ConnectorTask<TerminalConnection<Self::Terminal>> {
            self.transport.0.borrow_mut().terminal_bindings += 1;
            if !self.terminal_available {
                return ConnectorTask::Ready(Err("terminal unavailable".to_owned()));
            }
            let connection = self.terminal_connection(context);
            if self.defer_terminal {
                let (sender, receiver) = async_channel::bounded(1);
                self.terminal_senders.borrow_mut().push(sender);
                return ConnectorTask::Pending(receiver);
            }
            ConnectorTask::Ready(Ok(connection))
        }
    }

    impl FakeConnector {
        fn terminal_connection(
            &self,
            context: &SelectedSessionContext,
        ) -> TerminalConnection<FakeTerminal> {
            let pane_id = if self.wrong_pane {
                "%wrong".to_owned()
            } else {
                context.host_pane.clone().expect("fixture pane")
            };
            let (_sender, output) = async_channel::unbounded();
            TerminalConnection {
                binding: FakeTerminal {
                    pane_id,
                    process_identity: TerminalProcessIdentity::new(if self.wrong_process {
                        self.host_process.get().saturating_add(1)
                    } else {
                        self.host_process.get()
                    })
                    .expect("valid fixture Terminal process"),
                    chunks: self.terminal_chunks.clone(),
                    log: self.transport.0.clone(),
                    identity_valid: self.terminal_identity_valid.clone(),
                },
                output,
            }
        }
    }

    fn connector(harness: Rc<RefCell<Harness>>) -> FakeConnector {
        FakeConnector {
            transport: FakeTransport(harness),
            terminal_chunks: Rc::default(),
            terminal_senders: Rc::default(),
            feed_available: true,
            terminal_available: true,
            defer_terminal: false,
            wrong_pane: false,
            wrong_process: false,
            host_process: Rc::new(Cell::new(4312)),
            terminal_identity_valid: Rc::new(Cell::new(true)),
        }
    }

    struct ScriptedRunner {
        outputs: RefCell<VecDeque<Result<CommandOutput, String>>>,
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, _program: &str, _args: &[&str]) -> Result<CommandOutput, String> {
            self.outputs
                .borrow_mut()
                .pop_front()
                .expect("scripted command output")
        }
    }

    struct WorkerBackend {
        process_ids: RefCell<VecDeque<u32>>,
        resizes: RefCell<Vec<(usize, usize)>>,
    }

    impl TerminalWorkerBackend for WorkerBackend {
        fn pane_process_id(&self, _pane_id: &str) -> Result<u32, String> {
            self.process_ids
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| "missing scripted process identity".to_owned())
        }

        fn capture(&self, _pane_id: &str) -> Result<Vec<u8>, String> {
            Ok(Vec::new())
        }

        fn send_input(&self, _pane_id: &str, _bytes: &[u8]) -> Result<(), String> {
            Err("injected input failure".to_owned())
        }

        fn resize_pane(&self, _pane_id: &str, columns: usize, rows: usize) -> Result<(), String> {
            self.resizes.borrow_mut().push((columns, rows));
            Ok(())
        }
    }

    fn command_output(stdout: &str) -> Result<CommandOutput, String> {
        Ok(CommandOutput {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    fn failed_worker_context() -> TerminalWorkerContext {
        let (commands, receiver) = std::sync::mpsc::sync_channel(1);
        commands
            .send(TerminalWorkerCommand::SendInput(b"fail".to_vec()))
            .expect("queue injected failure");
        TerminalWorkerContext {
            pane_id: "%1".to_owned(),
            expected_process: TerminalProcessIdentity::new(4312).expect("fixture process"),
            last_capture: Vec::new(),
            commands: receiver,
            stop: Arc::new(AtomicBool::new(false)),
            original_size: (100, 30),
            latest_capture: Arc::new(Mutex::new(None)),
            failure: Arc::new(Mutex::new(None)),
            ownership: test_ownership(1),
        }
    }

    fn test_ownership(epoch: u64) -> PaneOwnershipLease {
        PaneOwnershipLease {
            owner: Arc::new(Mutex::new(PaneOwnership { epoch })),
            epoch,
        }
    }

    #[test]
    fn terminal_handshake_rejects_a_process_replaced_after_capture() {
        let runner = ScriptedRunner {
            outputs: RefCell::new(VecDeque::from([
                command_output("4312\n"),
                command_output("100 30\n"),
                command_output("replacement output\n"),
                command_output("5401\n"),
            ])),
        };
        let transport = TmuxTransport::new(runner);

        let error = capture_terminal_handshake(&transport, "%1")
            .err()
            .expect("replacement process must invalidate the whole handshake");

        assert!(error.contains("expected 4312, found 5401"), "{error}");
    }

    #[test]
    fn worker_failure_restores_size_only_for_the_exact_original_process() {
        let same_process = WorkerBackend {
            process_ids: RefCell::new(VecDeque::from([4312, 4312])),
            resizes: RefCell::new(Vec::new()),
        };
        run_terminal_worker_with_transport(failed_worker_context(), &same_process);
        assert_eq!(same_process.resizes.borrow().as_slice(), &[(100, 30)]);

        let replacement_process = WorkerBackend {
            process_ids: RefCell::new(VecDeque::from([4312, 5401])),
            resizes: RefCell::new(Vec::new()),
        };
        run_terminal_worker_with_transport(failed_worker_context(), &replacement_process);
        assert!(replacement_process.resizes.borrow().is_empty());
    }

    #[test]
    fn terminal_reaper_never_joins_the_worker_on_the_calling_thread() {
        let (release, wait) = std::sync::mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let worker = std::thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_millis(500));
            worker_finished.store(true, Ordering::Release);
        });
        let started = Instant::now();

        reap_terminal_worker(worker);

        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(!finished.load(Ordering::Acquire));
        release.send(()).expect("release detached worker");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !finished.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(finished.load(Ordering::Acquire));
    }

    #[test]
    fn retired_worker_cannot_restore_after_new_same_pane_ownership_commits() {
        let pane_id = format!("%ownership-{}", Uuid::new_v4());
        let (_, old_ownership) =
            establish_terminal_ownership(&pane_id, || Ok(())).expect("first pane owner");
        let (_, new_ownership) =
            establish_terminal_ownership(&pane_id, || Ok(())).expect("replacement pane owner");
        let backend = WorkerBackend {
            process_ids: RefCell::new(VecDeque::new()),
            resizes: RefCell::new(Vec::new()),
        };
        new_ownership
            .with_current(|| {
                backend
                    .resize_pane(&pane_id, 120, 40)
                    .expect("new owner resize");
            })
            .expect("new ownership state");
        let mut retired = failed_worker_context();
        retired.pane_id = pane_id;
        retired.ownership = old_ownership;
        retired.stop.store(true, Ordering::Release);

        run_terminal_worker_with_transport(retired, &backend);

        assert_eq!(backend.resizes.borrow().as_slice(), &[(120, 40)]);
    }

    #[test]
    fn native_shutdown_drain_waits_for_every_owned_terminal_worker() {
        let (release, wait) = std::sync::mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let worker = std::thread::spawn(move || {
            wait.recv().expect("release cleanup worker");
            worker_finished.store(true, Ordering::Release);
        });
        reap_terminal_worker(worker);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            release.send(()).expect("release tracked cleanup");
        });

        drain_native_background_tasks().expect("drain native background task registry");
        releaser.join().expect("join test releaser");

        assert!(finished.load(Ordering::Acquire));
    }

    #[test]
    fn completed_worker_handles_are_reaped_during_the_resident_runtime() {
        let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut retired_ids = Vec::new();
        for _ in 0..64 {
            let worker_completed = Arc::clone(&completed);
            let worker = std::thread::spawn(move || {
                worker_completed.fetch_add(1, Ordering::AcqRel);
            });
            retired_ids.push(worker.thread().id());
            track_native_background_task(worker);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while completed.load(Ordering::Acquire) < retired_ids.len() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(completed.load(Ordering::Acquire), retired_ids.len());

        reap_finished_native_background_tasks();

        let tracked =
            lock_recovering(NATIVE_BACKGROUND_TASKS.get_or_init(|| Mutex::new(Vec::new())));
        assert!(
            tracked
                .iter()
                .all(|handle| !retired_ids.contains(&handle.thread().id()))
        );
    }

    fn protocol(id: &str) -> DesktopSessionProtocol {
        DesktopSessionProtocol {
            kind: "nopal.session/v2".to_owned(),
            transport: "unix".to_owned(),
            address: format!("/tmp/{id}.sock"),
            state: "ready".to_owned(),
            extra: BTreeMap::new(),
        }
    }

    fn session(id: &str, pane: &str) -> DesktopActivity {
        DesktopActivity::Session {
            session_id: id.to_owned(),
            host_pane: Some(pane.to_owned()),
            state: "active".to_owned(),
            protocol: Some(protocol(id)),
        }
    }

    fn field() -> DesktopField {
        DesktopField {
            plots: vec![
                DesktopPlot {
                    plot_id: "plot-a".to_owned(),
                    title: "A".to_owned(),
                    progress: "active".to_owned(),
                    conditions: Vec::new(),
                    activities: vec![
                        session("session-a", "%1"),
                        DesktopActivity::Execution {
                            service_id: "rondo".to_owned(),
                            repo_id: "repo".to_owned(),
                            run_id: "run".to_owned(),
                            status: "running".to_owned(),
                        },
                    ],
                    selected_session_id: Some("session-a".to_owned()),
                    extra: BTreeMap::new(),
                },
                DesktopPlot {
                    plot_id: "plot-b".to_owned(),
                    title: "B".to_owned(),
                    progress: "active".to_owned(),
                    conditions: Vec::new(),
                    activities: vec![session("session-b", "%2")],
                    selected_session_id: Some("session-b".to_owned()),
                    extra: BTreeMap::new(),
                },
            ],
            selected_plot_id: Some("plot-a".to_owned()),
            extra: BTreeMap::new(),
        }
    }

    fn field_v4() -> DesktopField {
        let mut field = field();
        for plot in &mut field.plots {
            for activity in &mut plot.activities {
                if let DesktopActivity::Session {
                    protocol: Some(protocol),
                    ..
                } = activity
                {
                    protocol.kind = "nopal.session/v4".to_owned();
                }
            }
        }
        field
    }

    fn model(provider: &str, id: &str, name: &str) -> SessionModelDescriptor {
        SessionModelDescriptor {
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
            extra: BTreeMap::new(),
        }
    }

    fn model_state(
        request_id: Option<&str>,
        revision: u64,
        current: SessionModelDescriptor,
    ) -> SessionFeedServerFrame {
        SessionFeedServerFrame::ModelState(SessionModelState {
            kind: nopal_feed_client::session::SESSION_MODEL_STATE_KIND.to_owned(),
            plot_id: "plot-a".to_owned(),
            session_id: "session-a".to_owned(),
            request_id: request_id.map(str::to_owned),
            state_epoch: "model-epoch-a".to_owned(),
            revision,
            agent_state: SessionAgentState::Idle,
            current: Some(current),
            available: vec![
                model("nopal-proof", "deterministic-a", "Model A"),
                model("nopal-proof", "deterministic-b", "Model B"),
            ],
            extra: BTreeMap::new(),
        })
    }

    fn complete(
        plot: &str,
        session: &str,
        sequence: u64,
        cursor: Option<&str>,
        count: u64,
    ) -> SessionFeedServerFrame {
        SessionFeedServerFrame::ReplayComplete(SessionReplayComplete {
            kind: SESSION_REPLAY_COMPLETE_KIND.to_owned(),
            request_id: format!("replay-{plot}-{sequence}"),
            plot_id: plot.to_owned(),
            session_id: session.to_owned(),
            stream_id: format!("stream-{session}"),
            cursor: cursor.map(str::to_owned),
            sequence,
            event_count: count,
            extra: BTreeMap::new(),
        })
    }

    fn event(
        plot: &str,
        session: &str,
        sequence: u64,
        previous_cursor: Option<&str>,
        command_id: Option<&str>,
    ) -> SessionFeedServerFrame {
        SessionFeedServerFrame::Event(Box::new(VerifiedSessionEvent::V2(DurableSessionEvent {
            kind: DURABLE_SESSION_EVENT_KIND.to_owned(),
            event_id: format!("event-{session}-{sequence}"),
            plot_id: plot.to_owned(),
            session_id: session.to_owned(),
            stream_id: format!("stream-{session}"),
            sequence,
            previous_cursor: previous_cursor.map(str::to_owned),
            cursor: format!("cursor-{session}-{sequence}"),
            command_id: command_id.map(str::to_owned),
            event: SessionEventPayload::UserMessage {
                text: "hello".to_owned(),
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        })))
    }

    fn v3_event(
        sequence: u64,
        previous_cursor: Option<&str>,
        event: SessionActivityEventPayload,
    ) -> SessionFeedServerFrame {
        SessionFeedServerFrame::Event(Box::new(VerifiedSessionEvent::V3(
            DurableSessionActivityEvent {
                kind: DURABLE_SESSION_ACTIVITY_EVENT_KIND.to_owned(),
                event_id: format!("event-session-a-{sequence}"),
                plot_id: "plot-a".to_owned(),
                session_id: "session-a".to_owned(),
                stream_id: "stream-session-a".to_owned(),
                sequence,
                previous_cursor: previous_cursor.map(str::to_owned),
                cursor: format!("cursor-session-a-{sequence}"),
                command_id: None,
                event,
                extra: BTreeMap::new(),
            },
        )))
    }

    #[test]
    fn cold_restore_becomes_live_and_drains_durable_history() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness.borrow_mut().incoming.extend([
            event("plot-a", "session-a", 1, None, None),
            complete("plot-a", "session-a", 1, Some("cursor-session-a-1"), 1),
        ]);
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));

        let outcome = runtime.drain_at(0);

        assert_eq!(outcome.events_applied, 1);
        assert_eq!(runtime.current_events().len(), 1);
        assert_eq!(runtime.feed_state(), Some(&FeedState::Live));
        assert_eq!(runtime.replay_state(), ReplayState::Live);
        assert!(runtime.can_submit());
        assert_eq!(runtime.status(), &RuntimeStatus::Ready);
    }

    #[test]
    fn healthy_output_binds_terminal_only_after_explicit_intent() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        runtime.drain_at(0);

        assert!(runtime.terminal_binding().is_none());
        assert_eq!(harness.borrow().terminal_bindings, 0);

        runtime.set_presentation(RuntimePresentation::Terminal);

        assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
        assert_eq!(
            runtime
                .terminal_binding()
                .map(TerminalSessionBinding::pane_id),
            Some("%1")
        );
        assert_eq!(harness.borrow().terminal_bindings, 1);

        runtime.set_presentation(RuntimePresentation::Output);
        runtime.set_presentation(RuntimePresentation::Terminal);
        assert_eq!(harness.borrow().terminal_bindings, 1);
    }

    #[test]
    fn forced_terminal_fallback_recovers_output_without_overwriting_explicit_intent() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness));
        runtime.drain_at(0);
        let generation = runtime.generation();

        runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Backoff {
                    attempt: 1,
                    retry_at_ms: 10,
                    reason: "structured transport interrupted".to_owned(),
                },
            },
        );

        assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
        assert!(runtime.terminal_binding().is_some());

        runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Live,
            },
        );
        assert_eq!(runtime.presentation(), RuntimePresentation::Output);

        runtime.set_presentation(RuntimePresentation::Terminal);
        runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Backoff {
                    attempt: 2,
                    retry_at_ms: 20,
                    reason: "structured transport interrupted again".to_owned(),
                },
            },
        );
        runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Live,
            },
        );
        assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
    }

    #[test]
    fn model_switch_requires_exact_ack_and_disconnect_clears_unconfirmed_intent() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness.borrow_mut().incoming.extend([
            complete("plot-a", "session-a", 0, None, 0),
            model_state(None, 1, model("nopal-proof", "deterministic-a", "Model A")),
        ]);
        let mut runtime = SessionRuntime::new(field_v4(), connector(harness.clone()));
        runtime.drain_at(0);
        assert!(runtime.can_switch_model());

        let target = model("nopal-proof", "deterministic-b", "Model B");
        let request_id = runtime.switch_model(&target).expect("send model switch");
        assert!(runtime.model_switch_pending());
        assert!(matches!(
            harness.borrow().sent.last(),
            Some(ClientFeedFrame::Model(request)) if request.request_id == request_id
        ));

        harness
            .borrow_mut()
            .incoming
            .push_back(model_state(Some(&request_id), 2, target.clone()));
        runtime.drain_at(1);
        assert!(!runtime.model_switch_pending());
        assert_eq!(runtime.take_confirmed_model_switch(), Some(target));

        let unconfirmed = model("nopal-proof", "deterministic-a", "Model A");
        runtime
            .switch_model(&unconfirmed)
            .expect("send second model switch");
        let generation = runtime.generation();
        runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Backoff {
                    attempt: 1,
                    retry_at_ms: 10,
                    reason: "connection interrupted".to_owned(),
                },
            },
        );
        assert!(!runtime.model_switch_pending());
        assert!(runtime.model_state().is_none());
        assert!(runtime.model_error().is_some());
    }

    #[test]
    fn v3_restore_counts_and_retains_exact_message_and_activity_envelopes() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness.borrow_mut().incoming.extend([
            v3_event(
                1,
                None,
                SessionActivityEventPayload::AssistantMessage {
                    text: "Exact v3".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
            v3_event(
                2,
                Some("cursor-session-a-1"),
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "activity-1".to_owned(),
                    tool_call_id: "call-1".to_owned(),
                    command: "printf exact".to_owned(),
                    started_at: "2026-07-13T17:00:00Z".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            complete("plot-a", "session-a", 2, Some("cursor-session-a-2"), 2),
        ]);
        let mut v3_field = field();
        let DesktopActivity::Session { protocol, .. } = &mut v3_field.plots[0].activities[0] else {
            panic!("fixture session")
        };
        protocol.as_mut().unwrap().kind = "nopal.session/v3".to_owned();
        let mut runtime = SessionRuntime::new(v3_field, connector(harness));

        let outcome = runtime.drain_at(0);

        assert_eq!(outcome.events_applied, 2);
        assert_eq!(runtime.current_events().len(), 1);
        assert_eq!(runtime.current_events()[0].event_id, "event-session-a-1");
        assert_eq!(runtime.timelines.current_verified_events().len(), 2);
        assert!(matches!(
            &runtime.timelines.current_verified_events()[1],
            VerifiedSessionEvent::V3(DurableSessionActivityEvent {
                event: SessionActivityEventPayload::CommandStarted { activity_id, .. },
                ..
            }) if activity_id == "activity-1"
        ));
        assert_eq!(runtime.replay_state(), ReplayState::Live);
    }

    #[test]
    fn live_transport_does_not_enable_submission_while_history_is_reconnecting() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness));
        runtime.drain_at(0);
        let generation = runtime.generation();

        runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Backoff {
                    attempt: 1,
                    retry_at_ms: 1,
                    reason: "history restore interrupted".to_owned(),
                },
            },
        );

        assert_eq!(runtime.feed_state(), Some(&FeedState::Live));
        assert!(matches!(
            runtime.replay_state(),
            ReplayState::Reconnecting { .. }
        ));
        assert!(!runtime.can_submit());
    }

    #[test]
    fn zero_event_replay_reports_a_visible_state_change() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness));

        let restored = runtime.drain_at(0);
        let idle = runtime.drain_at(1);

        assert!(restored.visible_changed);
        assert!(!idle.visible_changed);
        assert_eq!(restored.events_applied, 0);
        assert_eq!(runtime.status(), &RuntimeStatus::Ready);
    }

    #[test]
    fn retarget_closes_feed_before_terminal_and_binds_the_exact_new_pane() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        runtime.drain_at(0);
        runtime.set_presentation(RuntimePresentation::Terminal);
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-b", "session-b", 0, None, 0));

        runtime.select_plot("plot-b").expect("known plot");

        assert_eq!(&harness.borrow().log[..2], ["feed-close", "terminal-close"]);
        assert_eq!(
            runtime
                .terminal_binding()
                .map(TerminalSessionBinding::pane_id),
            Some("%2")
        );
        let stale = runtime.generation() - 1;
        assert!(!runtime.apply_terminal_output(stale, b"late"));
    }

    #[test]
    fn retarget_before_ack_resends_the_same_command_id_when_replay_misses_it() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        runtime.drain_at(0);
        let SubmitOutcome::Sent { command_id } = runtime.submit_prompt("survive retarget") else {
            panic!("live prompt should be accepted");
        };

        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-b", "session-b", 0, None, 0));
        runtime.select_plot("plot-b").expect("plot b");
        runtime.drain_at(1);
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        runtime.select_plot("plot-a").expect("plot a");
        runtime.drain_at(2);

        let prompt_ids = harness
            .borrow()
            .sent
            .iter()
            .filter_map(|frame| match frame {
                ClientFeedFrame::Prompt(command) => Some(command.command_id.clone()),
                ClientFeedFrame::Subscribe(_) => None,
                ClientFeedFrame::Model(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(prompt_ids, [command_id.clone(), command_id]);
        assert!(
            !runtime.can_submit(),
            "the retry remains pending until durable ack"
        );
    }

    #[test]
    fn non_command_fatal_preserves_pending_identity_for_later_recovery() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        runtime.drain_at(0);
        let SubmitOutcome::Sent { command_id } = runtime.submit_prompt("survive fatal") else {
            panic!("live prompt should be accepted");
        };
        let generation = runtime.generation();
        assert!(runtime.apply_feed_update(
            generation,
            FeedUpdate::Error {
                generation,
                code: "history_gap".to_owned(),
                message: "history gap".to_owned(),
                retryable: false,
            },
        ));

        let key = SessionKey {
            plot_id: "plot-a".to_owned(),
            session_id: "session-a".to_owned(),
        };
        let pending = runtime
            .pending_prompts
            .get(&key)
            .expect("fatal history failure must preserve the pending prompt");
        assert_eq!(pending.command_id, command_id);
        assert_eq!(pending.text, "survive fatal");
        assert!(pending.needs_send);
    }

    #[test]
    fn invalid_retarget_is_rolled_back_without_closing_bindings() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        let generation = runtime.generation();

        assert_eq!(
            runtime.select_plot("missing"),
            Err(RuntimeError::Selection(SelectionError::UnknownPlot))
        );
        assert_eq!(runtime.generation(), generation);
        assert_eq!(runtime.field().selected_plot_id.as_deref(), Some("plot-a"));
        assert!(harness.borrow().log.is_empty());
    }

    #[test]
    fn exact_pane_mismatch_closes_wrong_binding_and_keeps_output_only() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut connector = connector(harness.clone());
        connector.wrong_pane = true;
        let mut runtime = SessionRuntime::new(field(), connector);
        runtime.drain_at(0);
        runtime.set_presentation(RuntimePresentation::Terminal);

        assert!(runtime.terminal_binding().is_none());
        assert!(
            matches!(runtime.status(), RuntimeStatus::StructuredOnly { terminal_error } if terminal_error.contains("mismatch"))
        );
        assert_eq!(harness.borrow().log, ["terminal-close"]);
    }

    #[test]
    fn exact_process_mismatch_closes_wrong_binding_and_keeps_output_only() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut connector = connector(harness.clone());
        connector.wrong_process = true;
        let mut runtime = SessionRuntime::new(field(), connector);
        runtime.drain_at(0);

        runtime.set_presentation(RuntimePresentation::Terminal);

        assert!(runtime.terminal_binding().is_none());
        assert!(
            matches!(runtime.status(), RuntimeStatus::StructuredOnly { terminal_error } if terminal_error.contains("process mismatch"))
        );
        assert_eq!(harness.borrow().log, ["terminal-close"]);
    }

    #[test]
    fn stale_terminal_identity_is_closed_until_structured_recovery_refreshes_trust() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let connector = connector(harness.clone());
        let host_process = connector.host_process.clone();
        let identity_valid = connector.terminal_identity_valid.clone();
        let mut runtime = SessionRuntime::new(field(), connector);
        runtime.drain_at(0);
        runtime.set_presentation(RuntimePresentation::Terminal);
        assert_eq!(
            runtime
                .terminal_binding()
                .map(TerminalSessionBinding::process_identity)
                .map(TerminalProcessIdentity::get),
            Some(4312)
        );

        host_process.set(5401);
        identity_valid.set(false);
        let outcome = runtime.drain_at(1);

        assert!(
            outcome
                .errors
                .iter()
                .any(|error| error.contains("identity changed"))
        );
        assert!(runtime.terminal_binding().is_none());
        assert_eq!(runtime.presentation(), RuntimePresentation::Output);
        runtime.set_presentation(RuntimePresentation::Terminal);
        assert!(runtime.terminal_binding().is_none());
        assert_eq!(harness.borrow().terminal_bindings, 2);

        identity_valid.set(true);
        let generation = runtime.generation();
        assert!(runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Live,
            },
        ));
        runtime.set_presentation(RuntimePresentation::Terminal);

        assert_eq!(
            runtime
                .terminal_binding()
                .map(TerminalSessionBinding::process_identity)
                .map(TerminalProcessIdentity::get),
            Some(5401)
        );
        assert_eq!(harness.borrow().terminal_bindings, 3);
    }

    #[test]
    fn production_terminal_poll_handoff_never_runs_tmux_on_the_caller() {
        let identity = TerminalProcessIdentity::new(1).expect("valid fixture process");
        let mut worker = TerminalWorker::start(
            "%missing".to_owned(),
            identity,
            Vec::new(),
            (80, 24),
            test_ownership(1),
        );
        let started = Instant::now();

        let _ = worker.poll();

        assert!(
            started.elapsed() < Duration::from_millis(20),
            "polling the worker receiver must remain nonblocking"
        );
        worker.close();
    }

    #[test]
    fn partial_binding_failures_preserve_structured_and_terminal_fallbacks() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        let mut no_feed = connector(harness.clone());
        no_feed.feed_available = false;
        let terminal_only = SessionRuntime::new(field(), no_feed);
        assert!(matches!(
            terminal_only.status(),
            RuntimeStatus::TerminalOnly { .. }
        ));
        assert!(terminal_only.terminal_binding().is_some());

        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut no_terminal = connector(harness);
        no_terminal.terminal_available = false;
        let mut structured_only = SessionRuntime::new(field(), no_terminal);
        structured_only.drain_at(0);
        structured_only.set_presentation(RuntimePresentation::Terminal);
        assert!(matches!(
            structured_only.status(),
            RuntimeStatus::StructuredOnly { .. }
        ));
        assert!(structured_only.feed_state().is_some());
    }

    #[test]
    fn execution_selection_disables_composer_and_restores_exact_text() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        let mut runtime = SessionRuntime::new(field(), connector(harness));

        runtime
            .select_activity(DesktopActivityKey::Execution {
                service_id: "rondo".to_owned(),
                repo_id: "repo".to_owned(),
                run_id: "run".to_owned(),
            })
            .expect("known execution");

        assert_eq!(runtime.status(), &RuntimeStatus::ExecutionSelected);
        assert!(!runtime.can_submit());
        assert!(
            matches!(runtime.submit_prompt(" exact "), SubmitOutcome::RestoreText { text, .. } if text == " exact ")
        );
    }

    #[test]
    fn presentation_and_per_session_drafts_survive_a_b_a_retargeting() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        let mut runtime = SessionRuntime::new(field(), connector(harness));
        runtime.set_presentation(RuntimePresentation::Terminal);
        runtime.set_composer_draft("draft a");
        runtime.select_plot("plot-b").expect("plot b");
        assert_eq!(runtime.composer_draft(), "");
        runtime.set_composer_draft("draft b");
        runtime.select_plot("plot-a").expect("plot a");

        assert_eq!(runtime.composer_draft(), "draft a");
        assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
        runtime.select_plot("plot-b").expect("plot b again");
        assert_eq!(runtime.composer_draft(), "draft b");
    }

    #[test]
    fn a_b_a_retarget_resumes_from_each_sessions_verified_cursor() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness.borrow_mut().incoming.extend([
            event("plot-a", "session-a", 1, None, None),
            complete("plot-a", "session-a", 1, Some("cursor-session-a-1"), 1),
        ]);
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        runtime.drain_at(0);
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-b", "session-b", 0, None, 0));
        runtime.select_plot("plot-b").expect("plot b");
        runtime.drain_at(1);
        harness.borrow_mut().incoming.push_back(complete(
            "plot-a",
            "session-a",
            1,
            Some("cursor-session-a-1"),
            0,
        ));
        runtime.select_plot("plot-a").expect("plot a");
        runtime.drain_at(2);

        let a_resume_points = harness
            .borrow()
            .sent
            .iter()
            .filter_map(|frame| match frame {
                ClientFeedFrame::Subscribe(subscribe) if subscribe.session_id == "session-a" => {
                    Some(subscribe.after_cursor.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            a_resume_points,
            [None, Some("cursor-session-a-1".to_owned())]
        );
        assert_eq!(runtime.current_events().len(), 1);
        assert_eq!(runtime.replay_state(), ReplayState::Live);
    }

    #[test]
    fn synchronous_send_failure_retries_the_same_command_id_until_durable_ack() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        runtime.drain_at(0);
        harness.borrow_mut().fail_next_prompt = true;

        let failed = runtime.submit_prompt("keep this exact");
        assert!(
            matches!(failed, SubmitOutcome::RestoreText { text, .. } if text == "keep this exact")
        );
        assert!(!runtime.can_submit());
        assert!(runtime.retry_now());
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        runtime.drain_at(1);

        let command_ids = harness
            .borrow()
            .sent
            .iter()
            .filter_map(|frame| match frame {
                ClientFeedFrame::Prompt(command) => Some(command.command_id.clone()),
                ClientFeedFrame::Subscribe(_) => None,
                ClientFeedFrame::Model(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(command_ids.len(), 1);
        let command_id = command_ids[0].clone();
        harness.borrow_mut().incoming.push_back(event(
            "plot-a",
            "session-a",
            1,
            None,
            Some(&command_id),
        ));
        runtime.drain_at(2);
        assert!(runtime.can_submit());
    }

    #[test]
    fn rapid_resubmit_reuses_pending_identity_and_rejects_a_different_instruction() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        runtime.drain_at(0);

        let first = runtime.submit_prompt("ship it");
        let repeated = runtime.submit_prompt("ship it");
        let different = runtime.submit_prompt("ship something else");

        assert_eq!(first, repeated);
        assert!(
            matches!(different, SubmitOutcome::RestoreText { text, .. } if text == "ship something else")
        );
        assert_eq!(
            harness
                .borrow()
                .sent
                .iter()
                .filter(|frame| matches!(frame, ClientFeedFrame::Prompt(_)))
                .count(),
            1
        );
    }

    #[test]
    fn independent_runtime_instances_do_not_reuse_command_ids() {
        let first_harness = Rc::new(RefCell::new(Harness::default()));
        first_harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut first = SessionRuntime::new(field(), connector(first_harness));
        first.drain_at(0);
        let SubmitOutcome::Sent {
            command_id: first_id,
        } = first.submit_prompt("first")
        else {
            panic!("first runtime should accept a prompt");
        };

        let second_harness = Rc::new(RefCell::new(Harness::default()));
        second_harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        let mut second = SessionRuntime::new(field(), connector(second_harness));
        second.drain_at(0);
        let SubmitOutcome::Sent {
            command_id: second_id,
        } = second.submit_prompt("second")
        else {
            panic!("second runtime should accept a prompt");
        };

        assert_ne!(first_id, second_id);
        for command_id in [first_id, second_id] {
            let value = command_id
                .strip_prefix("command-desktop-")
                .expect("command id prefix");
            let uuid = Uuid::parse_str(value).expect("command id UUID");
            assert_eq!(uuid.get_version(), Some(uuid::Version::Random));
        }
    }

    #[test]
    fn gap_freezes_verified_prefix_and_rejects_late_generation_updates() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness.borrow_mut().incoming.extend([
            event("plot-a", "session-a", 1, None, None),
            event("plot-a", "session-a", 3, Some("cursor-session-a-1"), None),
        ]);
        let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
        let generation = runtime.generation();

        let outcome = runtime.drain_at(0);

        assert_eq!(outcome.events_applied, 1);
        assert!(
            runtime.current_events().is_empty(),
            "the invalid replay must not publish its staged prefix"
        );
        assert!(matches!(
            runtime.feed_state(),
            Some(FeedState::Fatal { code, .. }) if code == "protocol"
        ));
        assert!(matches!(runtime.replay_state(), ReplayState::Failed(_)));
        assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
        assert!(matches!(
            runtime.status(),
            RuntimeStatus::TerminalOnly { .. }
        ));
        assert!(!runtime.apply_feed_update(
            generation,
            FeedUpdate::State {
                generation,
                state: FeedState::Live,
            },
        ));
        assert!(!runtime.apply_feed_update(
            generation - 1,
            FeedUpdate::State {
                generation: generation - 1,
                state: FeedState::Live,
            },
        ));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        assert!(runtime.retry_now());
        assert!(runtime.generation() > generation);
        runtime.drain_at(1);
        assert_eq!(runtime.replay_state(), ReplayState::Live);
        assert_eq!(runtime.presentation(), RuntimePresentation::Output);
    }

    #[test]
    fn retry_replaces_a_stale_pending_terminal_attachment_for_the_new_generation() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness.borrow_mut().incoming.extend([
            event("plot-a", "session-a", 1, None, None),
            event("plot-a", "session-a", 3, Some("cursor-session-a-1"), None),
        ]);
        let mut deferred = connector(harness.clone());
        deferred.defer_terminal = true;
        let control = deferred.clone();
        let mut runtime = SessionRuntime::new(field(), deferred);

        runtime.drain_at(0);
        assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
        assert_eq!(harness.borrow().terminal_bindings, 1);
        assert_eq!(control.terminal_senders.borrow().len(), 1);

        assert!(runtime.retry_now());
        assert_eq!(harness.borrow().terminal_bindings, 2);
        assert_eq!(control.terminal_senders.borrow().len(), 2);
        let context = runtime
            .selected_session_context()
            .expect("selected Session context");
        let stale = control.terminal_connection(&context);
        assert!(
            control.terminal_senders.borrow()[0]
                .try_send(Ok(stale))
                .is_err(),
            "the stale generation receiver must be cancelled"
        );

        let current = control.terminal_connection(&context);
        control.terminal_senders.borrow()[1]
            .try_send(Ok(current))
            .expect("complete current generation Terminal attachment");
        runtime.drain_at(1);

        assert!(runtime.terminal_binding().is_some());
        assert_eq!(runtime.presentation(), RuntimePresentation::Terminal);
    }

    #[test]
    fn drop_cancels_a_live_feed_before_terminal_shutdown() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        {
            let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
            runtime.drain_at(0);
            runtime.set_presentation(RuntimePresentation::Terminal);
        }
        assert_eq!(&harness.borrow().log[..2], ["feed-close", "terminal-close"]);
    }

    #[test]
    fn drop_cancels_backoff_without_opening_another_connection() {
        let harness = Rc::new(RefCell::new(Harness::default()));
        harness
            .borrow_mut()
            .incoming
            .push_back(complete("plot-a", "session-a", 0, None, 0));
        {
            let mut runtime = SessionRuntime::new(field(), connector(harness.clone()));
            runtime.drain_at(0);
            harness.borrow_mut().fail_next_prompt = true;
            assert!(matches!(
                runtime.submit_prompt("retry me"),
                SubmitOutcome::RestoreText { .. }
            ));
            assert!(matches!(
                runtime.feed_state(),
                Some(FeedState::Backoff { .. })
            ));
        }
        assert_eq!(harness.borrow().connections, 1);
        assert_eq!(
            harness.borrow().log.last().map(String::as_str),
            Some("terminal-close")
        );
    }
}
