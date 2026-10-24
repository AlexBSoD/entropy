//! The herd's thread: it takes every Rynk board that serves the personal
//! commands, keeps its agent row and limit bars fed and logs its halves.
//!
//! Requests go out as background work and the feeds as posts, so the editor
//! always comes first on a shared link. The firmware blanks a feed it has not
//! heard from in 30 s, which is also what happens while herdr is away.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rmk_types_rynk::protocol::rynk::ergohaven::{self, BatteryHalves, GetBatteryHalves};

use super::battery_log::{Halves, Log};
use super::herdr::{AgentCounts, HEARTBEAT};
use super::side::{
    caps, AgentSummary, GetHalvesDiag, GetSideInfo, SetAgents, SetUsage, SideInfo, UsageSummary,
    SIDE_REVISION,
};
use super::usage::Usage;
use crate::rynk::link::{PhysicalId, RynkLink};

/// The halves resample every 15 s, so logging more often only repeats rows.
const LOG_EVERY: Duration = Duration::from_secs(300);
const SUMMARY_EVERY: Duration = Duration::from_secs(600);
/// Unchanged counts go out again this often. herdr reports at least every
/// [`HEARTBEAT`], so a board hears them about that often, well inside the
/// firmware's 30 s.
const RESEND: Duration = Duration::from_secs(5);
/// The posts the herd makes per board: one each, the latest value wins.
const SLOT: u16 = 0;

pub(crate) enum Message {
    /// A board, with the link the herd now holds.
    Appeared(Arc<RynkLink>),
    /// A board that was present before the herd looked, for the herd to open.
    Open(PhysicalId),
    Gone(PhysicalId),
    Agents(Option<AgentCounts>),
    Stop,
}

struct Board {
    link: Arc<RynkLink>,
    side: SideInfo,
    /// `GetBatteryHalves` is served.
    battery: bool,
    /// `None` when the board has nothing to log.
    log: Option<Log>,
    next_log: Instant,
}

impl Board {
    /// One row. The halves report their diagnostics in answer to the refresh
    /// `GetBatteryHalves` sends, after its reply, so a row carries the previous
    /// poll's diagnostics (none in the first), as over Vial; the halves'
    /// uptimes date them.
    fn log_halves(&mut self) -> bool {
        let Some(log) = self.log.as_mut() else {
            return false;
        };
        let mut halves = Halves::default();
        if self.battery {
            match self.link.request_background::<GetBatteryHalves>(()) {
                Ok(levels) => (halves.left, halves.right) = levels_of(levels),
                Err(error) => log::warn!("herd battery: {error}"),
            }
        }
        if self.side.serves(caps::HALVES_DIAG) {
            match self.link.request_background::<GetHalvesDiag>(()) {
                Ok(diag) => {
                    halves.left_diag = diag.left.into();
                    halves.right_diag = diag.right.into();
                }
                Err(error) => log::warn!("herd diag: {error}"),
            }
        }
        if let Err(error) = log.append(halves) {
            log::warn!("herd battery: {error:#}");
        }
        true
    }
}

fn levels_of(levels: BatteryHalves) -> (Option<u8>, Option<u8>) {
    let level = |bit: u8, pct: u8| (levels.mask & bit != 0 && pct <= 100).then_some(pct);
    (level(1, levels.left), level(2, levels.right))
}

/// What the periodic summary reports: proof of life, and the number that
/// would make a spin obvious.
#[derive(Clone, Copy, Default)]
struct Stats {
    herdr_reports: u64,
    posts: u64,
    log_rows: u64,
}

pub(crate) struct Herd {
    boards: HashMap<PhysicalId, Board>,
    agents: Option<AgentSummary>,
    agents_sent: Option<(AgentSummary, Instant)>,
    read_usage: Box<dyn FnMut() -> Usage + Send>,
    log_for: Box<dyn Fn([u8; 8]) -> Option<Log> + Send>,
    next_usage: Instant,
    next_summary: Instant,
    started: Instant,
    stats: Stats,
    reported: Stats,
}

impl Herd {
    pub(crate) fn new(
        read_usage: impl FnMut() -> Usage + Send + 'static,
        log_for: impl Fn([u8; 8]) -> Option<Log> + Send + 'static,
        now: Instant,
    ) -> Self {
        Self {
            boards: HashMap::new(),
            agents: None,
            agents_sent: None,
            read_usage: Box::new(read_usage),
            log_for: Box::new(log_for),
            next_usage: now,
            next_summary: now + SUMMARY_EVERY,
            started: now,
            stats: Stats::default(),
            reported: Stats::default(),
        }
    }

    /// Takes the board if it serves anything the herd feeds or logs, and
    /// brings it up to date at once: a board that just rebooted shows nothing.
    pub(crate) fn attach(&mut self, link: Arc<RynkLink>, now: Instant) {
        let id = link.id().clone();
        let side = match link.request_background::<GetSideInfo>(()) {
            Ok(side) => side,
            Err(error) if error.is_unknown_cmd() => {
                log::info!("herd: Rynk board {} has no personal commands", id.0);
                return;
            }
            Err(error) => {
                log::warn!(
                    "herd: Rynk board {} did not say what it serves: {error}",
                    id.0
                );
                return;
            }
        };
        if side.revision != SIDE_REVISION {
            log::warn!(
                "herd: Rynk board {} speaks personal revision {}, the herd {SIDE_REVISION}",
                id.0,
                side.revision
            );
            return;
        }
        let info = &link.board().info;
        let battery = info.caps & ergohaven::caps::BATTERY_HALVES != 0;
        let feeds = side.serves(caps::AGENTS) || side.serves(caps::USAGE);
        let logs = battery || side.serves(caps::HALVES_DIAG);
        if !feeds && !logs {
            log::info!(
                "herd: Rynk board {} serves none of the herd's commands",
                id.0
            );
            return;
        }
        let log = logs.then(|| (self.log_for)(info.keyboard_uid)).flatten();
        log::info!(
            "herd: following Rynk board {} (caps {:#x})",
            id.0,
            side.caps
        );
        let mut board = Board {
            link,
            side,
            battery,
            log,
            next_log: now,
        };
        let usage = UsageSummary::from((self.read_usage)());
        let stats = &mut self.stats;
        let fed = self
            .agents
            .is_none_or(|agents| post::<SetAgents>(&board, caps::AGENTS, agents, stats))
            && post::<SetUsage>(&board, caps::USAGE, usage, stats);
        if !fed {
            return;
        }
        if board.log_halves() {
            self.stats.log_rows += 1;
            board.next_log = now + LOG_EVERY;
        }
        self.boards.insert(id, board);
    }

    /// Lets the board go and breaks its log there.
    pub(crate) fn detach(&mut self, id: &PhysicalId) {
        if let Some(board) = self.boards.remove(id) {
            log::info!("herd: Rynk board {} is gone", id.0);
            mark_gap(board);
        }
    }

    pub(crate) fn agents(&mut self, counts: Option<AgentCounts>, now: Instant) {
        self.stats.herdr_reports += 1;
        self.agents = counts.map(AgentSummary::from);
        let Some(agents) = self.agents else {
            // Nothing to show: the firmware blanks the row on its own, and
            // the next reading goes out at once.
            self.agents_sent = None;
            return;
        };
        let due = self
            .agents_sent
            .is_none_or(|(sent, at)| sent != agents || now.duration_since(at) >= RESEND);
        if due {
            if self.agents_sent.is_none_or(|(sent, _)| sent != agents) {
                log::info!("herd agents: {}", counts.unwrap_or_default());
            }
            self.post_all::<SetAgents>(caps::AGENTS, agents);
            self.agents_sent = Some((agents, now));
        }
    }

    /// Runs what is due: the limits every [`HEARTBEAT`], each board's log row,
    /// the summary.
    pub(crate) fn tick(&mut self, now: Instant) {
        if now >= self.next_usage {
            let usage = UsageSummary::from((self.read_usage)());
            self.post_all::<SetUsage>(caps::USAGE, usage);
            self.next_usage = now + HEARTBEAT;
        }
        for board in self.boards.values_mut() {
            if now >= board.next_log {
                if board.log_halves() {
                    self.stats.log_rows += 1;
                }
                board.next_log = now + LOG_EVERY;
            }
        }
        if now >= self.next_summary {
            self.summary(now);
        }
    }

    /// When [`tick`](Self::tick) has something to do next.
    pub(crate) fn next_due(&self) -> Instant {
        self.boards
            .values()
            .map(|board| board.next_log)
            .chain([self.next_usage, self.next_summary])
            .min()
            .unwrap_or(self.next_usage)
    }

    /// Breaks every log here, for an exit.
    pub(crate) fn stop(&mut self) {
        for (_, board) in self.boards.drain() {
            mark_gap(board);
        }
    }

    fn summary(&mut self, now: Instant) {
        let delta = Stats {
            herdr_reports: self.stats.herdr_reports - self.reported.herdr_reports,
            posts: self.stats.posts - self.reported.posts,
            log_rows: self.stats.log_rows - self.reported.log_rows,
        };
        log::info!(
            "herd: alive {}m, {} boards; last {}m: {} herdr reports, {} posts, {} log rows",
            now.duration_since(self.started).as_secs() / 60,
            self.boards.len(),
            SUMMARY_EVERY.as_secs() / 60,
            delta.herdr_reports,
            delta.posts,
            delta.log_rows,
        );
        self.reported = self.stats;
        self.next_summary = now + SUMMARY_EVERY;
    }

    /// Posts to every board serving `cap`; a board whose link is gone is let go.
    fn post_all<E>(&mut self, cap: u32, request: E::Request)
    where
        E: rmk_types_rynk::protocol::rynk::command::Endpoint,
        E::Request: Clone + Send + 'static,
    {
        let mut lost = Vec::new();
        for (id, board) in &self.boards {
            if !post::<E>(board, cap, request.clone(), &mut self.stats) {
                lost.push(id.clone());
            }
        }
        for id in lost {
            self.detach(&id);
        }
    }
}

/// `true` unless the link is gone. A board that does not serve `cap` is
/// skipped.
fn post<E>(board: &Board, cap: u32, request: E::Request, stats: &mut Stats) -> bool
where
    E: rmk_types_rynk::protocol::rynk::command::Endpoint,
    E::Request: Send + 'static,
{
    if !board.side.serves(cap) {
        return true;
    }
    match board.link.post::<E>(SLOT, request) {
        Ok(()) => {
            stats.posts += 1;
            true
        }
        Err(error) => {
            log::warn!("herd: Rynk board {}: {error}", board.link.id().0);
            false
        }
    }
}

fn mark_gap(board: Board) {
    if let Some(mut log) = board.log {
        if let Err(error) = log.mark_gap() {
            log::warn!("herd battery: {error:#}");
        }
    }
}

/// Runs `herd` on a thread of its own until [`Message::Stop`] or until every
/// sender is gone, then breaks the logs and signals `done`.
pub(crate) fn spawn(
    mut herd: Herd,
    messages: mpsc::Receiver<Message>,
    done: mpsc::Sender<()>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("herd".to_owned())
        .spawn(move || {
            loop {
                herd.tick(Instant::now());
                let wait = herd.next_due().saturating_duration_since(Instant::now());
                match messages.recv_timeout(wait) {
                    Ok(Message::Appeared(link)) => herd.attach(link, Instant::now()),
                    Ok(Message::Open(id)) => match RynkLink::open(&id) {
                        Ok(link) => herd.attach(link, Instant::now()),
                        Err(error) => {
                            log::warn!("herd: Rynk board {} was not opened: {error}", id.0)
                        }
                    },
                    Ok(Message::Gone(id)) => herd.detach(&id),
                    Ok(Message::Agents(counts)) => herd.agents(counts, Instant::now()),
                    Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            herd.stop();
            let _ = done.send(());
        })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rmk_types_rynk::protocol::rynk::command::Endpoint as _;
    use rmk_types_rynk::protocol::rynk::ergohaven::ERGOHAVEN_RYNK_REVISION;
    use rmk_types_rynk::protocol::rynk::{Cmd, RynkError, RynkHeader};

    use super::super::side::{HalfDiag, HalvesDiag, SleepStats};
    use super::*;
    use crate::rynk::client::testing::*;
    use crate::rynk::link::BoxedTransport;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "entropy-herd-service-{name}-{}",
                std::process::id()
            ));
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
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// What a board was fed, decoded; a channel, since the fake board's
    /// handler runs under its lock.
    #[derive(Debug, PartialEq)]
    enum Fed {
        Agents(AgentSummary),
        Usage(UsageSummary),
    }

    struct Fake {
        board: FakeBoard,
        fed: mpsc::Receiver<Fed>,
    }

    impl Fake {
        /// A board answering `GetSideInfo` with `side` (`None`: `UnknownCmd`)
        /// and reporting `ergohaven_caps` in its handshake.
        fn new(side: Option<SideInfo>, ergohaven_caps: u32) -> Self {
            let (feed, fed) = mpsc::channel();
            let board =
                FakeBoard::new(move |header: RynkHeader, payload: &[u8]| match header.cmd {
                    Cmd::GetErgohavenInfo => vec![ok(
                        header,
                        ergohaven::ErgohavenInfo {
                            caps: ergohaven_caps,
                            ..info(ERGOHAVEN_RYNK_REVISION)
                        },
                    )],
                    cmd if cmd == GetSideInfo::CMD => match side {
                        Some(side) => vec![ok(header, side)],
                        None => vec![err(header, RynkError::UnknownCmd)],
                    },
                    cmd if cmd == SetAgents::CMD => {
                        let _ = feed.send(Fed::Agents(postcard::from_bytes(payload).unwrap()));
                        vec![ok(header, ())]
                    }
                    cmd if cmd == SetUsage::CMD => {
                        let _ = feed.send(Fed::Usage(postcard::from_bytes(payload).unwrap()));
                        vec![ok(header, ())]
                    }
                    cmd if cmd == GetBatteryHalves::CMD => vec![ok(
                        header,
                        BatteryHalves {
                            mask: 1,
                            left: 80,
                            right: 0xFF,
                        },
                    )],
                    cmd if cmd == GetHalvesDiag::CMD => vec![ok(header, diag())],
                    _ => handshake(header, 512).into_iter().collect(),
                });
            Self { board, fed }
        }

        fn link(&self, name: &str) -> Arc<RynkLink> {
            let board = self.board.clone();
            RynkLink::acquire(PhysicalId(format!("herd-{name}")), quick(), move || {
                Ok(Box::new(board.transport()) as BoxedTransport)
            })
            .unwrap()
        }

        fn next(&self) -> Fed {
            self.fed
                .recv_timeout(Duration::from_secs(3))
                .expect("the board was fed")
        }

        fn nothing_more(&self) {
            assert_eq!(self.fed.recv_timeout(Duration::from_millis(200)).ok(), None);
        }
    }

    fn side(caps: u32) -> SideInfo {
        SideInfo {
            revision: SIDE_REVISION,
            caps,
        }
    }

    fn diag() -> HalvesDiag {
        HalvesDiag {
            left: HalfDiag {
                sleep: Some(SleepStats {
                    wakes: 7,
                    awake_min: 30,
                    uptime_min: 600,
                }),
                pointing: None,
                power: None,
            },
            right: HalfDiag {
                sleep: None,
                pointing: None,
                power: None,
            },
        }
    }

    fn counts(working: u8) -> AgentCounts {
        AgentCounts {
            working,
            idle: 1,
            ..AgentCounts::default()
        }
    }

    fn usage() -> Usage {
        Usage {
            five_hour: Some(42),
            seven_day: None,
            stale: false,
        }
    }

    fn herd(now: Instant) -> Herd {
        Herd::new(usage, |_| None, now)
    }

    #[test]
    fn a_board_with_the_personal_commands_is_fed_at_once() {
        let fake = Fake::new(Some(side(caps::AGENTS | caps::USAGE)), 0);
        let now = Instant::now();
        let mut herd = herd(now);
        herd.agents(Some(counts(2)), now);

        herd.attach(fake.link("fed"), now);

        let mut fed = [fake.next(), fake.next()];
        fed.sort_by_key(|fed| matches!(fed, Fed::Usage(_)));
        assert_eq!(
            fed,
            [Fed::Agents(counts(2).into()), Fed::Usage(usage().into())]
        );
    }

    #[test]
    fn a_board_without_them_is_let_go() {
        let fake = Fake::new(None, ergohaven::caps::BATTERY_HALVES);
        let now = Instant::now();
        let mut herd = herd(now);
        herd.agents(Some(counts(2)), now);

        herd.attach(fake.link("release"), now);

        assert!(herd.boards.is_empty());
        let deadline = Instant::now() + Duration::from_secs(3);
        while fake.board.dropped() == 0 {
            assert!(Instant::now() < deadline, "the link is closed");
            thread::sleep(Duration::from_millis(2));
        }
        fake.nothing_more();
    }

    #[test]
    fn unchanged_counts_wait_for_the_resend() {
        let fake = Fake::new(Some(side(caps::AGENTS)), 0);
        let now = Instant::now();
        let mut herd = herd(now);
        herd.attach(fake.link("resend"), now);
        let posts = |herd: &Herd| herd.stats.posts;
        let attached = posts(&herd);

        herd.agents(Some(counts(1)), now);
        assert_eq!(fake.next(), Fed::Agents(counts(1).into()));
        herd.agents(Some(counts(1)), now + RESEND / 2);
        assert_eq!(posts(&herd), attached + 1);
        fake.nothing_more();

        herd.agents(Some(counts(1)), now + RESEND);
        assert_eq!(fake.next(), Fed::Agents(counts(1).into()));
        herd.agents(Some(counts(3)), now + RESEND + Duration::from_millis(1));
        assert_eq!(fake.next(), Fed::Agents(counts(3).into()));
        assert_eq!(posts(&herd), attached + 3);
    }

    #[test]
    fn nothing_goes_out_while_herdr_is_away() {
        let fake = Fake::new(Some(side(caps::AGENTS)), 0);
        let now = Instant::now();
        let mut herd = herd(now);
        herd.agents(Some(counts(1)), now);
        herd.attach(fake.link("away"), now);
        assert_eq!(fake.next(), Fed::Agents(counts(1).into()));

        herd.agents(None, now + RESEND);
        fake.nothing_more();
        herd.agents(Some(counts(1)), now + RESEND + Duration::from_millis(1));
        assert_eq!(fake.next(), Fed::Agents(counts(1).into()));
    }

    #[test]
    fn the_limits_go_out_every_heartbeat() {
        let fake = Fake::new(Some(side(caps::USAGE)), 0);
        let now = Instant::now();
        let mut herd = herd(now);
        herd.tick(now);
        herd.attach(fake.link("limits"), now);
        assert_eq!(fake.next(), Fed::Usage(usage().into()));

        herd.tick(now + HEARTBEAT / 2);
        fake.nothing_more();
        herd.tick(now + HEARTBEAT);
        assert_eq!(fake.next(), Fed::Usage(usage().into()));
    }

    #[test]
    fn a_rebooted_board_is_fed_again_at_once() {
        let fake = Fake::new(Some(side(caps::AGENTS)), 0);
        let now = Instant::now();
        let mut herd = herd(now);
        herd.agents(Some(counts(1)), now);
        herd.attach(fake.link("reboot"), now);
        assert_eq!(fake.next(), Fed::Agents(counts(1).into()));

        herd.attach(fake.link("reboot"), now + Duration::from_millis(1));

        assert_eq!(fake.next(), Fed::Agents(counts(1).into()));
        assert_eq!(herd.boards.len(), 1);
    }

    #[test]
    fn the_halves_are_logged_from_the_start_and_break_where_the_board_left() {
        let dir = TempDir::new("log");
        let fake = Fake::new(
            Some(side(caps::HALVES_DIAG)),
            ergohaven::caps::BATTERY_HALVES,
        );
        let now = Instant::now();
        let logs = dir.0.clone();
        let mut herd = Herd::new(usage, move |_| Some(Log::in_dir(&logs)), now);

        herd.attach(fake.link("log"), now);
        let battery = dir.0.join("battery.csv");
        let logged = rows(&battery);
        assert_eq!(logged.len(), 2, "{logged:?}");
        assert!(logged[1].contains(",80,,7,30,600,,,,"), "{}", logged[1]);

        herd.tick(now + LOG_EVERY / 2);
        assert_eq!(rows(&battery).len(), 2);
        herd.tick(now + LOG_EVERY);
        assert_eq!(rows(&battery).len(), 3);

        herd.detach(&PhysicalId("herd-log".to_owned()));
        let logged = rows(&battery);
        assert_eq!(logged.len(), 4);
        assert!(logged[3].ends_with(&",".repeat(12)), "{}", logged[3]);
    }

    #[test]
    fn levels_count_only_where_the_mask_says_so() {
        assert_eq!(
            levels_of(BatteryHalves {
                mask: 2,
                left: 80,
                right: 55,
            }),
            (None, Some(55))
        );
        assert_eq!(
            levels_of(BatteryHalves {
                mask: 3,
                left: 0xFF,
                right: 100,
            }),
            (None, Some(100))
        );
    }

    #[test]
    fn the_thread_breaks_the_logs_on_stop() {
        let dir = TempDir::new("stop");
        let fake = Fake::new(Some(side(caps::HALVES_DIAG)), 0);
        let logs = dir.0.clone();
        let herd = Herd::new(usage, move |_| Some(Log::in_dir(&logs)), Instant::now());
        let (messages, received) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let thread = spawn(herd, received, done).unwrap();

        messages.send(Message::Appeared(fake.link("stop"))).unwrap();
        messages.send(Message::Stop).unwrap();

        finished.recv_timeout(Duration::from_secs(3)).unwrap();
        thread.join().unwrap();
        let logged = rows(&dir.0.join("diag.csv"));
        assert_eq!(logged.len(), 3, "{logged:?}");
        assert!(logged[2].ends_with(&",".repeat(38)), "{}", logged[2]);
    }
}
