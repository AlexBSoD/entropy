//! A CSV log of the connected keyboard's halves, for measuring how fast they
//! drain: `$XDG_STATE_HOME/entropy/battery/<keyboard id>.csv`, one per keyboard.
//!
//! The rows come from the battery reads Entropy already makes: one when the
//! keyboard connects, then one every [`LOG_EVERY`]. A half with no reading
//! leaves its cell empty, and a row with only the time marks where the keyboard
//! went away or Entropy exited, so a plot breaks the line there instead of
//! drawing it straight across the gap.
//!
//! `ENTROPY_BATTERY_LOG=0` leaves it off, `ENTROPY_BATTERY_LOG_DIR` moves it.

use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::hid::BatteryHalves;

const HEADER: &str = "time,left,right";

/// Entropy reads the halves every 5 min once both report, and every 10 s
/// while one does not; logging those would only repeat rows.
const LOG_EVERY: Duration = Duration::from_secs(300);

/// The keyboard being logged and its last row.
struct Logged {
    keyboard_id: u64,
    path: PathBuf,
    at: Instant,
    halves: BatteryHalves,
}

struct Log {
    root: PathBuf,
    current: Option<Logged>,
}

impl Log {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            current: None,
        }
    }

    fn path(&self, keyboard_id: u64) -> PathBuf {
        self.root.join(format!("{keyboard_id:016x}.csv"))
    }

    /// Writes a row for the first read of a keyboard, then one every
    /// [`LOG_EVERY`]; `true` when it did. Another keyboard breaks the log of
    /// the one before.
    fn record(&mut self, keyboard_id: u64, halves: BatteryHalves, now: Instant) -> Result<bool> {
        if self
            .current
            .as_ref()
            .is_some_and(|logged| logged.keyboard_id != keyboard_id)
        {
            self.gap()?;
        }
        let path = match &self.current {
            Some(logged) if now.duration_since(logged.at) < LOG_EVERY => return Ok(false),
            Some(logged) => {
                if logged.halves != halves {
                    log::info!("battery {keyboard_id:016x}: {}", show(halves));
                }
                logged.path.clone()
            }
            None => {
                let path = self.path(keyboard_id);
                log::info!(
                    "battery {keyboard_id:016x}: {}, logging to {}",
                    show(halves),
                    path.display()
                );
                path
            }
        };
        let row = format!(
            "{},{},{}",
            timestamp(),
            level_cell(halves.left),
            level_cell(halves.right)
        );
        append_row(&path, &row)?;
        self.current = Some(Logged {
            keyboard_id,
            path,
            at: now,
            halves,
        });
        Ok(true)
    }

    /// Breaks the current keyboard's log here; nothing when none is logged.
    fn gap(&mut self) -> Result<()> {
        let Some(logged) = self.current.take() else {
            return Ok(());
        };
        append_row(&logged.path, &format!("{},,", timestamp()))
    }
}

static LOG: OnceLock<Option<Mutex<Log>>> = OnceLock::new();

/// The log's directory; `None` when it is off.
fn root() -> Option<PathBuf> {
    if std::env::var_os("ENTROPY_BATTERY_LOG").is_some_and(|value| value == "0") {
        log::info!("battery log: off (ENTROPY_BATTERY_LOG=0)");
        return None;
    }
    match std::env::var_os("ENTROPY_BATTERY_LOG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(dirs::state_dir()?.join("entropy").join("battery")),
    }
}

fn with_log(f: impl FnOnce(&mut Log) -> Result<()>) {
    // The GUI tests connect fake keyboards; they must not reach the user's logs.
    if cfg!(test) {
        return;
    }
    let Some(log) = LOG.get_or_init(|| root().map(|root| Mutex::new(Log::new(root)))) else {
        return;
    };
    let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(error) = f(&mut log) {
        log::warn!("battery log: {error:#}");
    }
}

/// Logs a battery read of the connected keyboard; a read with no levels is
/// skipped.
pub(crate) fn record(keyboard_id: u64, halves: Option<BatteryHalves>) {
    let Some(halves) = halves else {
        return;
    };
    with_log(|log| log.record(keyboard_id, halves, Instant::now()).map(|_| ()));
}

/// Breaks the log of the keyboard that was connected: it went away, another
/// takes its place, or Entropy exits.
pub(crate) fn gap() {
    with_log(Log::gap);
}

fn show(halves: BatteryHalves) -> String {
    let show = |level: Option<u8>| level.map_or("--".to_owned(), |level| format!("{level}%"));
    format!("left {}, right {}", show(halves.left), show(halves.right))
}

fn level_cell(level: Option<u8>) -> String {
    level.map(|level| level.to_string()).unwrap_or_default()
}

fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// Appends `row` to the CSV at `path`, starting a new file with the header.
fn append_row(path: &Path, row: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    set_aside_other_layout(path)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    if file.metadata()?.len() == 0 {
        writeln!(file, "{HEADER}")?;
    }
    writeln!(file, "{row}").with_context(|| format!("writing {}", path.display()))
}

/// Renames a log written with other columns to `<name>-until-<date>.csv`, so
/// new rows never land under a header that does not describe them.
fn set_aside_other_layout(path: &Path) -> Result<()> {
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(());
    };
    let mut header = String::new();
    BufReader::new(file).read_line(&mut header)?;
    if header.is_empty() || header.trim_end() == HEADER {
        return Ok(());
    }
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("battery");
    let aside = path.with_file_name(format!(
        "{name}-until-{}.csv",
        chrono::Local::now().format("%Y-%m-%d")
    ));
    log::info!(
        "battery log: {} has other columns, moving it to {}",
        path.display(),
        aside.display()
    );
    std::fs::rename(path, &aside).with_context(|| format!("moving {} aside", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("entropy-battery-log-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const KEYBOARD: u64 = 0x0302_0100_3c2b_4cf6;

    fn halves(left: Option<u8>, right: Option<u8>) -> BatteryHalves {
        BatteryHalves { left, right }
    }

    fn rows(path: &Path) -> Vec<Vec<String>> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .skip(1)
            .map(|row| row.split(',').map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn each_keyboard_logs_to_its_own_file() {
        let log = Log::new(PathBuf::from("/state/entropy/battery"));
        assert_eq!(
            log.path(KEYBOARD),
            Path::new("/state/entropy/battery/030201003c2b4cf6.csv")
        );
        assert_ne!(log.path(KEYBOARD), log.path(KEYBOARD + 1));
    }

    #[test]
    fn a_connected_keyboard_is_logged_at_once_then_every_five_minutes() {
        let dir = TempDir::new("cadence");
        let mut log = Log::new(dir.0.clone());
        let start = Instant::now();
        let levels = halves(Some(80), Some(75));

        assert!(log.record(KEYBOARD, levels, start).unwrap());
        assert!(!log
            .record(KEYBOARD, levels, start + Duration::from_secs(10))
            .unwrap());
        assert!(!log
            .record(KEYBOARD, levels, start + LOG_EVERY - Duration::from_secs(1))
            .unwrap());
        assert!(log.record(KEYBOARD, levels, start + LOG_EVERY).unwrap());

        let path = log.path(KEYBOARD);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().next(), Some(HEADER));
        let rows = rows(&path);
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.len(), HEADER.split(',').count());
            assert!(
                chrono::DateTime::parse_from_rfc3339(&row[0]).is_ok(),
                "{row:?}"
            );
            assert_eq!(row[1..], ["80", "75"]);
        }
    }

    #[test]
    fn a_half_without_a_reading_leaves_its_cell_empty() {
        let dir = TempDir::new("missing");
        let mut log = Log::new(dir.0.clone());
        log.record(KEYBOARD, halves(Some(43), None), Instant::now())
            .unwrap();

        assert_eq!(rows(&log.path(KEYBOARD))[0][1..], ["43", ""]);
    }

    #[test]
    fn a_gap_is_a_row_with_only_the_time_and_the_next_read_is_logged_at_once() {
        let dir = TempDir::new("gap");
        let mut log = Log::new(dir.0.clone());
        let start = Instant::now();
        let levels = halves(Some(80), Some(81));
        log.record(KEYBOARD, levels, start).unwrap();
        log.gap().unwrap();
        // Nothing logged since the gap: a second one adds no row.
        log.gap().unwrap();
        assert!(log
            .record(KEYBOARD, levels, start + Duration::from_secs(1))
            .unwrap());

        let rows = rows(&log.path(KEYBOARD));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].len(), HEADER.split(',').count());
        assert!(!rows[1][0].is_empty());
        assert!(rows[1][1..].iter().all(String::is_empty), "{:?}", rows[1]);
        assert_eq!(rows[2][1..], ["80", "81"]);
    }

    #[test]
    fn another_keyboard_breaks_the_log_of_the_one_before() {
        let dir = TempDir::new("switch");
        let mut log = Log::new(dir.0.clone());
        let start = Instant::now();
        let other = KEYBOARD + 1;
        log.record(KEYBOARD, halves(Some(60), Some(61)), start)
            .unwrap();
        assert!(log
            .record(
                other,
                halves(Some(90), None),
                start + Duration::from_secs(5)
            )
            .unwrap());

        let first = rows(&log.path(KEYBOARD));
        assert_eq!(first.len(), 2);
        assert!(first[1][1..].iter().all(String::is_empty));
        assert_eq!(rows(&log.path(other))[0][1..], ["90", ""]);
    }

    #[test]
    fn nothing_is_written_before_a_keyboard_is_read() {
        let dir = TempDir::new("empty");
        let mut log = Log::new(dir.0.clone());
        log.gap().unwrap();

        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
    }

    #[test]
    fn a_log_with_other_columns_is_set_aside() {
        let dir = TempDir::new("aside");
        let mut log = Log::new(dir.0.clone());
        let path = log.path(KEYBOARD);
        std::fs::write(
            &path,
            "time,left,right,left_mv\n2026-10-27T16:54:08+03:00,95,67,4012\n",
        )
        .unwrap();

        log.record(KEYBOARD, halves(Some(95), Some(66)), Instant::now())
            .unwrap();

        assert_eq!(rows(&path).len(), 1);
        let aside: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("-until-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert!(aside[0].starts_with("030201003c2b4cf6-until-"));
    }
}
