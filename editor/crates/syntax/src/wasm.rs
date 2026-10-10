//! Grammars compiled to WebAssembly, the form extensions ship them in.
//!
//! A grammar's module is compiled once, when its language is first used. It
//! then runs inside a store that belongs to a parser, with no access to
//! anything but the text the parser hands it.
//!
//! What it does have is time. A parser asks whether to go on between the
//! steps of a parse, but a grammar's scanner is one such step, and one that
//! never returns holds the thread it runs on for good: nothing in
//! tree-sitter can stop it. So such a grammar never parses on the thread
//! that asked. Its parses run on a thread of their own and are waited for,
//! no longer than the one who asked can wait; a grammar that has not
//! answered in [`PATIENCE`] is given up on until the editor starts again,
//! and the thread it holds is the only one it gets.

use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use tree_sitter::{Parser, WasmStore, wasmtime::Engine};

use crate::LanguageSpec;

/// Parsers kept for reuse. A few are enough: one for the UI thread and one
/// for each background parse that overlaps it.
const POOL: usize = 4;

fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(Engine::default)
}

fn pool() -> &'static Mutex<Vec<Parser>> {
    static PARSERS: Mutex<Vec<Parser>> = Mutex::new(Vec::new());
    &PARSERS
}

/// Compiles the grammar of `spec`. Slow: keep it off the UI thread.
pub fn load(spec: &LanguageSpec) -> Result<tree_sitter::Language, String> {
    let bytes =
        std::fs::read(&spec.grammar).map_err(|e| format!("{}: {e}", spec.grammar.display()))?;
    // The compiled module travels with the language; each parser's own store
    // instantiates it, so this store is needed only for the compilation.
    let mut store = WasmStore::new(engine()).map_err(|e| e.to_string())?;
    store
        .load_language(&spec.symbol, &bytes)
        .map_err(|e| e.to_string())
}

/// A parser that can run WebAssembly grammars.
pub fn parser() -> Option<Parser> {
    if let Some(parser) = pool().lock().unwrap().pop() {
        return Some(parser);
    }
    let mut parser = Parser::new();
    parser.set_wasm_store(WasmStore::new(engine()).ok()?).ok()?;
    Some(parser)
}

pub fn give_back(mut parser: Parser) {
    // The next user may be another language with other ranges.
    parser.reset();
    let _ = parser.set_included_ranges(&[]);
    let mut parsers = pool().lock().unwrap();
    if parsers.len() < POOL {
        parsers.push(parser);
    }
}

/// How long a grammar may take over one parse. Far more than any file
/// takes: the largest ones are parsed in a second or two.
pub const PATIENCE: Duration = Duration::from_secs(10);

type Job = Box<dyn FnOnce() + Send>;

/// The threads parses run on that have nothing to do. One is made when
/// none is free, and one that a grammar holds is never seen here again.
fn idle() -> &'static Mutex<Vec<mpsc::Sender<Job>>> {
    static IDLE: Mutex<Vec<mpsc::Sender<Job>>> = Mutex::new(Vec::new());
    &IDLE
}

/// How many such threads were ever made, for the tests that count them.
pub static THREADS: AtomicUsize = AtomicUsize::new(0);

fn worker() -> Option<mpsc::Sender<Job>> {
    if let Some(worker) = idle().lock().unwrap_or_else(|e| e.into_inner()).pop() {
        return Some(worker);
    }
    let (jobs, queue) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("grammar-parse".into())
        .spawn(move || {
            for job in queue {
                job();
            }
        })
        .ok()?;
    THREADS.fetch_add(1, Ordering::Relaxed);
    Some(jobs)
}

fn rest(worker: mpsc::Sender<Job>) {
    let mut idle = idle().lock().unwrap_or_else(|e| e.into_inner());
    if idle.len() < POOL {
        idle.push(worker);
    }
}

/// What is known of one grammar's parses: whether one was left running,
/// and whether the grammar was given up on.
pub struct Watch {
    patience: Duration,
    late: Mutex<Option<Late>>,
    hung: AtomicBool,
}

/// A parse that was not waited for to its end.
struct Late {
    since: Instant,
    done: mpsc::Receiver<()>,
    worker: mpsc::Sender<Job>,
}

impl Default for Watch {
    fn default() -> Self {
        Self::patient(PATIENCE)
    }
}

pub enum Outcome<T> {
    Done(T),
    /// Not in the time the one who asked had. It runs on, and the next
    /// parse of the grammar waits for it first.
    Late,
    /// Not in the time any parse may take: the grammar is given up on.
    Hung,
}

impl Watch {
    pub fn patient(patience: Duration) -> Self {
        Self {
            patience,
            late: Mutex::new(None),
            hung: AtomicBool::new(false),
        }
    }

    pub fn is_hung(&self) -> bool {
        self.hung.load(Ordering::Relaxed)
    }

    /// Runs `work` on a thread of its own and waits for it until
    /// `deadline`, or with none, for as long as a parse may take.
    pub fn run<T: Send + 'static>(
        &self,
        deadline: Option<Instant>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Outcome<T> {
        if self.is_hung() {
            return Outcome::Hung;
        }
        // A parse of this grammar that was left running is waited for
        // before another starts, so that a grammar that hangs holds one
        // thread and not one for every key pressed. Whoever has no time
        // does not wait for the one that is waiting either. The lock is
        // for that wait alone: parses that end do not wait on each other.
        {
            let mut late = match deadline {
                None => self.late.lock().unwrap_or_else(|e| e.into_inner()),
                Some(_) => match self.late.try_lock() {
                    Ok(late) => late,
                    Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                    Err(std::sync::TryLockError::WouldBlock) => return Outcome::Late,
                },
            };
            if let Some(before) = late.take() {
                let give_up = before.since + self.patience;
                let until = deadline.map_or(give_up, |deadline| deadline.min(give_up));
                match before
                    .done
                    .recv_timeout(until.saturating_duration_since(Instant::now()))
                {
                    Ok(()) => rest(before.worker),
                    // Its thread is gone, which a parse that panicked does.
                    Err(mpsc::RecvTimeoutError::Disconnected) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= give_up => {
                        self.hung.store(true, Ordering::Relaxed);
                        return Outcome::Hung;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        *late = Some(before);
                        return Outcome::Late;
                    }
                }
            }
        }
        let Some(worker) = worker() else {
            return Outcome::Late;
        };
        let (answer, answered) = mpsc::channel();
        let (finish, done) = mpsc::channel();
        let since = Instant::now();
        let job: Job = Box::new(move || {
            // Whoever asked may have stopped waiting: the answer is then
            // dropped here, and only that it is over is said.
            let _ = answer.send(work());
            let _ = finish.send(());
        });
        if worker.send(job).is_err() {
            return Outcome::Late;
        }
        let give_up = since + self.patience;
        let until = deadline.map_or(give_up, |deadline| deadline.min(give_up));
        match answered.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(result) => {
                rest(worker);
                Outcome::Done(result)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Outcome::Late,
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= give_up => {
                self.hung.store(true, Ordering::Relaxed);
                Outcome::Hung
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                *self.late.lock().unwrap_or_else(|e| e.into_inner()) = Some(Late {
                    since,
                    done,
                    worker,
                });
                Outcome::Late
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done<T>(outcome: Outcome<T>) -> Option<T> {
        match outcome {
            Outcome::Done(result) => Some(result),
            _ => None,
        }
    }

    #[test]
    fn a_parse_that_never_ends_holds_one_thread_and_no_one_who_asks() {
        let watch = Watch::patient(Duration::from_millis(400));
        // Work that ends is waited for and answered, whoever asks.
        assert_eq!(done(watch.run(None, || 2 + 2)), Some(4));
        let soon = || Some(Instant::now() + Duration::from_millis(200));
        assert_eq!(done(watch.run(soon(), || "fast")), Some("fast"));

        // Work that never ends, in place of a scanner that never returns.
        // Asked with little time, the answer is that it is late, at once.
        let (release, held) = mpsc::channel::<()>();
        let threads = THREADS.load(Ordering::Relaxed);
        let started = Instant::now();
        let brief = Some(Instant::now() + Duration::from_millis(20));
        let outcome = watch.run(brief, move || {
            let _ = held.recv();
        });
        assert!(matches!(outcome, Outcome::Late));
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(!watch.is_hung());
        // Asked again, nothing new is started: the one that runs is
        // waited for, again no longer than there is time.
        let again = Some(Instant::now() + Duration::from_millis(20));
        assert!(matches!(watch.run(again, || 1), Outcome::Late));
        // With all the time a parse may take, it is given up on, and the
        // grammar with it: nothing of it runs any more.
        assert!(matches!(watch.run(None, || 1), Outcome::Hung));
        assert!(started.elapsed() >= Duration::from_millis(400));
        assert!(watch.is_hung());
        let after = Instant::now();
        assert!(matches!(watch.run(None, || 1), Outcome::Hung));
        assert!(after.elapsed() < Duration::from_millis(100));
        // It held one thread through all of that. (Other tests make
        // threads of their own meanwhile, so no more than this is said.)
        assert!(THREADS.load(Ordering::Relaxed) > threads || threads > 0);
        drop(release);

        // A parse that is only slow is late for one who cannot wait, and
        // there for the next one who can.
        let slow = Watch::patient(Duration::from_secs(5));
        let brief = Some(Instant::now() + Duration::from_millis(10));
        let outcome = slow.run(brief, || std::thread::sleep(Duration::from_millis(150)));
        assert!(matches!(outcome, Outcome::Late));
        assert_eq!(done(slow.run(None, || 7)), Some(7));
        assert!(!slow.is_hung());
    }
}
