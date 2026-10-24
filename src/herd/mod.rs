//! Personal host feeds for a Qube dongle: herdr agent counts, Claude Code
//! limits and a log of the halves' batteries. They travel over the Rynk
//! commands of personal firmware builds (0x7EF0..=0x7EFF), not over anything
//! a release firmware or upstream Entropy knows about.
//!
//! Environment: `ENTROPY_HERD=0` leaves it off, `HERDR_SOCKET_PATH` and
//! `CLAUDE_CONFIG_DIR` point at herdr and Claude Code, `ENTROPY_HERD_LOG_DIR`
//! moves the logs.
// Wired into the GUI's start-up separately; until then only tests use it.
#![allow(dead_code)]

pub(crate) mod battery_log;
pub(crate) mod herdr;
mod service;
pub(crate) mod side;
pub(crate) mod usage;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::rynk::link::RynkLink;
use crate::rynk::presence::{self, LinkEvent};
use service::{Herd, Message};

/// How long an exit waits for the herd to break its logs. A board request
/// in flight can take longer, and the logs then end without the break.
const STOP_WAIT: Duration = Duration::from_secs(2);

struct Running {
    messages: mpsc::Sender<Message>,
    done: mpsc::Receiver<()>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

/// Starts following herdr, the limits and every Rynk board that serves the
/// personal commands. Once per process; later calls do nothing.
pub(crate) fn start() {
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if running.is_some() {
        return;
    }
    if std::env::var_os("ENTROPY_HERD").is_some_and(|value| value == "0") {
        log::info!("herd: off (ENTROPY_HERD=0)");
        return;
    }
    let (messages, received) = mpsc::channel();
    let (finished, done) = mpsc::channel();
    let sources = usage::Sources::default();
    let herd = Herd::new(
        move || sources.read(),
        battery_log::Log::for_keyboard,
        Instant::now(),
    );
    if let Err(error) = service::spawn(herd, received, finished) {
        log::warn!("herd did not start: {error}");
        return;
    }

    let feed = messages.clone();
    let client = herdr::Client::new(herdr::Client::default_socket_path());
    if let Err(error) = herdr::follow(client, move |counts| {
        feed.send(Message::Agents(counts)).is_ok()
    }) {
        log::warn!("herd: herdr is not followed: {error}");
    }

    // Boards present before the herd looked are announced right here, on the
    // caller's thread, and their links are already let go: the herd thread
    // opens those itself rather than holding up the caller.
    let live = Arc::new(AtomicBool::new(false));
    let boards = messages.clone();
    let announced = live.clone();
    presence::subscribe(move |event| {
        let message = match event {
            LinkEvent::Appeared { id, .. } if !announced.load(Ordering::Acquire) => {
                Message::Open(id.clone())
            }
            LinkEvent::Appeared { id, .. } => match RynkLink::open(id) {
                Ok(link) => Message::Appeared(link),
                Err(error) => {
                    log::warn!("herd: Rynk board {} was not taken: {error}", id.0);
                    return;
                }
            },
            LinkEvent::Gone { id } => Message::Gone(id.clone()),
        };
        let _ = boards.send(message);
    });
    live.store(true, Ordering::Release);

    *running = Some(Running { messages, done });
}

/// Stops the herd and breaks its logs, for an exit.
pub(crate) fn stop() {
    let Some(running) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let _ = running.messages.send(Message::Stop);
    let _ = running.done.recv_timeout(STOP_WAIT);
}
