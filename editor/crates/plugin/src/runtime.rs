//! Runs one plugin: its own WebAssembly instance on its own thread.
//!
//! The editor never waits for a plugin. Events go into a queue; the thread
//! hands them to the plugin one at a time and answers the requests it makes
//! through [`Host`], after checking the manifest's permissions.
//!
//! Work is counted in fuel, which the interpreter charges per instruction.
//! An event caused by typing gets a small first slice; a plugin that needs
//! more is paused there and continues once typing has stopped, and that is
//! counted against it. Any event that uses more than the limit is stopped
//! for good and the plugin starts again from a fresh instance.

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
    Caller, CompilationMode, Config, Engine, Extern, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, TypedFunc, TypedResumableCall,
};

use crate::Manifest;

/// What the editor does for plugins. Called on the plugin's thread.
pub trait Host: Send + Sync + 'static {
    /// Answers a request the manifest allows.
    fn request(&self, plugin: &str, request: Request) -> Reply;
    /// The plugin's [`Stats`] changed.
    fn changed(&self, _plugin: &str) {}
}

/// How much a plugin may compute, in fuel. On an Apple M4 a release build
/// runs about 1.4 million units a millisecond (`fuel_per_millisecond` in the
/// tests prints it).
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// The first slice of an event caused by typing: about 4 ms.
    pub typing: u64,
    /// Each later slice, and the first of other events: about 40 ms.
    pub slice: u64,
    /// One event's total before it is stopped: about 10 s.
    pub limit: u64,
    /// How long typing must have paused before deferred work continues.
    pub idle: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            typing: 5_000_000,
            slice: 50_000_000,
            limit: 13_000_000_000,
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
const MESSAGE_LIMIT: usize = 8 << 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub events: u64,
    /// Events caused by typing that needed more than the typing budget.
    pub over_budget: u32,
    /// Events stopped at the limit.
    pub stopped: u32,
    /// Fuel the last event used.
    pub last_fuel: u64,
    /// Wall time of the last event and of the slowest.
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
    /// Starts the plugin's thread, which compiles `wasm` and waits for
    /// events. A module that cannot run reports that in [`Stats::error`].
    pub fn start(
        manifest: Manifest,
        wasm: Vec<u8>,
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
            .spawn(move || worker.run(&wasm, rx));
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
struct State {
    /// The event, or the reply to the last request, for `read` to copy.
    pending: Vec<u8>,
    limits: StoreLimits,
    manifest: Arc<Manifest>,
    host: Arc<dyn Host>,
    stats: Arc<Mutex<Stats>>,
}

struct Instance {
    store: Store<State>,
    entry: TypedFunc<u32, ()>,
}

struct Worker {
    manifest: Arc<Manifest>,
    host: Arc<dyn Host>,
    activity: Arc<Activity>,
    budget: Budget,
    stats: Arc<Mutex<Stats>>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    fn run(self, wasm: &[u8], rx: mpsc::Receiver<Message>) {
        let mut config = Config::default();
        // Translated up front, on this thread: translating a function at its
        // first call is charged to the event's fuel, and an event that runs
        // out there fails instead of pausing.
        config
            .consume_fuel(true)
            .compilation_mode(CompilationMode::Eager);
        let engine = Engine::new(&config);
        let loaded = Module::new(&engine, wasm)
            .map_err(|e| e.to_string())
            .and_then(|module| Ok((self.instantiate(&engine, &module)?, module)));
        let (mut instance, module) = match loaded {
            Ok(loaded) => loaded,
            Err(e) => return self.fail(format!("Could not load: {e}")),
        };
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
                if let Err(error) = self.handle(&mut instance, &event) {
                    {
                        let mut stats = self.stats.lock().unwrap();
                        stats.failures += 1;
                        stats.last_failure = Some(error);
                    }
                    // Stopped halfway, its memory is in no known state.
                    match self.instantiate(&engine, &module) {
                        Ok(fresh) => instance = fresh,
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

    fn instantiate(&self, engine: &Engine, module: &Module) -> Result<Instance, String> {
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
            },
        );
        store.limiter(|state| &mut state.limits);
        let mut linker = Linker::<State>::new(engine);
        linker
            .func_wrap("solder", "read", host_read)
            .and_then(|l| l.func_wrap("solder", "call", host_call))
            .map_err(|e| e.to_string())?;
        // A start function runs under the same budget as an event.
        store
            .set_fuel(self.budget.slice)
            .map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate_and_start(&mut store, module)
            .map_err(|e| e.to_string())?;
        let entry = instance
            .get_typed_func::<u32, ()>(&store, "solder_event")
            .map_err(|_| "The module has no solder_event function".to_string())?;
        Ok(Instance { store, entry })
    }

    /// Runs one event to its end, in slices. An error leaves the instance
    /// unusable.
    fn handle(&self, instance: &mut Instance, event: &Event) -> Result<(), String> {
        let bytes = serde_json::to_vec(event).map_err(|e| e.to_string())?;
        let len = bytes.len() as u32;
        let store = &mut instance.store;
        store.data_mut().pending = bytes;
        let typing = matches!(event, Event::Change { .. });
        let started = Instant::now();
        let mut slice = if typing {
            self.budget.typing
        } else {
            self.budget.slice
        };
        let mut used = 0;
        let set_fuel =
            |store: &mut Store<State>, fuel| store.set_fuel(fuel).map_err(|e| e.to_string());
        set_fuel(store, slice)?;
        let mut call = instance
            .entry
            .call_resumable(&mut *store, len)
            .map_err(|e| format!("The plugin failed: {e}"))?;
        let outcome = loop {
            let paused = match call {
                TypedResumableCall::Finished(()) => break Ok(()),
                TypedResumableCall::HostTrap(trap) => {
                    break Err(format!("The plugin failed: {}", trap.host_error()));
                }
                TypedResumableCall::OutOfFuel(paused) => paused,
            };
            if used == 0 && typing {
                self.stats.lock().unwrap().over_budget += 1;
                self.host.changed(&self.manifest.name);
            }
            used += slice;
            if used >= self.budget.limit {
                self.stats.lock().unwrap().stopped += 1;
                break Err("Stopped: one event ran past the limit".to_string());
            }
            // What typing started waits until typing has paused.
            while typing && self.activity.idle_for() < self.budget.idle {
                if self.stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if self.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            slice = self.budget.slice.max(paused.required_fuel());
            set_fuel(store, slice)?;
            call = paused
                .resume(&mut *store)
                .map_err(|e| format!("The plugin failed: {e}"))?;
        };
        let ms = started.elapsed().as_secs_f64() * 1000.;
        let left = store.get_fuel().unwrap_or(0);
        let mut stats = self.stats.lock().unwrap();
        stats.events += 1;
        stats.last_fuel = used + slice.saturating_sub(left);
        stats.last_ms = ms;
        stats.slowest_ms = stats.slowest_ms.max(ms);
        outcome
    }
}

fn memory(caller: &Caller<'_, State>) -> Result<wasmi::Memory, wasmi::Error> {
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
    let state = caller.data();
    let reply: Reply = match serde_json::from_slice::<Request>(&bytes) {
        Err(e) => Err(format!("Not a request: {e}")),
        Ok(request) => match state.manifest.allows(&request) {
            Err(refused) => Err(refused),
            Ok(()) => match request {
                Request::Log { text } => {
                    let mut stats = state.stats.lock().unwrap();
                    stats.log.push(text);
                    if stats.log.len() > LOG_LINES {
                        let extra = stats.log.len() - LOG_LINES;
                        stats.log.drain(..extra);
                    }
                    Ok(serde_json::Value::Null)
                }
                request => state.host.request(&state.manifest.name, request),
            },
        },
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
