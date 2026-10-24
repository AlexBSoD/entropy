//! The layout lane samples when the desktop announces a layout change and
//! polls rarely while it does. No session bus, IBus daemon or physical HID.
use super::*;

const LAYOUT: HostDataMode = HostDataMode {
    time: false,
    volume: false,
    layout: true,
    media: false,
};

/// Records what the layout lane asked for, and keeps the events handle the
/// lane gave it so the test can play the desktop.
#[derive(Clone, Default)]
struct FollowedDesktop {
    events: Arc<Mutex<Option<LayoutEvents>>>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl FollowedDesktop {
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn events(&self) -> LayoutEvents {
        self.events.lock().unwrap().clone().unwrap()
    }

    fn await_calls(&self, count: usize) -> Vec<&'static str> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let calls = self.calls();
            if calls.len() >= count {
                return calls;
            }
            assert!(Instant::now() < deadline, "lane made only {calls:?}");
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl DesktopSource for FollowedDesktop {
    fn volume(&mut self) -> Option<u8> {
        None
    }
    fn layout(&mut self) -> Option<u8> {
        self.calls.lock().unwrap().push("layout");
        Some(0)
    }
    fn media(&mut self) -> Option<MediaInfo> {
        None
    }
    fn follow_layout(&mut self, events: LayoutEvents) {
        *self.events.lock().unwrap() = Some(events);
    }
    fn layout_list_changed(&mut self) {
        self.calls.lock().unwrap().push("relist");
    }
}

#[test]
fn a_followed_layout_is_sampled_when_announced_and_polled_otherwise() {
    let desktop = FollowedDesktop::default();
    let service = HostDataService::start({
        let desktop = desktop.clone();
        move || desktop.clone()
    });
    let bridge = Arc::new(BridgeWake::default());
    let _subscription = service.subscribe(LAYOUT, &bridge);
    desktop.await_calls(1);
    let events = desktop.events();

    assert!(events.following(true));
    let switched = desktop.calls().len();
    // Polled every 100 ms this would have sampled about five times. One poll
    // may already have been under way when following began.
    thread::sleep(Duration::from_millis(500));
    let followed = desktop.calls().len();
    assert!(
        followed - switched <= 1,
        "a followed layout was polled {} times",
        followed - switched
    );

    // Nothing else is due for ten seconds: within five, only the
    // announcements can have caused these samples.
    assert!(events.changed(false));
    assert_eq!(desktop.await_calls(followed + 1)[followed..], ["layout"]);
    assert!(events.changed(true));
    assert_eq!(
        desktop.await_calls(followed + 3)[followed + 1..],
        ["relist", "layout"]
    );

    // Signals gone: the lane polls again without being told to sample.
    assert!(events.following(false));
    desktop.await_calls(followed + 5);
}

#[test]
fn layout_events_end_with_the_service() {
    let desktop = FollowedDesktop::default();
    let service = HostDataService::start({
        let desktop = desktop.clone();
        move || desktop.clone()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while desktop.events.lock().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "the lane never handed out its events"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let events = desktop.events();
    assert!(events.changed(false));
    drop(service);
    assert!(
        !events.changed(false),
        "a stopped service still took events"
    );
    assert!(!events.following(true));
}

fn p2p_pair() -> (zbus::blocking::Connection, zbus::blocking::Connection) {
    let (client, server) = std::os::unix::net::UnixStream::pair().expect("socket pair");
    let guid = zbus::Guid::generate();
    let server = thread::spawn(move || {
        zbus::blocking::connection::Builder::unix_stream(server)
            .server(guid)
            .expect("server guid")
            .p2p()
            .build()
            .expect("server connection")
    });
    let client = zbus::blocking::connection::Builder::unix_stream(client)
        .p2p()
        .build()
        .expect("client connection");
    (client, server.join().expect("server thread"))
}

fn await_state(shared: &DesktopShared, what: &str, done: impl Fn(&DesktopState) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&shared.state.lock().unwrap()) {
        assert!(Instant::now() < deadline, "never saw {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn clear_announcements(shared: &DesktopShared) {
    let mut state = shared.state.lock().unwrap();
    state.layout_due = false;
    state.layout_list_stale = false;
}

#[test]
fn kde_and_ibus_signals_become_layout_events_until_a_bus_goes_away() {
    let shared = Arc::new(DesktopShared::default());
    let events = LayoutEvents {
        shared: Arc::downgrade(&shared),
    };
    let (kde_client, kde) = p2p_pair();
    let (ibus_client, ibus) = p2p_pair();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        futures_lite::future::block_on(async {
            let kde =
                layout_signal_stream(kde_client.inner(), KDE_LAYOUT_INTERFACE, KDE_LAYOUT_PATH)
                    .await;
            let ibus = layout_signal_stream(ibus_client.inner(), IBUS_INTERFACE, IBUS_PATH).await;
            assert!(kde.is_some() && ibus.is_some());
            watch_layout_signals(kde, ibus, &events).await;
        });
        finished_tx.send(()).unwrap();
    });

    // Following starts with a full re-read.
    await_state(&shared, "following", |state| {
        state.layout_following && state.layout_due && state.layout_list_stale
    });
    clear_announcements(&shared);

    kde.emit_signal(
        None::<&str>,
        KDE_LAYOUT_PATH,
        KDE_LAYOUT_INTERFACE,
        "layoutChanged",
        &(1u32,),
    )
    .unwrap();
    await_state(&shared, "a KDE switch", |state| {
        state.layout_due && !state.layout_list_stale
    });
    clear_announcements(&shared);

    kde.emit_signal(
        None::<&str>,
        KDE_LAYOUT_PATH,
        KDE_LAYOUT_INTERFACE,
        "layoutListChanged",
        &(),
    )
    .unwrap();
    await_state(&shared, "a KDE list edit", |state| {
        state.layout_due && state.layout_list_stale
    });
    clear_announcements(&shared);

    ibus.emit_signal(
        None::<&str>,
        IBUS_PATH,
        IBUS_INTERFACE,
        "GlobalEngineChanged",
        &("xkb:us::eng",),
    )
    .unwrap();
    await_state(&shared, "an IBus engine switch", |state| {
        state.layout_due && !state.layout_list_stale
    });
    clear_announcements(&shared);

    // KDE went away: the watcher ends so the lane can go back to polling.
    drop(kde);
    finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the watcher outlived its bus");
}
