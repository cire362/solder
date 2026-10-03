//! Runs one plugin: its own WebAssembly instance on its own thread.
//!
//! The editor never waits for a plugin. Events go into a queue; the thread
//! hands them to the plugin one at a time and answers the requests it makes
//! through [`Host`], after checking the manifest's permissions.
//!
//! A plugin's work is measured in time. The interpreter can only be paused
//! by running it out of fuel, so it runs in slices of fuel sized to last
//! about a millisecond each, and the clock is read between them. An event
//! caused by typing that needs more than the typing budget is paused there
//! and continues once typing has stopped, which is counted against the
//! plugin. Any event that works longer than the limit is stopped for good
//! and the plugin starts again from a fresh instance.
//!
//! Fuel itself is not a measure: a unit is worth a hundred times less time
//! in the JavaScript engine's dispatch loop than in a plugin compiled from
//! Rust.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use solder_plugin::{Event, Reply, Request};
use wasmi::{
    Caller, CompilationMode, Config, Engine, Extern, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, TypedFunc, TypedResumableCall, TypedResumableCallHostTrap,
    TypedResumableCallOutOfFuel, Val,
};

use crate::{Manifest, script};

/// What the editor does for plugins. Called on the plugin's thread.
pub trait Host: Send + Sync + 'static {
    /// Answers a request the manifest allows.
    fn request(&self, plugin: &str, request: Request) -> Reply;
    /// The plugin's [`Stats`] changed.
    fn changed(&self, _plugin: &str) {}
}

/// What a plugin is made of.
#[derive(Clone, Debug, PartialEq)]
pub enum Code {
    /// `plugin.wasm`: a module that exports `solder_event`.
    Module(Vec<u8>),
    /// `plugin.js`: a script, run by the JavaScript engine built in.
    Script(String),
}

/// How long a plugin may compute. Time it spends waiting for the editor's
/// answers does not count.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// What an event caused by typing gets before it is deferred.
    pub typing: Duration,
    /// One event's total before it is stopped.
    pub limit: Duration,
    /// How long typing must have paused before deferred work continues.
    pub idle: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            typing: Duration::from_millis(4),
            limit: Duration::from_secs(10),
            idle: Duration::from_millis(150),
        }
    }
}

/// When the user last typed, shared by every plugin.
pub struct Activity {
    start: Instant,
    last_ms: AtomicU64,
}

impl Default for Activity {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            // Idle from the start.
            last_ms: AtomicU64::new(0),
        }
    }
}

impl Activity {
    /// The user typed just now.
    pub fn touch(&self) {
        let now = self.start.elapsed().as_millis() as u64;
        self.last_ms.store(now.max(1), Ordering::Relaxed);
    }

    fn idle_for(&self) -> Duration {
        match self.last_ms.load(Ordering::Relaxed) {
            0 => Duration::MAX,
            last => self
                .start
                .elapsed()
                .saturating_sub(Duration::from_millis(last)),
        }
    }
}

/// Over the typing budget this many times, a plugin is marked slow.
pub const SLOW_AFTER: u32 = 3;

const LOG_LINES: usize = 200;
/// A plugin's memory: 64 MiB.
const MEMORY_LIMIT: usize = 64 << 20;
/// One request or event: 8 MiB.
pub(crate) const MESSAGE_LIMIT: usize = 8 << 20;

/// The fuel of the first slice; later ones are sized by how long the last
/// one took.
const FIRST_SLICE: u64 = 1_000_000;
/// How long a script's engine may take to start and run the script's top
/// level. Not an event: the limit of one, which a user can see spent on a
/// slow machine before anything ran, does not apply.
const LOAD_LIMIT: Duration = Duration::from_secs(30);
const SLICE: std::ops::Range<Duration> = Duration::from_micros(500)..Duration::from_millis(2);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub events: u64,
    /// Events caused by typing that needed more than the typing budget.
    pub over_budget: u32,
    /// Events stopped at the limit.
    pub stopped: u32,
    /// How long the last event computed, and the longest one.
    pub last_ms: f64,
    pub slowest_ms: f64,
    /// Why the plugin is not running.
    pub error: Option<String>,
    /// Events that ended in a failure, and what the last one died of.
    pub failures: u32,
    pub last_failure: Option<String>,
    pub log: Vec<String>,
}

impl Stats {
    /// A repeat offender, to be marked in the Plugins window.
    pub fn slow(&self) -> bool {
        self.over_budget >= SLOW_AFTER || self.stopped > 0
    }
}

enum Message {
    Event(Event),
    Stop,
}

/// A running plugin. Dropping it stops the plugin.
pub struct Plugin {
    name: String,
    tx: mpsc::Sender<Message>,
    stats: Arc<Mutex<Stats>>,
    stop: Arc<AtomicBool>,
}

impl Plugin {
    /// Starts the plugin's thread, which loads `code` and waits for events.
    /// Code that cannot run reports that in [`Stats::error`].
    pub fn start(
        manifest: Manifest,
        code: Code,
        host: Arc<dyn Host>,
        activity: Arc<Activity>,
        budget: Budget,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let stats = Arc::new(Mutex::new(Stats::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let name = manifest.name.clone();
        let worker = Worker {
            manifest: Arc::new(manifest),
            host,
            activity,
            budget,
            stats: stats.clone(),
            stop: stop.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name(format!("plugin-{name}"))
            .spawn(move || worker.run(code, rx));
        if let Err(e) = spawned {
            stats.lock().unwrap().error = Some(format!("Could not start: {e}"));
        }
        Self {
            name,
            tx,
            stats,
            stop,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Queues an event; never blocks.
    pub fn send(&self, event: Event) {
        let _ = self.tx.send(Message::Event(event));
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        // The flag ends work in progress at its next slice.
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Message::Stop);
    }
}

/// What the plugin's instance can reach.
pub(crate) struct State {
    /// The event, or the reply to the last request, for `read` to copy.
    pending: Vec<u8>,
    limits: StoreLimits,
    pub(crate) manifest: Arc<Manifest>,
    pub(crate) host: Arc<dyn Host>,
    pub(crate) stats: Arc<Mutex<Stats>>,
    /// Time spent in the editor answering requests during this slice.
    pub(crate) answering: Duration,
    /// A script's side of the conversation.
    pub(crate) script: script::Io,
}

impl State {
    pub(crate) fn log(&self, text: String) {
        let mut stats = self.stats.lock().unwrap();
        stats.log.push(text);
        if stats.log.len() > LOG_LINES {
            let extra = stats.log.len() - LOG_LINES;
            stats.log.drain(..extra);
        }
    }

    /// Answers one request: the log is kept here, the rest goes to the
    /// editor if the manifest allows it.
    pub(crate) fn answer(&mut self, request: Request) -> Reply {
        self.manifest.allows(&request)?;
        match request {
            Request::Log { text } => {
                self.log(text);
                Ok(serde_json::Value::Null)
            }
            request => {
                let asked = Instant::now();
                let reply = self.host.request(&self.manifest.name, request);
                self.answering += asked.elapsed();
                reply
            }
        }
    }
}

/// A loaded plugin, between events.
struct Guest {
    store: Store<State>,
    kind: Kind,
    /// Fuel that lasts about a millisecond in this plugin's code.
    slice: u64,
}

enum Kind {
    /// Called once for each event.
    Module { entry: TypedFunc<u32, ()> },
    /// One long call that reads events as lines; between events it is
    /// parked in the read.
    Script {
        memory: Memory,
        parked: Option<TypedResumableCallHostTrap<()>>,
    },
}

/// A call that ran out of fuel, to be continued.
type Paused = TypedResumableCallOutOfFuel<()>;

struct Worker {
    manifest: Arc<Manifest>,
    host: Arc<dyn Host>,
    activity: Arc<Activity>,
    budget: Budget,
    stats: Arc<Mutex<Stats>>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    fn run(self, code: Code, rx: mpsc::Receiver<Message>) {
        let mut config = Config::default();
        // Translated up front, on this thread: translating a function at its
        // first call is charged to the fuel of the slice, and a call that
        // runs out there fails instead of pausing.
        config
            .consume_fuel(true)
            .compilation_mode(CompilationMode::Eager);
        let engine = Engine::new(&config);
        let wasm: &[u8] = match &code {
            Code::Module(wasm) => wasm,
            Code::Script(_) => script::ENGINE,
        };
        let loaded = Module::new(&engine, wasm)
            .map_err(|e| e.to_string())
            .and_then(|module| Ok((self.instantiate(&engine, &module, &code)?, module)));
        let (mut guest, module) = match loaded {
            Ok(loaded) => loaded,
            Err(e) => return self.fail(format!("Could not load: {e}")),
        };
        self.host.changed(&self.manifest.name);
        let mut queue: Vec<Event> = Vec::new();
        loop {
            match rx.recv() {
                Ok(Message::Event(event)) => queue.push(event),
                Ok(Message::Stop) | Err(_) => return,
            }
            // Everything that arrived meanwhile, so a burst of typing is
            // one change per file.
            for message in rx.try_iter() {
                match message {
                    Message::Event(event) => {
                        if !matches!(event, Event::Change { .. }) || !queue.contains(&event) {
                            queue.push(event);
                        }
                    }
                    Message::Stop => return,
                }
            }
            for event in queue.drain(..) {
                if self.stop.load(Ordering::Relaxed) {
                    return;
                }
                if !self.manifest.wants(&event) {
                    continue;
                }
                let typing = matches!(event, Event::Change { .. });
                let handled = serde_json::to_vec(&event)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| self.drive(&mut guest, bytes, typing));
                if let Err(error) = handled {
                    {
                        let mut stats = self.stats.lock().unwrap();
                        stats.failures += 1;
                        stats.last_failure = Some(error);
                    }
                    // Stopped halfway, its memory is in no known state.
                    match self.instantiate(&engine, &module, &code) {
                        Ok(fresh) => guest = fresh,
                        Err(e) => return self.fail(format!("Could not restart: {e}")),
                    }
                }
                self.host.changed(&self.manifest.name);
            }
        }
    }

    fn fail(&self, error: String) {
        self.stats.lock().unwrap().error = Some(error);
        self.host.changed(&self.manifest.name);
    }

    fn instantiate(&self, engine: &Engine, module: &Module, code: &Code) -> Result<Guest, String> {
        let source = match code {
            Code::Script(source) => Some(source.as_str()),
            Code::Module(_) => None,
        };
        let mut store = Store::new(
            engine,
            State {
                pending: Vec::new(),
                limits: StoreLimitsBuilder::new()
                    .memory_size(MEMORY_LIMIT)
                    .instances(1)
                    .memories(1)
                    .build(),
                manifest: self.manifest.clone(),
                host: self.host.clone(),
                stats: self.stats.clone(),
                answering: Duration::ZERO,
                script: script::Io::new(source),
            },
        );
        store.limiter(|state| &mut state.limits);
        let mut linker = Linker::<State>::new(engine);
        linker
            .func_wrap("solder", "read", host_read)
            .and_then(|l| l.func_wrap("solder", "call", host_call))
            .map_err(|e| e.to_string())?;
        script::link(&mut linker).map_err(|e| e.to_string())?;
        // Enough for a module's start function, which is not an event.
        store
            .set_fuel(FIRST_SLICE * 50)
            .map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate_and_start(&mut store, module)
            .map_err(|e| e.to_string())?;
        if source.is_none() {
            let entry = instance
                .get_typed_func::<u32, ()>(&store, "solder_event")
                .map_err(|_| "The module has no solder_event function".to_string())?;
            return Ok(Guest {
                store,
                kind: Kind::Module { entry },
                slice: FIRST_SLICE,
            });
        }
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or("The engine exports no memory")?;
        let mut guest = Guest {
            store,
            kind: Kind::Script {
                memory,
                parked: None,
            },
            slice: FIRST_SLICE,
        };
        // The engine starts and runs the script up to where it waits for
        // its first event.
        let start = instance
            .get_typed_func::<(), ()>(&guest.store, "_start")
            .map_err(|e| e.to_string())?;
        self.drive_call(&mut guest, false, LOAD_LIMIT, |store| {
            start.call_resumable(store, ())
        })?;
        Ok(guest)
    }

    /// Hands the plugin one event and runs it to its end. An error leaves
    /// the guest unusable.
    fn drive(&self, guest: &mut Guest, bytes: Vec<u8>, typing: bool) -> Result<(), String> {
        let ms = match &mut guest.kind {
            Kind::Module { entry } => {
                let (entry, len) = (*entry, bytes.len() as u32);
                guest.store.data_mut().pending = bytes;
                self.drive_call(guest, typing, self.budget.limit, |store| {
                    entry.call_resumable(store, len)
                })?
            }
            Kind::Script { memory, parked } => {
                let (memory, parked) = (*memory, parked.take());
                let parked = parked.ok_or("The script is not waiting for an event")?;
                guest.store.data_mut().script.push_line(&bytes);
                // The read it was parked in now has something to return.
                script::finish_read(&mut guest.store, memory).map_err(|e| e.to_string())?;
                self.drive_call(guest, typing, self.budget.limit, |store| {
                    parked.resume(store, &[Val::I32(0)])
                })?
            }
        };
        // A handler that threw: the script lives on, the event failed.
        let thrown = guest.store.data_mut().script.take_failure();
        let mut stats = self.stats.lock().unwrap();
        stats.events += 1;
        stats.last_ms = ms;
        stats.slowest_ms = stats.slowest_ms.max(ms);
        if let Some(thrown) = thrown {
            stats.failures += 1;
            stats.last_failure = Some(thrown);
        }
        Ok(())
    }

    /// Runs the call `begin` makes until the plugin is done with it, in
    /// slices, under the budget. Returns how long it computed.
    fn drive_call(
        &self,
        guest: &mut Guest,
        typing: bool,
        limit: Duration,
        begin: impl FnOnce(&mut Store<State>) -> Result<TypedResumableCall<()>, wasmi::Error>,
    ) -> Result<f64, String> {
        let failed = |e: wasmi::Error| format!("The plugin failed: {e}");
        // A script that stopped says why itself.
        let ended = |guest: &Guest, e: String| match guest.kind {
            Kind::Script { .. } => guest.store.data().script.ended(),
            Kind::Module { .. } => e,
        };
        let mut begin = Some(begin);
        let mut paused: Option<Paused> = None;
        let mut spent = Duration::ZERO;
        let mut over = false;
        loop {
            let store = &mut guest.store;
            store.set_fuel(guest.slice).map_err(failed)?;
            store.data_mut().answering = Duration::ZERO;
            let started = Instant::now();
            let call = match (begin.take(), paused.take()) {
                (Some(begin), _) => begin(store),
                (None, Some(paused)) => paused.resume(&mut *store),
                (None, None) => unreachable!("a call is begun or paused"),
            };
            let took = started.elapsed().saturating_sub(store.data().answering);
            let call = match call {
                Ok(call) => call,
                Err(e) => return Err(ended(guest, failed(e))),
            };
            spent += took;
            // The next slice should last about a millisecond.
            if took < SLICE.start {
                guest.slice = guest.slice.saturating_mul(2).min(u64::MAX / 4);
            } else if took > SLICE.end {
                guest.slice = (guest.slice / 2).max(FIRST_SLICE / 16);
            }
            let late = typing && !over && spent > self.budget.typing;
            if late {
                over = true;
                self.stats.lock().unwrap().over_budget += 1;
                self.host.changed(&self.manifest.name);
            }
            match call {
                TypedResumableCall::Finished(()) => match guest.kind {
                    Kind::Module { .. } => break,
                    Kind::Script { .. } => return Err(ended(guest, String::new())),
                },
                TypedResumableCall::HostTrap(trap) => {
                    // A script waiting for its next event is done with
                    // this one.
                    if let Kind::Script { parked, .. } = &mut guest.kind
                        && guest.store.data().script.waiting()
                    {
                        *parked = Some(trap);
                        break;
                    }
                    let error = format!("The plugin failed: {}", trap.host_error());
                    return Err(ended(guest, error));
                }
                TypedResumableCall::OutOfFuel(more) => {
                    if spent > limit {
                        self.stats.lock().unwrap().stopped += 1;
                        return Err("Stopped: one event ran past the limit".to_string());
                    }
                    // What typing started waits until typing has paused.
                    while over && self.activity.idle_for() < self.budget.idle {
                        if self.stop.load(Ordering::Relaxed) {
                            return Err("Stopped".into());
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if self.stop.load(Ordering::Relaxed) {
                        return Err("Stopped".into());
                    }
                    guest.slice = guest.slice.max(more.required_fuel());
                    paused = Some(more);
                }
            }
        }
        Ok(spent.as_secs_f64() * 1000.)
    }
}

pub(crate) fn memory(caller: &Caller<'_, State>) -> Result<Memory, wasmi::Error> {
    match caller.get_export("memory") {
        Some(Extern::Memory(memory)) => Ok(memory),
        _ => Err(wasmi::Error::new("the module exports no memory")),
    }
}

/// `solder.read(ptr, len)`: copies what is waiting into the plugin.
fn host_read(mut caller: Caller<'_, State>, ptr: u32, len: u32) -> Result<(), wasmi::Error> {
    let memory = memory(&caller)?;
    let pending = std::mem::take(&mut caller.data_mut().pending);
    if pending.len() != len as usize {
        return Err(wasmi::Error::new("read: nothing of that length is waiting"));
    }
    memory
        .write(&mut caller, ptr as usize, &pending)
        .map_err(|_| wasmi::Error::new("read: outside the plugin's memory"))
}

/// `solder.call(ptr, len) -> len`: a request; its reply waits for `read`.
fn host_call(mut caller: Caller<'_, State>, ptr: u32, len: u32) -> Result<u32, wasmi::Error> {
    if len as usize > MESSAGE_LIMIT {
        return Err(wasmi::Error::new("call: the request is too large"));
    }
    let memory = memory(&caller)?;
    let mut bytes = vec![0; len as usize];
    memory
        .read(&caller, ptr as usize, &mut bytes)
        .map_err(|_| wasmi::Error::new("call: outside the plugin's memory"))?;
    let reply: Reply = match serde_json::from_slice::<Request>(&bytes) {
        Err(e) => Err(format!("Not a request: {e}")),
        Ok(request) => caller.data_mut().answer(request),
    };
    let mut reply = serde_json::to_vec(&reply).unwrap_or_default();
    if reply.len() > MESSAGE_LIMIT {
        reply =
            serde_json::to_vec(&Reply::Err("The answer is too large".into())).unwrap_or_default();
    }
    let len = reply.len() as u32;
    caller.data_mut().pending = reply;
    Ok(len)
}
