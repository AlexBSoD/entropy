use super::*;
use std::cell::RefCell;

// Per-worker OS/HID boundary scripts; no global clock changes or copied
// recovery logic. Unscripted tests and production use the native boundaries.
thread_local! {
    static FAILURE_TIME: RefCell<Option<Box<dyn Fn() -> Instant>>> = RefCell::new(None);
    static OUTPUT_SCRIPT: RefCell<Option<Box<dyn FnMut(&[u8]) -> anyhow::Result<()>>>> = RefCell::new(None);
}

pub(super) fn failure_now(native: Instant) -> Instant {
    FAILURE_TIME.with(|clock| clock.borrow().as_ref().map_or(native, |clock| clock()))
}

pub(super) fn scripted_write(payload: &[u8]) -> Option<anyhow::Result<()>> {
    OUTPUT_SCRIPT.with(|script| script.borrow_mut().as_mut().map(|script| script(payload)))
}

fn incident_target() -> crate::device::Device {
    crate::device::Device {
        name: "SM62 incident fixture".into(),
        vendor_id: SM62_VENDOR_ID,
        product_id: SM62_PRODUCT_ID,
        manufacturer: String::new(),
        serial_number: "incident fixture".into(),
        bus_type: "USB".into(),
        path: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/qmk_hid_host.rs")
            .to_string_lossy()
            .into_owned(),
        instance_token: "incident-owner".into(),
        firmware: crate::firmware::FirmwareProtocol::Vial,
    }
}

#[test]
fn real_worker_grants_second_transient_incident_its_own_grace() {
    let desktop = HostDataService {
        owner: Arc::new(DesktopOwner {
            shared: Arc::new(DesktopShared::default()),
        }),
    };
    desktop.owner.shared.state.lock().unwrap().snapshot.volume = Some(50);
    let cache = desktop.clone();
    let t = Instant::now();
    let logical_seconds = Arc::new(AtomicU64::new(0));
    let clock_seconds = logical_seconds.clone();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let script_events = event_tx.clone();
    let (hid, recorder) = crate::hid::HidDevice::test_device();
    let mut initial_hid = Some(hid);
    let mut bridge = QmkHidHostBridge::start_with_desktop_service(
        incident_target(),
        HostDataMode {
            volume: true,
            ..Default::default()
        },
        None,
        HostProtocol::Selected(false),
        desktop,
        move |_, _| {
            let Some(hid) = initial_hid.take() else {
                event_tx.send("reopened").unwrap();
                anyhow::bail!("scripted reconnect refused");
            };
            let clock_seconds = clock_seconds.clone();
            FAILURE_TIME.with(|clock| {
                *clock.borrow_mut() = Some(Box::new(move || {
                    t + Duration::from_secs(clock_seconds.load(Ordering::SeqCst))
                }))
            });
            let logical_seconds = logical_seconds.clone();
            let cache = cache.clone();
            let script_events = script_events.clone();
            let mut attempts = [0; 2];
            OUTPUT_SCRIPT.with(|script| {
                *script.borrow_mut() = Some(Box::new(move |packet| {
                    assert_eq!(packet[0], DATA_VOLUME);
                    let index = usize::from(packet[1] == 51);
                    attempts[index] += 1;
                    if index == 1 {
                        logical_seconds.store(6, Ordering::SeqCst);
                    }
                    if attempts[index] <= 2 {
                        anyhow::bail!("scripted transient output busy");
                    }
                    if index == 0 {
                        cache
                            .owner
                            .shared
                            .store_for_test(|snapshot| snapshot.volume = Some(51));
                    } else {
                        script_events.send("second recovered").unwrap();
                    }
                    Ok(())
                }))
            });
            Ok(HostDataHid::Dedicated(hid))
        },
    );
    let outcome = event_rx.recv_timeout(Duration::from_secs(5));
    bridge.suppress_shutdown();
    let worker = bridge.thread.take().unwrap();
    bridge.stop();
    worker.join().unwrap();
    assert_eq!(
        outcome.unwrap(),
        "second recovered",
        "second incident inherited the first grace timestamp"
    );
    assert_eq!(
        recorder
            .requests()
            .iter()
            .map(|p| p[..2].to_vec())
            .collect::<Vec<_>>(),
        vec![vec![DATA_VOLUME, 50], vec![DATA_VOLUME, 51]]
    );
}

#[test]
fn real_worker_owner_handoff_starts_a_new_failure_grace() {
    let desktop = HostDataService {
        owner: Arc::new(DesktopOwner {
            shared: Arc::new(DesktopShared::default()),
        }),
    };
    desktop.owner.shared.state.lock().unwrap().snapshot.volume = Some(50);
    let t = Instant::now();
    let seconds = Arc::new(AtomicU64::new(0));
    let (tx, rx) = std::sync::mpsc::channel();
    let (old_hid, old_recorder) = crate::hid::HidDevice::test_device();
    let (new_hid, new_recorder) = crate::hid::HidDevice::test_device();
    let mut hid = Some(old_hid);
    let mut bridge = QmkHidHostBridge::start_with_desktop_service(
        incident_target(),
        HostDataMode {
            volume: true,
            ..Default::default()
        },
        None,
        HostProtocol::Selected(false),
        desktop,
        move |_, _| {
            let Some(hid) = hid.take() else {
                tx.send("reopened").unwrap();
                anyhow::bail!("unexpected reconnect");
            };
            let clock = seconds.clone();
            FAILURE_TIME.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    t + Duration::from_secs(clock.load(Ordering::SeqCst))
                }))
            });
            let seconds = seconds.clone();
            let tx = tx.clone();
            let mut attempts = 0;
            OUTPUT_SCRIPT.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |_| {
                    attempts += 1;
                    match attempts {
                        1 => anyhow::bail!("temporary busy"),
                        2 => {
                            tx.send("first incident").unwrap();
                            anyhow::bail!("temporary busy");
                        }
                        3 | 4 => {
                            seconds.store(6, Ordering::SeqCst);
                            anyhow::bail!("new owner temporarily busy");
                        }
                        _ => {
                            tx.send("replacement recovered").unwrap();
                            Ok(())
                        }
                    }
                }))
            });
            Ok(HostDataHid::Dedicated(hid))
        },
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        "first incident"
    );
    assert!(bridge.control.handoff_selected(new_hid, false).is_ok());
    let outcome = rx.recv_timeout(Duration::from_secs(5));
    bridge.suppress_shutdown();
    let worker = bridge.thread.take().unwrap();
    bridge.stop();
    worker.join().unwrap();
    assert_eq!(
        outcome.unwrap(),
        "replacement recovered",
        "new owner retained an old incident timestamp"
    );
    assert!(old_recorder.requests().is_empty());
    assert_eq!(new_recorder.requests().len(), 1);
    assert_eq!(new_recorder.requests()[0][..2], [DATA_VOLUME, 50]);
}

#[test]
fn real_worker_reconnects_on_persistent_failure_and_resets_replacement_grace() {
    let desktop = HostDataService {
        owner: Arc::new(DesktopOwner {
            shared: Arc::new(DesktopShared::default()),
        }),
    };
    desktop.owner.shared.state.lock().unwrap().snapshot.volume = Some(50);
    let t = Instant::now();
    let seconds = Arc::new(AtomicU64::new(0));
    let (tx, rx) = std::sync::mpsc::channel();
    let (old_hid, old_recorder) = crate::hid::HidDevice::test_device();
    let (new_hid, new_recorder) = crate::hid::HidDevice::test_device();
    let mut owners = vec![new_hid, old_hid];
    let cache = desktop.clone();
    let mut opens = 0;
    let mut bridge = QmkHidHostBridge::start_with_desktop_service(
        incident_target(),
        HostDataMode {
            volume: true,
            ..Default::default()
        },
        None,
        HostProtocol::Selected(false),
        desktop,
        move |_, _| {
            opens += 1;
            // The real service clears cache when reconnect drops demand; inject
            // the replacement OS sample rather than keeping stale demand alive.
            cache.owner.shared.state.lock().unwrap().snapshot.volume = Some(50);
            let Some(hid) = owners.pop() else {
                tx.send("extra reconnect").unwrap();
                anyhow::bail!("unexpected extra reconnect");
            };
            let clock = seconds.clone();
            FAILURE_TIME.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    t + Duration::from_secs(clock.load(Ordering::SeqCst))
                }))
            });
            let seconds = seconds.clone();
            let tx = tx.clone();
            let owner = opens;
            let mut attempts = 0;
            OUTPUT_SCRIPT.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |_| {
                    attempts += 1;
                    if owner == 1 {
                        if attempts > 2 {
                            seconds.store(6, Ordering::SeqCst);
                        }
                        anyhow::bail!("persistent busy");
                    }
                    seconds.store(7, Ordering::SeqCst);
                    if attempts <= 2 {
                        anyhow::bail!("replacement transient busy");
                    }
                    tx.send("reconnected owner recovered").unwrap();
                    Ok(())
                }))
            });
            Ok(HostDataHid::Dedicated(hid))
        },
    );
    let outcome = rx.recv_timeout(Duration::from_secs(5));
    bridge.suppress_shutdown();
    let worker = bridge.thread.take().unwrap();
    bridge.stop();
    worker.join().unwrap();
    assert_eq!(outcome.unwrap(), "reconnected owner recovered");
    assert!(old_recorder.requests().is_empty());
    assert_eq!(new_recorder.requests().len(), 1);
    assert_eq!(new_recorder.requests()[0][..2], [DATA_VOLUME, 50]);
}

#[test]
fn fresh_timeline_seek_to_same_raw_position_reanchors_real_clock() {
    let t = Instant::now();
    let mut clock = MediaProgressClock::default();
    let mut update = |ticks, age, seconds| {
        clock.update(
            "track A",
            Some(Duration::ZERO),
            ticks,
            age,
            true,
            Some(90),
            t + Duration::from_secs(seconds),
        )
    };
    assert_eq!(update(Some(100), Some(Duration::ZERO), 0), Some(0));
    assert_eq!(
        update(Some(100), Some(Duration::from_secs(10)), 10),
        Some(10)
    );
    assert_eq!(
        update(Some(200), Some(Duration::ZERO), 11),
        Some(0),
        "new timeline at same raw position must reset the anchor"
    );
    assert_eq!(update(Some(200), Some(Duration::from_secs(2)), 13), Some(2));
}

#[test]
fn timeline_clock_preserves_stale_invalid_pause_and_unknown_controls() {
    let t = Instant::now();
    let mut c = MediaProgressClock::default();
    let zero = Some(Duration::ZERO);
    // Initial old sample extrapolates from its original publication time.
    assert_eq!(
        c.update(
            "A",
            zero,
            Some(100),
            Some(Duration::from_secs(5)),
            true,
            Some(90),
            t
        ),
        Some(5)
    );
    assert_eq!(
        c.update(
            "A",
            zero,
            Some(100),
            Some(Duration::from_secs(15)),
            true,
            Some(90),
            t + Duration::from_secs(10)
        ),
        Some(15)
    );
    // Missing, invalid or regressing timestamp alone cannot restart progress.
    for (ticks, age, second) in [
        (None, None, 11),
        (Some(0), Some(Duration::ZERO), 12),
        (Some(99), Some(Duration::ZERO), 13),
        (Some(101), None, 14),
    ] {
        assert_eq!(
            c.update(
                "A",
                zero,
                ticks,
                age,
                true,
                Some(90),
                t + Duration::from_secs(second)
            ),
            Some(5 + second as u32)
        );
    }
    assert_eq!(
        c.update(
            "A",
            zero,
            Some(100),
            Some(Duration::from_secs(20)),
            false,
            Some(90),
            t + Duration::from_secs(15)
        ),
        Some(0)
    );
    assert_eq!(
        c.update(
            "A",
            zero,
            Some(100),
            Some(Duration::from_secs(25)),
            true,
            Some(90),
            t + Duration::from_secs(20)
        ),
        Some(0)
    );
    assert_eq!(
        c.update(
            "A",
            Some(Duration::from_secs(30)),
            Some(100),
            None,
            true,
            Some(90),
            t + Duration::from_secs(21)
        ),
        Some(30)
    );
    assert_eq!(
        c.update(
            "B",
            zero,
            None,
            None,
            true,
            None,
            t + Duration::from_secs(22)
        ),
        Some(0)
    );
    assert_eq!(
        c.update(
            "B",
            zero,
            None,
            None,
            true,
            None,
            t + Duration::from_secs(24)
        ),
        Some(2)
    );
    assert_eq!(
        c.update(
            "unknown",
            None,
            None,
            None,
            true,
            None,
            t + Duration::from_secs(25)
        ),
        None
    );
}

#[test]
fn sm62_uninitialized_successor_shutdown_clears_retained_layout() {
    let owner = Arc::new(HostOutputOwner::default());
    let old = owner.claim_sm62(
        HostDataMode {
            volume: true,
            layout: true,
            ..Default::default()
        },
        false,
        true,
    );
    old.write(&[DATA_LAYOUT, 1], |_| Ok(())).unwrap();
    let next = owner.claim_sm62(
        HostDataMode {
            volume: true,
            ..Default::default()
        },
        false,
        true,
    );
    old.shutdown(|_| panic!("retired lease wrote")).unwrap();
    let mut packets = Vec::new();
    next.shutdown(|p| {
        packets.push(p.to_vec());
        Ok(())
    })
    .unwrap();
    assert_eq!(packets, vec![vec![DATA_LAYOUT, 255]]);
}

#[test]
fn sm62_layout_takeover_clears_before_successor_volume() {
    for explicit_shutdown in [false, true] {
        for sm62 in [false, true] {
            let owner = Arc::new(HostOutputOwner::default());
            let unrelated = Arc::new(HostOutputOwner::default());
            let other = unrelated.claim(
                HostDataMode {
                    volume: true,
                    ..Default::default()
                },
                false,
            );
            let old = owner.claim_sm62(
                HostDataMode {
                    volume: true,
                    layout: true,
                    ..Default::default()
                },
                false,
                sm62,
            );
            let mut packets = Vec::new();
            old.write(&[DATA_LAYOUT, 1], |p| {
                packets.push(p.to_vec());
                Ok(())
            })
            .unwrap();
            if explicit_shutdown {
                old.shutdown(|p| {
                    packets.push(p.to_vec());
                    Ok(())
                })
                .unwrap();
            }
            let next = owner.claim_sm62(
                HostDataMode {
                    volume: true,
                    ..Default::default()
                },
                false,
                sm62,
            );
            old.shutdown(|_| panic!("superseded shutdown wrote"))
                .unwrap();
            assert!(old
                .write(&[DATA_LAYOUT, 1], |_| panic!("superseded lease wrote"))
                .is_err());
            next.write(&[DATA_VOLUME, 50], |p| {
                packets.push(p.to_vec());
                Ok(())
            })
            .unwrap();
            let expected = if sm62 {
                vec![
                    vec![DATA_LAYOUT, 1],
                    vec![DATA_LAYOUT, 255],
                    vec![DATA_VOLUME, 50],
                ]
            } else {
                vec![vec![DATA_LAYOUT, 1], vec![DATA_VOLUME, 50]]
            };
            assert_eq!(
                packets, expected,
                "explicit={explicit_shutdown} sm62={sm62}"
            );
            assert!(other.is_current(), "another physical owner was revoked");
            other
                .shutdown(|_| panic!("unrelated owner got output"))
                .unwrap();
        }
    }
}
