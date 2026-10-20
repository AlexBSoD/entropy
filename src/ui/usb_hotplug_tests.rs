mod usb_hotplug_tests {
    use super::*;
    fn device(name: &str, bus: &str) -> Device {
        Device {
            name: name.into(),
            vendor_id: 0xFFFF,
            product_id: 0xFFFF,
            manufacturer: "fixture".into(),
            serial_number: name.into(),
            bus_type: bus.into(),
            path: format!("/nonexistent/entropy-hotplug-{name}"),
            instance_token: "original".into(),
            firmware: FirmwareProtocol::Vial,
        }
    }
    fn fixture() -> (EntropyApp, egui::Context, crate::hid::TestHidRecorder) {
        let app = EntropyApp::new_inert_for_test();
        let ctx = egui::Context::default();
        let (hid, recorder) = crate::hid::HidDevice::test_device();
        let mut app = app;
        app.device_manager
            .replace_devices(vec![device("K04", "Bluetooth")]);
        app.selected_device = Some(0);
        app.layout = Some(KeyboardLayout {
            name: "K04 fixture".into(),
            rows: 0,
            cols: 0,
            keys: vec![],
            encoders: vec![],
            layers: vec![vec![]],
            encoder_layers: vec![vec![]],
            layer_names: vec!["Base".into()],
            custom_keycodes: vec![],
            layout_options: vec![],
            live_features: Default::default(),
            supports_rgb: false,
            lighting_mode: None,
            firmware: FirmwareProtocol::Vial,
        });
        app.current_device_name = "K04".into();
        app.hid_device = Some(hid);
        app.last_device_scan_at = 10.0;
        (app, ctx, recorder)
    }
    #[test]
    fn usb_hotplug_unplug_first_usb_does_not_retire_retained_bluetooth_bridge() {
        let _storage = crate::app::app_storage::ScopedTempConfig::new();
        for usb_first in [true, false] {
            let (mut app, ctx, _) = fixture();
            let bt = app.device_manager.devices()[0].clone();
            let usb = device("Macropad", "Usb");
            let devices = if usb_first {
                vec![usb, bt.clone()]
            } else {
                vec![bt.clone(), usb]
            };
            app.device_manager.replace_devices(devices);
            app.selected_device = Some(if usb_first { 0 } else { 1 });
            let bridge = crate::qmk_hid_host::QmkHidHostBridge::test_inert(
                bt.clone(),
                Default::default(),
                None,
                crate::qmk_hid_host::HostProtocol::Discover,
            );
            app.qmk_hid_hosts.insert(bt.path.clone(), bridge);
            let (tx, rx) = mpsc::channel();
            tx.send(Ok(vec![])).unwrap();
            app.device_scan_state = DeviceScanState::Scanning {
                rx,
                started_at: std::time::Instant::now(),
                generation: app.connection_generation,
                timeout_logged: false,
                usb_only: true,
            };
            app.poll_device_scan(&ctx);
            assert!(app.qmk_hid_hosts.contains_key(&bt.path),"unplugged USB index must not resolve to retained Bluetooth owner: usb_first={usb_first}");
            assert_eq!(app.device_manager.devices()[0].path, bt.path);
            assert!(app.selected_device.is_none());
            assert!(app.layout.is_none());
            assert!(app.status_msg.is_empty());
        }
    }
    #[test]
    fn usb_hotplug_normal_unplug_shows_selection_not_open_error() {
        let _storage = crate::app::app_storage::ScopedTempConfig::new();
        for language in [
            crate::i18n::Language::Russian,
            crate::i18n::Language::English,
        ] {
            for reenumerated in [false, true] {
                let (mut app, ctx, _) = fixture();
                app.app_settings.language = language;
                let bt = app.device_manager.devices()[0].clone();
                let usb = device("Macropad", "Usb");
                app.device_manager.replace_devices(vec![bt, usb.clone()]);
                app.selected_device = Some(1);
                let (connect_tx, connect_rx) = mpsc::channel();
                app.test_connect_requests = Some(connect_tx);
                let (tx, rx) = mpsc::channel();
                let mut replacement = usb;
                replacement.instance_token = "new-instance".into();
                tx.send(Ok(if reenumerated {
                    vec![replacement]
                } else {
                    vec![]
                }))
                .unwrap();
                app.device_scan_state = DeviceScanState::Scanning {
                    rx,
                    started_at: std::time::Instant::now(),
                    generation: app.connection_generation,
                    timeout_logged: false,
                    usb_only: true,
                };
                app.poll_device_scan(&ctx);
                assert!(app.layout.is_none());
                assert!(app.hid_device.is_none());
                let mut output = None;
                for _ in 0..2 {
                    output = Some(ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let rect = ui.max_rect();
                            app.draw_device_selection_empty_state(ui, rect);
                        });
                    }));
                }
                let labels: Vec<_> = output
                    .unwrap()
                    .shapes
                    .iter()
                    .filter_map(|s| match &s.shape {
                        egui::Shape::Text(t) => Some(t.galley.job.text.clone()),
                        _ => None,
                    })
                    .collect();
                let heading = if language == crate::i18n::Language::Russian {
                    "Выберите клавиатуру"
                } else {
                    "Select keyboard"
                };
                assert!(
                    labels.iter().any(|s| s == heading),
                    "normal unplug is not an open failure: {labels:?}"
                );
                assert!(!labels.iter().any(|s| s.contains("Не удалось открыть")
                    || s.contains("Could not open")
                    || s.contains("USB device changed")));
                assert!(labels.iter().any(|s| s.contains("K04")));
                assert!(connect_rx.try_recv().is_err());
                // Real open failures still retain the error heading and detail.
                app.status_msg = "Open failed: fixture access denied".into();
                let output = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let rect = ui.max_rect();
                        app.draw_device_selection_empty_state(ui, rect);
                    });
                });
                assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text.contains("Не удалось открыть")||t.galley.job.text.contains("Could not open"))));
            }
        }
    }
    #[test]
    fn usb_hotplug_reconnect_scan_entrypoint_never_enumerates_retiring_bluetooth() {
        let (mut app, ctx, _) = fixture();
        let bt = app.device_manager.devices()[0].clone();
        app.hid_device = None;
        app.selected_device = None;
        let (owner_tx, owner_rx) = mpsc::channel();
        app.retiring_connects.push(RetiringConnect {
            device: bt.clone(),
            rx: owner_rx,
        });
        app.connect_state =
            ConnectState::Reconnecting(BluetoothReconnectState::new(bt.stable_identity(), bt.name));
        let (tx, rx) = mpsc::channel();
        app.test_device_scan_requests = Some(tx);
        app.maybe_start_bluetooth_reconnect_scan(&ctx);
        assert!(
            rx.try_recv()
                .expect("due reconnect can refresh USB inventory but not Bluetooth owners")
                .0
        );
        drop(owner_tx);
    }
    #[test]
    fn usb_hotplug_loading_and_retiring_bluetooth_owners_protect_after_selection_changes() {
        let _storage = crate::app::app_storage::ScopedTempConfig::new();
        for retired in [false, true] {
            let (mut app, ctx, _) = fixture();
            app.hid_device = None;
            let bluetooth = app.device_manager.devices()[0].clone();
            let usb = device("Macropad", "Usb");
            app.device_manager
                .replace_devices(vec![bluetooth.clone(), usb]);
            app.selected_device = Some(1);
            let (owner_tx, owner_rx) = mpsc::channel();
            if retired {
                app.retiring_connects.push(RetiringConnect {
                    device: bluetooth,
                    rx: owner_rx,
                });
            } else {
                app.connect_state = ConnectState::Loading {
                    device: bluetooth,
                    rx: owner_rx,
                    started_at: std::time::Instant::now(),
                    last_progress_at: std::time::Instant::now(),
                    cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    reconnect: None,
                };
            }
            let (tx, rx) = mpsc::channel();
            app.test_device_scan_requests = Some(tx);
            app.update_native_background(&ctx, 12.0, false, false);
            let (usb_only, _) = rx.try_recv().expect(
                "the actual Bluetooth owner, not editor selection, determines USB-only scanning",
            );
            assert!(usb_only, "retired={retired}");
            drop(owner_tx);
        }
    }
    #[test]
    fn usb_hotplug_inventory_cannot_restart_reenumerated_usb_or_bluetooth_reconnect() {
        let _storage = crate::app::app_storage::ScopedTempConfig::new();
        for reconnect in [false, true] {
            let (mut app, ctx, _) = fixture();
            let (connect_tx, connect_rx) = mpsc::channel();
            app.test_connect_requests = Some(connect_tx);
            if reconnect {
                let bt = app.device_manager.devices()[0].clone();
                let mut state = BluetoothReconnectState::new(bt.stable_identity(), bt.name);
                state.next_attempt_at =
                    std::time::Instant::now() + std::time::Duration::from_secs(60);
                app.connect_state = ConnectState::Reconnecting(state);
            } else {
                let usb = device("Macropad", "Usb");
                app.device_manager
                    .replace_devices(vec![app.device_manager.devices()[0].clone(), usb]);
                app.selected_device = Some(1);
            }
            let mut usb = device("Macropad", "Usb");
            usb.instance_token = "re-enumerated".into();
            let (done_tx, done_rx) = mpsc::channel();
            done_tx.send(Ok(vec![usb])).unwrap();
            app.device_scan_state = DeviceScanState::Scanning {
                rx: done_rx,
                started_at: std::time::Instant::now(),
                generation: app.connection_generation,
                timeout_logged: false,
                usb_only: true,
            };
            app.poll_device_scan(&ctx);
            assert!(
                connect_rx.try_recv().is_err(),
                "USB inventory is not a connection/reconnect trigger: reconnect={reconnect}"
            );
        }
    }
    #[test]
    fn usb_hotplug_borrowed_bluetooth_owner_still_dispatches_usb_only() {
        let _storage = crate::app::app_storage::ScopedTempConfig::new();
        let (mut app, ctx, recorder) = fixture();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut held = false;
        recorder.respond_by(move |_| {
            if !held {
                held = true;
                entered_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            }
            None
        });
        app.queue_tap_hold_setting_write("fixture".into(), 18, 2, 1, 20);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        assert!(app.hid_device.is_none());
        assert!(app.hid_write_task_owner_active());
        let generation = app.connection_generation;
        let (tx, rx) = mpsc::channel();
        app.test_device_scan_requests = Some(tx);
        app.update_native_background(&ctx, 12.0, false, true);
        let (usb_only, complete) = rx
            .try_recv()
            .expect("worker-borrowed BT owner still permits USB discovery only");
        assert!(usb_only);
        complete.send(Ok(vec![device("Macropad", "Usb")])).unwrap();
        app.update_native_background(&ctx, 12.1, false, true);
        assert_eq!(app.connection_generation, generation);
        assert!(app.settings_write_task.is_some());
        assert_eq!(
            app.device_manager.devices()[app.selected_device.unwrap()].name,
            "K04"
        );
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.settings_write_task.is_some() && std::time::Instant::now() < deadline {
            app.poll_settings_write(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(app.hid_device.is_some());
        assert_eq!(app.connection_generation, generation);
    }
    #[test]
    fn usb_hotplug_keeps_one_pending_scan_and_ignores_retired_generation() {
        let _storage = crate::app::app_storage::ScopedTempConfig::new();
        let (mut app, ctx, _) = fixture();
        let (tx, rx) = mpsc::channel();
        app.test_device_scan_requests = Some(tx);
        app.update_native_background(&ctx, 12.0, false, true);
        let (usb_only, complete) = rx.try_recv().unwrap();
        assert!(usb_only);
        app.update_native_background(&ctx, 14.0, false, true);
        assert!(rx.try_recv().is_err(), "single worker only");
        app.connection_generation = app.connection_generation.wrapping_add(1);
        complete
            .send(Ok(vec![device("StaleMacropad", "Usb")]))
            .unwrap();
        app.update_native_background(&ctx, 14.1, false, true);
        assert_eq!(app.device_manager.devices().len(), 1);
        assert!(!app
            .device_manager
            .devices()
            .iter()
            .any(|d| d.name == "StaleMacropad"));
    }
    #[test]
    fn usb_hotplug_does_not_poll_while_hidden_to_tray() {
        let (mut app, ctx, _) = fixture();
        let (tx, rx) = mpsc::channel();
        app.test_device_scan_requests = Some(tx);
        app.update_native_background(&ctx, 12.0, true, true);
        assert!(rx.try_recv().is_err());
        app.update_native_background(&ctx, 12.1, false, true);
        assert!(rx.try_recv().unwrap().0);
    }
    #[test]
    fn usb_hotplug_while_bluetooth_selected_keeps_owner_and_selection() {
        let _storage = crate::app::app_storage::ScopedTempConfig::new();
        let (mut app, ctx, recorder) = fixture();
        let old_generation = app.connection_generation;
        let old_layout = format!("{:?}", app.layout);
        let (tx, rx) = mpsc::channel();
        app.test_device_scan_requests = Some(tx);
        let (connect_tx, connect_rx) = mpsc::channel();
        app.test_connect_requests = Some(connect_tx);
        app.update_native_background(&ctx, 11.9, false, true);
        assert!(
            rx.try_recv().is_err(),
            "do not scan before the two-second deadline"
        );
        app.update_native_background(&ctx, 12.0, false, true);
        let (usb_only, complete) = rx
            .try_recv()
            .expect("active Bluetooth must still dispatch USB discovery at two seconds");
        assert!(usb_only, "never run the full HID/Bluetooth enumerator");
        let macropad = device("Macropad", "Usb");
        complete.send(Ok(vec![macropad.clone()])).unwrap();
        app.update_native_background(&ctx, 12.1, false, true);
        assert_eq!(app.device_manager.devices().len(), 2);
        assert_eq!(
            app.device_manager.devices()[app.selected_device.unwrap()].name,
            "K04"
        );
        assert_eq!(app.connection_generation, old_generation);
        assert!(app.hid_device.is_some());
        assert_eq!(format!("{:?}", app.layout), old_layout);
        assert!(
            connect_rx.try_recv().is_err(),
            "listing a new USB device must not read its firmware or auto-select it"
        );
        assert!(
            recorder.requests().is_empty(),
            "discovery does not write/read the Bluetooth transport"
        );
        app.update_native_background(&ctx, 13.9, false, true);
        assert!(rx.try_recv().is_err());
        app.update_native_background(&ctx, 14.0, false, true);
        let (usb_only, complete) = rx.try_recv().unwrap();
        assert!(usb_only);
        complete.send(Ok(vec![])).unwrap();
        app.update_native_background(&ctx, 14.1, false, true);
        assert_eq!(app.device_manager.devices().len(), 1);
        assert_eq!(app.selected_device, Some(0));
        assert_eq!(app.connection_generation, old_generation);
        assert!(app.hid_device.is_some());
        // New enumeration failure must retain both the live BT owner and USB inventory.
        app.update_native_background(&ctx, 16.0, false, true);
        let (_, complete) = rx.try_recv().unwrap();
        complete
            .send(Err("USB enumeration fixture failure".into()))
            .unwrap();
        app.update_native_background(&ctx, 16.1, false, true);
        assert_eq!(app.device_manager.devices().len(), 1);
        assert_eq!(app.connection_generation, old_generation);
        assert!(connect_rx.try_recv().is_err());
        assert!(recorder.requests().is_empty());
    }
}
