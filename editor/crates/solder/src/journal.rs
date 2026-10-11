//! The file errors are written to, and what a crash leaves for the next
//! start.
//!
//! Both stay on this machine, in `logs` of the settings folder. An error is
//! a line of `solder.log`, which is put aside when it has grown to a
//! megabyte, three old ones kept. A panic writes a report of its own, a
//! file a crash, before the process goes: the next start finds it and
//! says so once.

use std::{
    fmt::Display,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{OnceLock, mpsc},
    time::{SystemTime, UNIX_EPOCH},
};

/// The log is put aside when it is this long.
const LIMIT: u64 = 1024 * 1024;
/// How many logs put aside are kept.
const KEPT: usize = 3;
/// How many reports that were looked at are kept.
const SEEN: usize = 5;
const NAME: &str = "solder.log";

/// Where the lines go. Empty until `start`, and always in tests, which
/// must leave the user's own log alone.
static WRITER: OnceLock<mpsc::Sender<String>> = OnceLock::new();

pub fn dir() -> PathBuf {
    crate::settings::config_dir().join("logs")
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(NAME)
}

/// Errors go to the file from here on, and a panic leaves a report.
/// The thread that writes only waits: a start that goes well opens no
/// file and makes no folder.
pub fn start(dir: PathBuf) {
    let previous = std::panic::take_hook();
    let crashes = dir.clone();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let report = report(
            info.payload_as_str().unwrap_or("a panic with no message"),
            info.location()
                .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column())),
            thread.name().unwrap_or("unnamed"),
            &std::backtrace::Backtrace::force_capture().to_string(),
        );
        // The process may end with this thread: written here, not sent.
        keep_crash(&crashes, &report);
        previous(info);
    }));
    let (send, receive) = mpsc::channel::<String>();
    if WRITER.set(send).is_ok() {
        std::thread::Builder::new()
            .name("journal".into())
            .spawn(move || {
                for line in receive {
                    append(&dir, &line);
                }
            })
            .ok();
    }
}

/// Says an error: on the terminal Solder was started from, if there is
/// one, and in the log.
pub fn error(text: impl Display) {
    let text = text.to_string();
    eprintln!("{text}");
    if let Some(writer) = WRITER.get() {
        let _ = writer.send(format!("{} error: {text}", stamp(SystemTime::now())));
    }
}

/// One more line of the log, the log put aside first if it is full.
fn append(dir: &Path, line: &str) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    // A line that held line breaks stays one entry: what follows the
    // first is set in.
    let entry = format!("{}\n", line.trim_end().replace('\n', "\n    "));
    let path = path(dir);
    if fs::metadata(&path).is_ok_and(|file| file.len() + entry.len() as u64 > LIMIT) {
        rotate(dir);
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(entry.as_bytes());
    }
}

fn aside(dir: &Path, number: usize) -> PathBuf {
    dir.join(format!("solder.{number}.log"))
}

fn rotate(dir: &Path) {
    let _ = fs::remove_file(aside(dir, KEPT));
    for number in (1..KEPT).rev() {
        let _ = fs::rename(aside(dir, number), aside(dir, number + 1));
    }
    let _ = fs::rename(path(dir), aside(dir, 1));
}

fn report(message: &str, at: Option<String>, thread: &str, trace: &str) -> String {
    format!(
        "Solder {} on {} {}\n{}\n\nThread: {thread}\nAt: {}\n\n{message}\n\n{trace}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        stamp(SystemTime::now()),
        at.as_deref().unwrap_or("an unknown place"),
    )
}

fn keep_crash(dir: &Path, report: &str) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // Two threads may panic in the same second, and so may two Solders.
    let name = format!(
        "crash-{}-{}-{:09}.txt",
        now.as_secs(),
        std::process::id(),
        now.subsec_nanos()
    );
    let _ = fs::write(dir.join(name), report);
    let first = report.lines().nth(6).unwrap_or_default();
    append(dir, &format!("{} crash: {first}", stamp(SystemTime::now())));
}

/// The reports nobody has looked at yet, the oldest first.
pub fn crashes(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("crash-") && name.ends_with(".txt"))
        })
        .collect();
    found.sort();
    found
}

/// Puts a report with the ones that were looked at, where it is said no
/// more, and says where it is now. The oldest of those are let go.
pub fn seen(report: &Path) -> Option<PathBuf> {
    let dir = report.parent()?.join("seen");
    fs::create_dir_all(&dir).ok()?;
    let kept = dir.join(report.file_name()?);
    fs::rename(report, &kept).ok()?;
    let mut all = crashes(&dir);
    while all.len() > SEEN {
        let _ = fs::remove_file(all.remove(0));
    }
    Some(kept)
}

/// A moment as `2026-10-11 09:30:00Z`, with no calendar but this one.
fn stamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Days since 1970 to a date: Howard Hinnant's `civil_from_days`.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_moment_is_written_as_a_date() {
        let at = |seconds| stamp(UNIX_EPOCH + Duration::from_secs(seconds));
        assert_eq!(at(0), "1970-01-01 00:00:00Z");
        assert_eq!(at(951_782_399), "2000-02-28 23:59:59Z");
        assert_eq!(at(951_782_400), "2000-02-29 00:00:00Z");
        assert_eq!(at(1_791_711_000), "2026-10-11 09:30:00Z");
    }

    #[test]
    fn a_full_log_is_put_aside_and_only_a_few_are_kept() {
        let dir = db::testing::dir("journal-rotation");
        let line = "x".repeat(64 * 1024 - 1);
        // Enough for the log to fill six times over.
        for _ in 0..16 * 6 {
            append(&dir, &line);
        }
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["solder.1.log", "solder.2.log", "solder.3.log", "solder.log"]
        );
        for name in names {
            assert!(fs::metadata(dir.join(name)).unwrap().len() <= LIMIT);
        }
    }

    #[test]
    fn an_error_of_several_lines_is_one_entry() {
        let dir = db::testing::dir("journal-lines");
        append(&dir, "first\nsecond\n");
        append(&dir, "third");
        assert_eq!(
            fs::read_to_string(path(&dir)).unwrap(),
            "first\n    second\nthird\n"
        );
    }

    #[test]
    fn a_report_is_found_once_and_old_ones_are_let_go() {
        let dir = db::testing::dir("journal-crashes");
        for number in 0..SEEN + 2 {
            fs::write(dir.join(format!("crash-{number}-1-000000000.txt")), "it").unwrap();
        }
        fs::write(dir.join("solder.log"), "not a report").unwrap();
        let found = crashes(&dir);
        assert_eq!(found.len(), SEEN + 2);
        assert!(found[0].ends_with("crash-0-1-000000000.txt"));
        for report in &found {
            let kept = seen(report).unwrap();
            assert!(kept.starts_with(dir.join("seen")));
        }
        assert!(crashes(&dir).is_empty());
        let kept = crashes(&dir.join("seen"));
        assert_eq!(kept.len(), SEEN);
        assert!(kept[0].ends_with("crash-2-1-000000000.txt"));
    }

    /// A panic in a process of its own: this very test, run again with a
    /// folder to leave the report in. The hook is the process's, so it is
    /// not set in the one the other tests run in.
    #[test]
    fn a_panic_leaves_a_report_and_a_line_of_the_log() {
        const DIR: &str = "SOLDER_TEST_JOURNAL";
        if let Some(dir) = std::env::var_os(DIR) {
            start(PathBuf::from(dir));
            error("said before the end");
            // The line is the writer's to write: let it.
            std::thread::sleep(Duration::from_millis(200));
            panic!("the reason it ended");
        }
        let dir = db::testing::dir("journal-panic");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "journal::tests::a_panic_leaves_a_report_and_a_line_of_the_log",
                "--nocapture",
            ])
            .env(DIR, &dir)
            .output()
            .unwrap();
        assert!(!child.status.success());
        let found = crashes(&dir);
        assert_eq!(found.len(), 1, "{child:?}");
        let report = fs::read_to_string(&found[0]).unwrap();
        assert!(report.starts_with(&format!("Solder {}", env!("CARGO_PKG_VERSION"))));
        assert!(report.contains("the reason it ended"), "{report}");
        assert!(report.contains("journal.rs"), "{report}");
        let log = fs::read_to_string(path(&dir)).unwrap();
        assert!(log.contains(" error: said before the end\n"), "{log}");
        assert!(log.contains(" crash: the reason it ended\n"), "{log}");
    }
}
