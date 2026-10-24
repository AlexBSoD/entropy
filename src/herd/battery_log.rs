//! CSV logs of a keyboard's halves, for watching how fast they drain and
//! whether they sleep: `battery.csv` and `diag.csv` in a directory per keyboard.
//!
//! One row per poll: time, left %, right %, then each half's sleep statistics
//! (wake-ups, awake minutes, uptime minutes), then each half's pointing sensor
//! loop passes and motion reads. A half with no reading leaves its cells empty,
//! so a disconnect shows up as a gap rather than as a repeated last value.
//! `diag.csv` holds the halves' power diagnostics, see [`DIAG_HEADER`]; only
//! diagnostic firmware reports them.

use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const HEADER: &str = concat!(
    "time,left,right,left_wakes,left_awake_min,left_up_min,right_wakes,right_awake_min,right_up_min,",
    "left_pt_wakes,left_pt_reads,right_pt_wakes,right_pt_reads"
);

/// Per half: battery mV (empty until measured), VBUS present at that sample,
/// REG1 and REG0 on DC/DC, VDDH supply, RESETREAS of the boot, boots since power
/// loss, uptime ms, CPU active ms (empty without the CPU counter), key presses,
/// link RSSI dBm (empty until read), last disconnect reason, disconnects,
/// connection interval µs (0 between sessions), peripheral latency, supervision
/// timeout ms, parameter updates, split messages sent and received.
const DIAG_HEADER: &str = concat!(
    "time,",
    "left_mv,left_vbus,left_dcdc,left_dcdc0,left_vddh,left_resetreas,left_boots,left_up_ms,left_cpu_ms,",
    "left_keys,left_rssi,left_disc_reason,left_disconnects,left_interval_us,left_latency,left_timeout_ms,",
    "left_param_updates,left_tx,left_rx,",
    "right_mv,right_vbus,right_dcdc,right_dcdc0,right_vddh,right_resetreas,right_boots,right_up_ms,right_cpu_ms,",
    "right_keys,right_rssi,right_disc_reason,right_disconnects,right_interval_us,right_latency,right_timeout_ms,",
    "right_param_updates,right_tx,right_rx"
);

/// What one poll learned about the halves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Halves {
    /// Percent; `None` for a half the keyboard has no reading from.
    pub(crate) left: Option<u8>,
    pub(crate) right: Option<u8>,
    pub(crate) left_diag: HalfDiag,
    pub(crate) right_diag: HalfDiag,
}

/// A half's diagnostics, as last reported; only diagnostic firmware has them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HalfDiag {
    pub(crate) sleep: Option<SleepStats>,
    pub(crate) pointing: Option<PointingStats>,
    pub(crate) power: Option<PowerDiag>,
}

/// How a half has slept since it booted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SleepStats {
    pub(crate) wakes: u16,
    pub(crate) awake_min: u16,
    pub(crate) uptime_min: u16,
}

/// How busy a half's pointing sensor loop has been since it booted. The
/// counters wrap, so compare consecutive rows modulo 2^32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PointingStats {
    pub(crate) wakes: u32,
    pub(crate) reads: u32,
}

/// A half's power diagnostics; counters run since the half booted and wrap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PowerDiag {
    pub(crate) flags: u8,
    /// Last measured battery voltage; 0 before the first measurement.
    pub(crate) battery_mv: u16,
    /// nRF52840 `POWER.RESETREAS` of the half's current boot.
    pub(crate) reset_reason: u32,
    /// Boots since the half lost power.
    pub(crate) boot_count: u16,
    pub(crate) uptime_ms: u32,
    /// Only measured when `flags` has [`PowerDiag::CPU_COUNTER`].
    pub(crate) cpu_active_ms: u32,
    pub(crate) key_presses: u32,
    /// dBm; 127 until the half has read one.
    pub(crate) rssi: i8,
    pub(crate) last_disconnect_reason: u8,
    pub(crate) conn_interval_us: u32,
    pub(crate) peripheral_latency: u16,
    pub(crate) supervision_timeout_ms: u16,
    pub(crate) param_updates: u16,
    pub(crate) disconnects: u16,
    pub(crate) split_tx: u32,
    pub(crate) split_rx: u32,
}

impl PowerDiag {
    pub(crate) const VBUS: u8 = 0x02;
    pub(crate) const DCDC_REG1: u8 = 0x04;
    pub(crate) const DCDC_REG0: u8 = 0x08;
    pub(crate) const HIGH_VOLTAGE: u8 = 0x10;
    pub(crate) const CPU_COUNTER: u8 = 0x20;
}

impl std::fmt::Display for Halves {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |level: Option<u8>| level.map_or("--".to_owned(), |level| format!("{level}%"));
        write!(f, "left {}, right {}", show(self.left), show(self.right))
    }
}

pub(crate) struct Log {
    battery: PathBuf,
    diag: PathBuf,
    last: Option<(Option<u8>, Option<u8>)>,
}

impl Log {
    /// The logs of the keyboard with this Vial keyboard id, under
    /// `$ENTROPY_HERD_LOG_DIR/<id>/`, by default `$XDG_STATE_HOME/entropy/herd/<id>/`.
    pub(crate) fn for_keyboard(keyboard_uid: [u8; 8]) -> Option<Self> {
        let id: String = keyboard_uid
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let root = match std::env::var_os("ENTROPY_HERD_LOG_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => dirs::state_dir()?.join("entropy").join("herd"),
        };
        Some(Self::in_dir(&root.join(id)))
    }

    pub(crate) fn in_dir(dir: &Path) -> Self {
        Self {
            battery: dir.join("battery.csv"),
            diag: dir.join("diag.csv"),
            last: None,
        }
    }

    pub(crate) fn append(&mut self, halves: Halves) -> Result<()> {
        // Uptime moves every poll; only a level change is worth a log line.
        let levels = (halves.left, halves.right);
        if self.last != Some(levels) {
            log::info!("herd battery: {halves}");
        }
        self.last = Some(levels);

        let now = timestamp();
        let (left, right) = (halves.left_diag, halves.right_diag);
        let row = format!(
            "{now},{},{},{},{},{},{}",
            level_cell(halves.left),
            level_cell(halves.right),
            sleep_cells(left.sleep),
            sleep_cells(right.sleep),
            pointing_cells(left.pointing),
            pointing_cells(right.pointing),
        );
        append_row(&self.battery, HEADER, &row)?;
        let diag_row = format!(
            "{now},{},{}",
            power_cells(left.power),
            power_cells(right.power)
        );
        append_row(&self.diag, DIAG_HEADER, &diag_row)
    }

    /// Marks that nothing was logged from here on, for instance because
    /// Entropy is about to exit: a row with only the time, so a plot breaks the
    /// line there instead of drawing it straight across the gap.
    pub(crate) fn mark_gap(&mut self) -> Result<()> {
        self.last = None;
        let now = timestamp();
        append_row(&self.battery, HEADER, &format!("{now}{}", ",".repeat(12)))?;
        append_row(&self.diag, DIAG_HEADER, &format!("{now}{}", ",".repeat(38)))
    }
}

fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// Appends `row` to the CSV at `path`, starting a new file with `header`.
fn append_row(path: &Path, header: &str, row: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    set_aside_other_layout(path, header)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    if file.metadata()?.len() == 0 {
        writeln!(file, "{header}")?;
    }
    writeln!(file, "{row}").with_context(|| format!("writing {}", path.display()))
}

fn level_cell(level: Option<u8>) -> String {
    level.map(|level| level.to_string()).unwrap_or_default()
}

fn sleep_cells(stats: Option<SleepStats>) -> String {
    stats.map_or_else(
        || ",,".to_owned(),
        |stats| format!("{},{},{}", stats.wakes, stats.awake_min, stats.uptime_min),
    )
}

fn pointing_cells(stats: Option<PointingStats>) -> String {
    stats.map_or_else(
        || ",".to_owned(),
        |stats| format!("{},{}", stats.wakes, stats.reads),
    )
}

fn power_cells(diag: Option<PowerDiag>) -> String {
    let Some(diag) = diag else {
        // One cell per half column of DIAG_HEADER.
        return ",".repeat(18);
    };
    let flag = |flag: u8| u8::from(diag.flags & flag != 0).to_string();
    let shown = |shown: bool, value: String| if shown { value } else { String::new() };
    [
        shown(diag.battery_mv != 0, diag.battery_mv.to_string()),
        flag(PowerDiag::VBUS),
        flag(PowerDiag::DCDC_REG1),
        flag(PowerDiag::DCDC_REG0),
        flag(PowerDiag::HIGH_VOLTAGE),
        format!("0x{:x}", diag.reset_reason),
        diag.boot_count.to_string(),
        diag.uptime_ms.to_string(),
        shown(
            diag.flags & PowerDiag::CPU_COUNTER != 0,
            diag.cpu_active_ms.to_string(),
        ),
        diag.key_presses.to_string(),
        shown(diag.rssi != 127, diag.rssi.to_string()),
        format!("0x{:02x}", diag.last_disconnect_reason),
        diag.disconnects.to_string(),
        diag.conn_interval_us.to_string(),
        diag.peripheral_latency.to_string(),
        diag.supervision_timeout_ms.to_string(),
        diag.param_updates.to_string(),
        diag.split_tx.to_string(),
        diag.split_rx.to_string(),
    ]
    .join(",")
}

/// Renames a log written with other columns to `<name>-until-<date>.csv`, so
/// new rows never land under a header that does not describe them.
fn set_aside_other_layout(path: &Path, expected: &str) -> Result<()> {
    let Ok(file) = std::fs::File::open(path) else {
        return Ok(());
    };
    let mut header = String::new();
    BufReader::new(file).read_line(&mut header)?;
    if header.is_empty() || header.trim_end() == expected {
        return Ok(());
    }
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("log");
    let aside = path.with_file_name(format!(
        "{name}-until-{}.csv",
        chrono::Local::now().format("%Y-%m-%d")
    ));
    log::info!(
        "herd: {} has other columns, moving it to {}",
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
                .join(format!("entropy-herd-log-{name}-{}", std::process::id()));
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

    fn rows(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn sleep_cells_leave_a_gap_for_a_half_without_stats() {
        assert_eq!(sleep_cells(None), ",,");
        let stats = SleepStats {
            wakes: 3,
            awake_min: 41,
            uptime_min: 600,
        };
        assert_eq!(sleep_cells(Some(stats)), "3,41,600");
    }

    #[test]
    fn pointing_cells_keep_the_full_counters() {
        assert_eq!(pointing_cells(None), ",");
        let stats = PointingStats {
            wakes: 0x0100_0000,
            reads: 340,
        };
        assert_eq!(pointing_cells(Some(stats)), "16777216,340");
    }

    #[test]
    fn power_cells_fill_the_header() {
        let columns = DIAG_HEADER.split(',').count();
        let diag = PowerDiag {
            flags: PowerDiag::VBUS | PowerDiag::DCDC_REG1,
            reset_reason: 0x4,
            rssi: -61,
            ..PowerDiag::default()
        };
        let row = format!("time,{},{}", power_cells(Some(diag)), power_cells(None));
        assert_eq!(row.split(',').count(), columns);
        // No voltage measured yet, no CPU counter: those cells stay empty.
        assert!(row.starts_with("time,,1,1,0,0,0x4,0,0,,0,-61,0x00,"));
    }

    #[test]
    fn a_poll_adds_a_row_to_each_log_under_its_header() {
        let dir = TempDir::new("append");
        let mut log = Log::in_dir(&dir.0);
        let halves = Halves {
            left: Some(43),
            right: None,
            left_diag: HalfDiag {
                sleep: Some(SleepStats {
                    wakes: 2,
                    awake_min: 5,
                    uptime_min: 60,
                }),
                ..HalfDiag::default()
            },
            ..Halves::default()
        };
        log.append(halves).unwrap();
        log.append(halves).unwrap();

        let battery = rows(&dir.0.join("battery.csv"));
        assert_eq!(battery[0], HEADER);
        assert_eq!(battery.len(), 3);
        let cells: Vec<_> = battery[1].split(',').collect();
        assert_eq!(cells.len(), HEADER.split(',').count());
        assert_eq!(&cells[1..6], ["43", "", "2", "5", "60"]);
        let diag = rows(&dir.0.join("diag.csv"));
        assert_eq!(diag[0], DIAG_HEADER);
        assert_eq!(diag[1].split(',').count(), DIAG_HEADER.split(',').count());
    }

    #[test]
    fn a_gap_is_a_row_with_only_the_time() {
        let dir = TempDir::new("gap");
        let mut log = Log::in_dir(&dir.0);
        log.append(Halves {
            left: Some(80),
            right: Some(81),
            ..Halves::default()
        })
        .unwrap();
        log.mark_gap().unwrap();

        for (file, header) in [("battery.csv", HEADER), ("diag.csv", DIAG_HEADER)] {
            let rows = rows(&dir.0.join(file));
            let gap: Vec<_> = rows.last().unwrap().split(',').collect();
            assert_eq!(gap.len(), header.split(',').count());
            assert!(!gap[0].is_empty());
            assert!(
                gap[1..].iter().all(|cell| cell.is_empty()),
                "{file}: {gap:?}"
            );
        }
    }

    #[test]
    fn a_log_with_other_columns_is_set_aside() {
        let dir = TempDir::new("aside");
        let path = dir.0.join("battery.csv");
        std::fs::write(&path, "time,left,right\n2026-09-27T16:54:08+03:00,95,67\n").unwrap();

        set_aside_other_layout(&path, HEADER).unwrap();

        assert!(!path.exists());
        let moved: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(moved.len(), 1);
        assert!(moved[0].to_string_lossy().starts_with("battery-until-"));
    }

    #[test]
    fn each_keyboard_logs_to_its_own_directory() {
        let a = Log::for_keyboard([0xf6, 0x4c, 0x2b, 0x3c, 0, 1, 2, 3]).unwrap();
        let b = Log::for_keyboard([0xf6, 0x4c, 0x2b, 0x3c, 0, 1, 2, 4]).unwrap();
        assert!(a
            .battery
            .ends_with("entropy/herd/f64c2b3c00010203/battery.csv"));
        assert_ne!(a.battery.parent(), b.battery.parent());
    }
}
