//! Which Rynk boards are present, for consumers that follow every board as it
//! comes and goes rather than the one the user picked.
//!
//! A board counts as present once its link opened and the handshake passed,
//! since that is where its [`ErgohavenInfo`] comes from. The watcher lets go
//! of the link right after telling everyone, so a board nobody took is closed
//! again: without a consumer no link is held.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Once, OnceLock};

use rmk_types_rynk::protocol::rynk::ergohaven::ErgohavenInfo;

use super::client::RynkClientError;
use super::link::{PhysicalId, RynkLink};
use super::transport::{self, BusEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkEvent {
    Appeared { id: PhysicalId, info: ErgohavenInfo },
    Gone { id: PhysicalId },
}

type Handler = Box<dyn FnMut(&LinkEvent) + Send>;

#[derive(Default)]
struct Presence {
    handlers: Vec<Handler>,
    present: HashMap<PhysicalId, ErgohavenInfo>,
}

impl Presence {
    fn subscribe(&mut self, mut handler: Handler) {
        for (id, info) in &self.present {
            handler(&LinkEvent::Appeared {
                id: id.clone(),
                info: info.clone(),
            });
        }
        self.handlers.push(handler);
    }

    fn dispatch(&mut self, event: &LinkEvent) {
        for handler in &mut self.handlers {
            handler(event);
        }
    }
}

fn presence() -> &'static Mutex<Presence> {
    static PRESENCE: OnceLock<Mutex<Presence>> = OnceLock::new();
    PRESENCE.get_or_init(Mutex::default)
}

fn lock(presence: &Mutex<Presence>) -> std::sync::MutexGuard<'_, Presence> {
    presence.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tells `handler` about every board present now, then about every board that
/// appears or goes. A handler that wants a board that appeared calls
/// [`RynkLink::open`] before it returns and shares the link the watcher just
/// opened; otherwise the board is closed again.
///
/// Handlers run one at a time, on the watcher's thread or, for the boards
/// already present, on the caller's. They must not subscribe in turn.
pub(crate) fn subscribe(handler: impl FnMut(&LinkEvent) + Send + 'static) {
    static WATCHER: Once = Once::new();
    lock(presence()).subscribe(Box::new(handler));
    WATCHER.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("entropy-rynk-watch".into())
            .spawn(|| {
                let watched = transport::watch(|event| {
                    follow(presence(), event, RynkLink::open);
                    true
                });
                if let Err(error) = watched {
                    log::warn!("Rynk boards are not watched: {error}");
                }
            });
        if let Err(error) = spawned {
            log::warn!("Rynk board watcher did not start: {error}");
        }
    });
}

/// Applies one bus event. A board that arrives is opened to learn what it is;
/// one that cannot be opened (another program holds it, or it speaks another
/// revision) is not announced, and so neither is its leaving.
fn follow(
    presence: &Mutex<Presence>,
    event: BusEvent,
    open: impl FnOnce(&PhysicalId) -> Result<Arc<RynkLink>, RynkClientError>,
) {
    match event {
        BusEvent::Arrived(id) => {
            let link = match open(&id) {
                Ok(link) => link,
                Err(error) => {
                    log::warn!("Rynk board {} was not opened: {error}", id.0);
                    return;
                }
            };
            let info = link.board().info.clone();
            let mut presence = lock(presence);
            presence.present.insert(id.clone(), info.clone());
            presence.dispatch(&LinkEvent::Appeared { id, info });
            // Whoever keeps the board took its own hold by now.
            drop(presence);
            drop(link);
        }
        BusEvent::Left(id) => {
            let mut presence = lock(presence);
            if presence.present.remove(&id).is_some() {
                presence.dispatch(&LinkEvent::Gone { id });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use rmk_types_rynk::protocol::rynk::ergohaven::ERGOHAVEN_RYNK_REVISION;
    use rmk_types_rynk::protocol::rynk::{command, Cmd, RynkHeader};

    use super::super::client::testing::*;
    use super::super::link::BoxedTransport;
    use super::*;

    fn id(name: &str) -> PhysicalId {
        PhysicalId(format!("presence-{name}"))
    }

    fn board() -> FakeBoard {
        FakeBoard::new(|header: RynkHeader, _: &[u8]| {
            if let Some(reply) = handshake(header, 512) {
                return vec![reply];
            }
            match header.cmd {
                Cmd::GetCurrentLayer => vec![ok(header, 3u8)],
                _ => vec![ok(header, ())],
            }
        })
    }

    fn opener(
        board: &FakeBoard,
        opened: &Arc<AtomicUsize>,
    ) -> impl Fn(&PhysicalId) -> Result<Arc<RynkLink>, RynkClientError> + Send + 'static {
        let (board, opened) = (board.clone(), opened.clone());
        move |id| {
            RynkLink::acquire(id.clone(), quick(), || {
                opened.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(board.transport()) as BoxedTransport)
            })
        }
    }

    fn recorder(presence: &Mutex<Presence>) -> Arc<Mutex<Vec<LinkEvent>>> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        lock(presence).subscribe(Box::new(move |event| {
            seen.lock().unwrap().push(event.clone())
        }));
        events
    }

    fn eventually(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn appeared(name: &str) -> LinkEvent {
        LinkEvent::Appeared {
            id: id(name),
            info: info(ERGOHAVEN_RYNK_REVISION),
        }
    }

    #[test]
    fn a_board_nobody_takes_is_announced_and_closed_again() {
        let presence = Mutex::default();
        let events = recorder(&presence);
        let (board, opened) = (board(), Arc::new(AtomicUsize::new(0)));

        follow(
            &presence,
            BusEvent::Arrived(id("untaken")),
            opener(&board, &opened),
        );

        assert_eq!(*events.lock().unwrap(), [appeared("untaken")]);
        eventually("the watcher lets go of the board", || board.dropped() == 1);
    }

    #[test]
    fn a_handler_that_takes_the_board_shares_the_watchers_link() {
        let presence = Mutex::default();
        let (board, opened) = (board(), Arc::new(AtomicUsize::new(0)));
        let kept = Arc::new(Mutex::new(None));
        let open = opener(&board, &opened);
        {
            let kept = kept.clone();
            lock(&presence).subscribe(Box::new(move |event| {
                if let LinkEvent::Appeared { id, .. } = event {
                    *kept.lock().unwrap() = Some(open(id).unwrap());
                }
            }));
        }

        follow(
            &presence,
            BusEvent::Arrived(id("taken")),
            opener(&board, &opened),
        );

        assert_eq!(opened.load(Ordering::SeqCst), 1, "one open, one claim");
        let link = kept.lock().unwrap().take().unwrap();
        assert_eq!(board.dropped(), 0, "the handler holds the board");
        assert_eq!(link.request::<command::GetCurrentLayer>(()).unwrap(), 3);
    }

    #[test]
    fn a_late_subscriber_hears_of_the_boards_already_present() {
        let presence = Mutex::default();
        let (board, opened) = (board(), Arc::new(AtomicUsize::new(0)));
        follow(
            &presence,
            BusEvent::Arrived(id("early")),
            opener(&board, &opened),
        );

        let events = recorder(&presence);
        assert_eq!(*events.lock().unwrap(), [appeared("early")]);

        follow(
            &presence,
            BusEvent::Left(id("early")),
            opener(&board, &opened),
        );
        assert_eq!(
            *events.lock().unwrap(),
            [appeared("early"), LinkEvent::Gone { id: id("early") }]
        );
        assert!(lock(&presence).present.is_empty());
    }

    #[test]
    fn a_board_that_did_not_open_is_neither_announced_nor_reported_gone() {
        let presence = Mutex::default();
        let events = recorder(&presence);

        follow(
            &presence,
            BusEvent::Arrived(id("refused")),
            |id: &PhysicalId| {
                RynkLink::acquire(id.clone(), quick(), || {
                    Err(io::ErrorKind::PermissionDenied.into())
                })
            },
        );
        follow(
            &presence,
            BusEvent::Left(id("refused")),
            |_: &PhysicalId| unreachable!("a board leaving is not opened"),
        );

        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn a_board_that_restarts_is_announced_again_under_its_id() {
        let presence = Mutex::default();
        let events = recorder(&presence);
        let (board, opened) = (board(), Arc::new(AtomicUsize::new(0)));

        follow(
            &presence,
            BusEvent::Arrived(id("restart")),
            opener(&board, &opened),
        );
        follow(
            &presence,
            BusEvent::Left(id("restart")),
            opener(&board, &opened),
        );
        follow(
            &presence,
            BusEvent::Arrived(id("restart")),
            opener(&board, &opened),
        );

        assert_eq!(
            *events.lock().unwrap(),
            [
                appeared("restart"),
                LinkEvent::Gone { id: id("restart") },
                appeared("restart"),
            ]
        );
        assert_eq!(opened.load(Ordering::SeqCst), 2);
    }
}
