//! Real layout/menu/file-dispatch code; only native picker and HID are replaced.
//! Each test owns an isolated output root, including settings and auto-backups.
use super::*;
use crate::application_layouts::{
    DetectedApplication, DeviceApplicationLayouts, DEFAULT_APPLICATION_LAYOUT_ID,
};

fn fixture() -> (EntropyApp, crate::hid::TestHidRecorder, String) {
    let mut app = EntropyApp::new_inert_for_test();
    let device = Device {
        name: "M4CR0Pad v3".into(),
        vendor_id: 0xE126,
        product_id: 0x0042,
        manufacturer: "Ergohaven".into(),
        serial_number: "layout-test".into(),
        bus_type: "Usb".into(),
        path: "inert-entlayout-macropad".into(),
        instance_token: "layout-test".into(),
        firmware: FirmwareProtocol::Vial,
    };
    app.device_about_info = Some(DeviceAboutInfo {
        supports_application_layouts: true,
        vendor_id: device.vendor_id,
        product_id: device.product_id,
        path: device.path.clone(),
        ..Default::default()
    });
    app.device_manager.replace_devices(vec![device]);
    app.selected_device = Some(0);
    app.current_keyboard_id = Some(42);
    let mut layout = KeyboardLayout::from_vial_json(&serde_json::json!({
        "name": "M4CR0Pad v3", "matrix": {"rows": 5, "cols": 3},
        "layouts": {"keymap": [["1,0", "1,1", "0,2", "0,0\n\n\n\n\n\n\n\n\ne"]]}
    }))
    .unwrap();
    let mut other_direction = layout.encoders[0].clone();
    other_direction.direction = 1 - other_direction.direction;
    other_direction.x += 1.0;
    layout.encoders.push(other_direction);
    layout.layers = vec![vec![4.into(); layout.keys.len()]; 8];
    layout.encoder_layers = vec![vec![5; layout.encoders.len()]; 8];
    app.layout = Some(layout);
    app.layer_count = 8;
    app.layer_names = (0..8).map(|i| format!("Firmware {i}")).collect();
    app.vial_unlocked = Some(true);
    app.default_layout_device_reconciled = true;
    let (hid, recorder) = crate::hid::HidDevice::test_device();
    app.shared_hid_output = hid.shared_output();
    app.hid_device = Some(hid);
    let key = app.application_layout_device_key().unwrap();
    let mut settings = DeviceApplicationLayouts::default();
    let application = settings.create_for_application(&DetectedApplication {
        executable: "vial".into(),
        display_name: "Vial".into(),
        ..Default::default()
    });
    settings
        .layouts
        .get_mut(&application)
        .unwrap()
        .set_keycode(0, 0, 7);
    settings
        .layouts
        .get_mut(DEFAULT_APPLICATION_LAYOUT_ID)
        .unwrap()
        .set_keycode(0, 0, 6);
    settings
        .layouts
        .get_mut(DEFAULT_APPLICATION_LAYOUT_ID)
        .unwrap()
        .layer_names[0] = "My Default".into();
    app.app_settings.application_layouts.insert(key, settings);
    for id in [DEFAULT_APPLICATION_LAYOUT_ID, application.as_str()] {
        app.application_layout_settings_mut()
            .unwrap()
            .layouts
            .get_mut(id)
            .unwrap()
            .set_encoder_stack(
                0,
                vec![
                    crate::application_layouts::EncoderStackAction {
                        name: "Volume".into(),
                        counter_clockwise: 0x0104,
                        clockwise: 0x0105,
                    },
                    crate::application_layouts::EncoderStackAction {
                        name: "Scroll".into(),
                        counter_clockwise: 0x0204,
                        clockwise: 0x0205,
                    },
                ],
            );
    }
    (app, recorder, application)
}

#[test]
fn macropad_application_navigation_round_trip_reloads_all_keys() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    let path = _storage.path().join("navigation.entlayout");
    let source = if let Ok(private_path) = std::env::var("ENTROPY_PROFILE_REPLAY") {
        assert!(
            app.import_application_layout_from_path(Path::new(&private_path), &application)
                .is_err(),
            "the obsolete input must not be accepted as a layout"
        );
        let bytes = std::fs::read(private_path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Replay only the captured content, not its obsolete file envelope.
        serde_json::from_value(value["profile"].clone()).unwrap()
    } else {
        let profile = app
            .application_layout_settings_mut()
            .unwrap()
            .layouts
            .get_mut(&application)
            .unwrap();
        profile.layers[0][..5].copy_from_slice(&[4, 0x00A9, 0x00F0, 0x7E05, 0x7E06]);
        let source = profile.clone();
        source
    };
    let source: crate::application_layouts::ApplicationLayout = source;
    write_application_entlayout(&path, source.clone(), app.app_settings.language).unwrap();
    let before = app.application_layout_settings().unwrap().clone();
    app.import_application_layout_from_path(&path, &application)
        .unwrap();
    // Restore through the production writer and reader. All 16x15 controls
    // and encoder stacks must survive disk reload.
    let modern = _storage.path().join("navigation.entlayout");
    app.write_application_layout_export(&modern, &application)
        .unwrap();
    let profile = app
        .application_layout_settings_mut()
        .unwrap()
        .layouts
        .get_mut(&application)
        .unwrap();
    profile.layers[0].fill(42);
    profile.layer_names[0] = "Changed".into();
    profile.encoder_stacks[0].clear();
    app.import_application_layout_from_path(&modern, &application)
        .unwrap();
    let restored: AppSettings =
        serde_json::from_slice(&std::fs::read(app_settings_path()).unwrap()).unwrap();
    let key = app.application_layout_device_key().unwrap();
    let target = &restored.application_layouts[&key].layouts[&application];
    assert_eq!(target.layers, source.layers);
    assert_eq!(target.layer_names, source.layer_names);
    assert_eq!(target.encoder_stacks, source.encoder_stacks);
    assert_eq!(target.id, before.layouts[&application].id);
    assert_eq!(
        restored.application_layouts[&key].layouts[DEFAULT_APPLICATION_LAYOUT_ID],
        before.layouts[DEFAULT_APPLICATION_LAYOUT_ID]
    );
    assert_eq!(
        restored.application_layouts[&key].active_layout_id,
        before.active_layout_id
    );
    assert!(recorder.requests().is_empty());
}

#[test]
fn macropad_application_entlayout_envelope_is_versioned_and_not_a_device_layout() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    let path = _storage.path().join("application.entlayout");
    app.write_application_layout_export(&path, &application)
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["format"], "entropy.layout");
    assert_eq!(value["version"], 8);
    assert_eq!(value["scope"], "application");
    let before = serde_json::to_value(&app.app_settings).unwrap();
    let error = app
        .import_entlayout_from_path(&path)
        .unwrap_err()
        .to_string();
    assert!(error.contains("explicit application target"), "{error}");
    assert_eq!(serde_json::to_value(&app.app_settings).unwrap(), before);
    app.application_layout_settings_mut()
        .unwrap()
        .layouts
        .get_mut(&application)
        .unwrap()
        .set_keycode(0, 0, 42);
    app.import_application_layout_from_path(&path, &application)
        .unwrap();
    assert_eq!(
        app.application_layout_settings().unwrap().layouts[&application].layers[0][0],
        7
    );
    let mut invalid = value.clone();
    invalid["version"] = serde_json::json!(9);
    std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
    assert!(app
        .import_application_layout_from_path(&path, &application)
        .is_err());
    assert!(recorder.requests().is_empty());
    // Characterize unchanged Default/device schema and explicit target boundary.
    let device = app.entlayout_snapshot().unwrap();
    let device_path = _storage.path().join("default.entlayout");
    write_entlayout_file(&device_path, &device, app.app_settings.language).unwrap();
    let device_value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&device_path).unwrap()).unwrap();
    assert_eq!(device_value["format"], "entropy.layout");
    assert_eq!(device_value["version"], 7);
    assert!(device_value.get("scope").is_none());
    let before = serde_json::to_value(&app.app_settings).unwrap();
    assert!(app
        .import_application_layout_from_path(&device_path, &application)
        .is_err());
    for (format, version, scope) in [
        ("entropy.layout", 7, Some("application")),
        ("entropy.layout", 8, Some("unknown")),
        ("entropy.layout", 8, None),
        ("entropy.application-layout", 2, None),
        ("entropy.application-layout", 1, Some("application")),
    ] {
        let mut malformed = value.clone();
        malformed["format"] = serde_json::json!(format);
        malformed["version"] = serde_json::json!(version);
        if let Some(scope) = scope {
            malformed["scope"] = serde_json::json!(scope);
        } else {
            malformed.as_object_mut().unwrap().remove("scope");
        }
        std::fs::write(&path, serde_json::to_vec(&malformed).unwrap()).unwrap();
        assert!(app
            .import_application_layout_from_path(&path, &application)
            .is_err());
        assert_eq!(serde_json::to_value(&app.app_settings).unwrap(), before);
    }
}

#[test]
fn macropad_application_native_picker_only_offers_entlayout() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, application) = fixture();
    for export in [true, false] {
        crate::app::file_dialog::set_test_dialog_result(None);
        app.application_layout_file_dialog(application.clone(), export);
        let options = crate::app::file_dialog::test_dialog_options();
        assert!(options.contains(".entlayout"), "{options}");
        assert!(options.contains("\"entlayout\""), "{options}");
        assert!(!options.contains("entapp"), "{options}");
        app.poll_file_dialog(&egui::Context::default());
    }
}

#[test]
fn macropad_application_rejects_legacy_format_and_other_extensions_without_changes() {
    let storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    let before = serde_json::to_value(&app.app_settings).unwrap();
    let profile = app.application_layout_settings().unwrap().layouts[&application].clone();
    for extension in ["entlayout", "entapp"] {
        let path = storage.path().join(format!("legacy.{extension}"));
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "format": "entropy.application-layout", "version": 1, "profile": profile
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(app
            .import_application_layout_from_path(&path, &application)
            .is_err());
    }
    let modern = storage.path().join("modern.entlayout");
    app.write_application_layout_export(&modern, &application)
        .unwrap();
    for extension in ["entapp", "json"] {
        let path = storage.path().join(format!("modern.{extension}"));
        std::fs::copy(&modern, &path).unwrap();
        assert!(app
            .import_application_layout_from_path(&path, &application)
            .is_err());
        assert!(app
            .write_application_layout_export(&path, &application)
            .is_err());
    }
    assert_eq!(serde_json::to_value(&app.app_settings).unwrap(), before);
    assert!(!app_settings_path().exists());
    assert!(recorder.requests().is_empty());
}

#[test]
fn macropad_entlayout_duplicate_fields_rejected_before_any_writes() {
    let storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    let device = serde_json::to_value(app.entlayout_snapshot().unwrap()).unwrap();
    let path = storage.path().join("duplicate.entlayout");
    app.write_application_layout_export(&path, &application)
        .unwrap();
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let before = serde_json::to_value(&app.app_settings).unwrap();
    let backups = app_settings_path().parent().unwrap().join("backups");
    for (value, field, contents, application_scope) in [
        (&device, "version", &device["version"], false),
        (&device, "data", &device["data"], false),
        (&device, "keymap", &device["data"]["keymap"], false),
        (&profile, "version", &profile["version"], true),
        (&profile, "profile", &profile["profile"], true),
        (&profile, "layers", &profile["profile"]["layers"], true),
    ] {
        let json = serde_json::to_string(value).unwrap();
        let member = format!("\"{field}\":{}", serde_json::to_string(contents).unwrap());
        let duplicate = json.replacen(&member, &format!("{member},{member}"), 1);
        assert_ne!(duplicate, json);
        std::fs::write(&path, duplicate).unwrap();
        let error = if application_scope {
            app.import_application_layout_from_path(&path, &application)
                .unwrap_err()
        } else {
            app.import_entlayout_from_path(&path).unwrap_err()
        };
        assert!(
            format!("{error:#}").contains("duplicate field"),
            "{field}: {error:#}"
        );
        assert_eq!(serde_json::to_value(&app.app_settings).unwrap(), before);
        assert!(!app_settings_path().exists());
        assert!(!backups.exists());
        assert!(recorder.requests().is_empty());
    }
}

#[test]
fn macropad_application_file_round_trip_targets_only_explicit_profile() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    app.application_layout_settings_mut()
        .unwrap()
        .layouts
        .get_mut(&application)
        .unwrap()
        .set_encoder_stack(
            0,
            vec![
                crate::application_layouts::EncoderStackAction {
                    name: "Zoom".into(),
                    counter_clockwise: 0x0104,
                    clockwise: 0x0105,
                },
                crate::application_layouts::EncoderStackAction {
                    name: "Scroll".into(),
                    counter_clockwise: 0x0204,
                    clockwise: 0x0205,
                },
            ],
        );
    let before = app.application_layout_settings().unwrap().clone();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("application.entlayout");
    app.write_application_layout_export(&path, &application)
        .unwrap();
    app.application_layout_settings_mut()
        .unwrap()
        .layouts
        .get_mut(&application)
        .unwrap()
        .set_keycode(0, 0, 42);
    app.import_application_layout_from_path(&path, &application)
        .unwrap();
    let settings = app.application_layout_settings().unwrap();
    assert_eq!(
        settings.layouts[&application].layers,
        before.layouts[&application].layers
    );
    assert_eq!(
        settings.layouts[&application].encoder_stacks,
        before.layouts[&application].encoder_stacks
    );
    assert_eq!(
        settings.layouts[DEFAULT_APPLICATION_LAYOUT_ID],
        before.layouts[DEFAULT_APPLICATION_LAYOUT_ID]
    );
    assert_eq!(settings.active_layout_id, before.active_layout_id);
    assert!(
        recorder.requests().is_empty(),
        "profile import must not rewrite shared firmware state"
    );
}

#[test]
fn macropad_application_persistence_failure_keeps_memory_library_and_existing_destination() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    let input = _storage.path().join("application.entlayout");
    app.write_application_layout_export(&input, &application)
        .unwrap();
    app.application_layout_settings_mut()
        .unwrap()
        .layouts
        .get_mut(&application)
        .unwrap()
        .set_keycode(0, 0, 42);
    let before = serde_json::to_value(&app.app_settings).unwrap();
    write_pictogram_file(
        &pictogram_library_path(),
        &app.app_settings.saved_pictograms,
    )
    .unwrap();
    let library_before = std::fs::read(pictogram_library_path()).unwrap();
    // A directory at the destination makes atomic persist fail even as root.
    let destination = app_settings_path();
    std::fs::create_dir(&destination).unwrap();
    let sentinel = destination.join("keep");
    std::fs::write(&sentinel, b"untouched").unwrap();
    let error = app
        .import_application_layout_from_path(&input, &application)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Failed to save application layout"),
        "{error:#}"
    );
    assert_eq!(serde_json::to_value(&app.app_settings).unwrap(), before);
    assert_eq!(
        std::fs::read(pictogram_library_path()).unwrap(),
        library_before
    );
    assert_eq!(std::fs::read(sentinel).unwrap(), b"untouched");
    assert!(recorder.requests().is_empty());
    // PersistError owns the temporary file until the reported error is dropped.
    drop(error);
    assert!(std::fs::read_dir(destination.parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")));
}

#[test]
fn macropad_application_menu_shows_explicit_copy_target() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, application) = fixture();
    app.application_layout_settings_mut()
        .unwrap()
        .editor_layout_id = application;
    let ctx = egui::Context::default();
    let mut texts = Vec::new();
    for _ in 0..2 {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1100.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| {
                app.draw_layout_device_dropdown(
                    ui,
                    &ctx,
                    crate::i18n::Language::English,
                    Some(egui::Rect::from_min_size(
                        egui::pos2(400.0, 0.0),
                        egui::vec2(80.0, 30.0),
                    )),
                    true,
                    false,
                    false,
                );
            },
        );
        texts = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect();
    }
    assert!(
        texts.iter().any(|text| text == "Export application: Vial"),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text == "Import into application: Vial"),
        "{texts:?}"
    );
}

#[test]
fn macropad_application_menu_clicks_reach_async_picker_and_profile_file_dispatch() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    for export in [true, false] {
        let (mut app, recorder, application) = fixture();
        app.application_layout_settings_mut()
            .unwrap()
            .editor_layout_id = application.clone();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clicked.entlayout");
        if !export {
            app.write_application_layout_export(&path, &application)
                .unwrap();
            app.application_layout_settings_mut()
                .unwrap()
                .layouts
                .get_mut(&application)
                .unwrap()
                .set_keycode(0, 0, 88);
        }
        let ctx = egui::Context::default();
        let draw = |app: &mut EntropyApp, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1100.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    app.draw_layout_device_dropdown(
                        ui,
                        &ctx,
                        crate::i18n::Language::English,
                        Some(egui::Rect::from_min_size(
                            egui::pos2(400.0, 0.0),
                            egui::vec2(80.0, 30.0),
                        )),
                        true,
                        false,
                        false,
                    );
                },
            )
        };
        let _ = draw(&mut app, vec![]);
        let output = draw(&mut app, vec![]);
        let label = if export {
            "Export application: Vial"
        } else {
            "Import into application: Vial"
        };
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap();
        crate::app::file_dialog::set_test_dialog_result(Some(path.clone()));
        for pressed in [true, false] {
            let _ = draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let expected = if export {
            crate::app::file_dialog::FileDialogAction::ExportApplicationLayout(application.clone())
        } else {
            crate::app::file_dialog::FileDialogAction::ImportApplicationLayout(application.clone())
        };
        assert_eq!(
            app.pending_file_dialog.as_ref().map(|pending| &pending.0),
            Some(&expected)
        );
        app.poll_file_dialog(&ctx);
        assert!(app.pending_file_dialog.is_none());
        assert!(path.exists());
        assert_eq!(
            app.application_layout_settings().unwrap().layouts[&application].layers[0][0],
            7
        );
        assert!(recorder.requests().is_empty());
    }
}

#[test]
fn macropad_application_picker_dispatch_keeps_captured_target_when_selection_changes() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    use crate::app::file_dialog::FileDialogAction;
    let (mut app, recorder, application) = fixture();
    let other = app
        .application_layout_settings_mut()
        .unwrap()
        .create_for_application(&DetectedApplication {
            executable: "other".into(),
            display_name: "Other".into(),
            ..Default::default()
        });
    let original = app.application_layout_settings().unwrap().clone();
    app.keycode_picker.macro_texts = vec![b"shared macro".to_vec()];
    app.keycode_picker.macro_names = vec!["Shared macro".into()];
    app.combo_entries = vec![ComboEntry {
        keys: [4, 5, 0, 0],
        output: 6.into(),
        layer: None,
    }];
    app.app_settings.saved_pictograms.push(SavedPictogram {
        origin: PictogramOrigin::Legacy,
        name: "Shared artwork".into(),
        color: [1, 2, 3],
        bitmap: vec![0x55; PICTOGRAM_BYTES],
    });
    let shared_before = serde_json::to_value(&app.app_settings.saved_pictograms).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("captured.entlayout");
    let ctx = egui::Context::default();
    for export in [true, false] {
        if !export {
            app.application_layout_settings_mut()
                .unwrap()
                .layouts
                .get_mut(&other)
                .unwrap()
                .set_keycode(0, 0, 99);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let action = if export {
            FileDialogAction::ExportApplicationLayout(application.clone())
        } else {
            FileDialogAction::ImportApplicationLayout(other.clone())
        };
        app.pending_file_dialog = Some((action, app.connection_generation, rx));
        app.application_layout_settings_mut()
            .unwrap()
            .editor_layout_id = DEFAULT_APPLICATION_LAYOUT_ID.into();
        tx.send(Some(path.clone())).unwrap();
        app.poll_file_dialog(&ctx);
        assert!(app.pending_file_dialog.is_none());
        assert!(!app.status_msg.contains("failed"), "{}", app.status_msg);
    }
    let settings = app.application_layout_settings().unwrap();
    assert_eq!(
        settings.layouts[&other].layers,
        original.layouts[&application].layers
    );
    assert_eq!(
        settings.layouts[&other].encoder_stacks,
        original.layouts[&application].encoder_stacks
    );
    assert_eq!(settings.layouts[&other].executable, "other");
    assert_eq!(settings.layouts[&other].id, other);
    assert_eq!(
        settings.layouts[&application],
        original.layouts[&application]
    );
    assert_eq!(
        settings.layouts[DEFAULT_APPLICATION_LAYOUT_ID],
        original.layouts[DEFAULT_APPLICATION_LAYOUT_ID]
    );
    assert_eq!(
        serde_json::to_value(&app.app_settings.saved_pictograms).unwrap(),
        shared_before
    );
    let persisted: AppSettings =
        serde_json::from_slice(&std::fs::read(app_settings_path()).unwrap()).unwrap();
    assert_eq!(
        app.keycode_picker.macro_texts,
        vec![b"shared macro".to_vec()]
    );
    assert_eq!(app.keycode_picker.macro_names, vec!["Shared macro"]);
    assert_eq!(app.combo_entries[0].keys, [4, 5, 0, 0]);
    assert_eq!(
        persisted.application_layouts,
        app.app_settings.application_layouts
    );
    assert!(recorder.requests().is_empty());
}

#[test]
fn macropad_application_export_rejects_unimportable_keys_and_encoder_actions_without_writing() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, application) = fixture();
    let path = _storage.path().join("unsupported.entlayout");
    std::fs::write(&path, b"existing export").unwrap();
    for code in [
        0x2000, 0x5700, 0x5F00, 0x7700, 0x7E00, 0x7E04, 0x7E07, 0x7EFF,
    ] {
        for encoder in [false, true] {
            let profile = app
                .application_layout_settings_mut()
                .unwrap()
                .layouts
                .get_mut(&application)
                .unwrap();
            profile.set_keycode(0, 0, if encoder { 4 } else { code });
            profile.encoder_stacks[0][0].clockwise = if encoder { code } else { 0x00A9 };
            let error = app
                .write_application_layout_export(&path, &application)
                .unwrap_err()
                .to_string();
            assert!(error.contains("Safe resource remapping"), "{error}");
            assert_eq!(std::fs::read(&path).unwrap(), b"existing export");
        }
    }
}

#[test]
fn macropad_application_import_rejects_shared_resource_ids_without_partial_changes() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resource.entlayout");
    for code in [0x7700, 0x5700, 0x5f00, 0x7e00] {
        app.application_layout_settings_mut()
            .unwrap()
            .layouts
            .get_mut(&application)
            .unwrap()
            .set_keycode(0, 0, code);
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "format": "entropy.layout", "version": 8, "scope": "application",
                "profile": app.application_layout_settings().unwrap().layouts[&application]
            }))
            .unwrap(),
        )
        .unwrap();
        let before = serde_json::to_value(&app.app_settings).unwrap();
        let error = app
            .import_application_layout_from_path(&path, &application)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Safe resource remapping"), "{error}");
        assert_eq!(serde_json::to_value(&app.app_settings).unwrap(), before);
    }
    assert!(recorder.requests().is_empty());
}

#[test]
fn macropad_application_picker_rejects_stale_connection_and_deleted_target() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, application) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale.entlayout");
    let (tx, rx) = std::sync::mpsc::channel();
    app.pending_file_dialog = Some((
        crate::app::file_dialog::FileDialogAction::ExportApplicationLayout(application.clone()),
        app.connection_generation,
        rx,
    ));
    app.connection_generation += 1;
    tx.send(Some(path.clone())).unwrap();
    app.poll_file_dialog(&egui::Context::default());
    assert!(!path.exists());
    app.write_application_layout_export(&path, &application)
        .unwrap();
    app.application_layout_settings_mut()
        .unwrap()
        .layouts
        .remove(&application);
    let before = serde_json::to_value(&app.app_settings).unwrap();
    assert!(app
        .import_application_layout_from_path(&path, &application)
        .is_err());
    assert!(app
        .import_application_layout_from_path(&path, DEFAULT_APPLICATION_LAYOUT_ID)
        .is_err());
    assert_eq!(serde_json::to_value(&app.app_settings).unwrap(), before);
}

#[test]
fn macropad_default_universal_reordered_mapping_imports_encoders_losslessly_and_keeps_stacks() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, application) = fixture();
    let stacks = app.application_layout_settings().unwrap().layouts[DEFAULT_APPLICATION_LAYOUT_ID]
        .encoder_stacks
        .clone();
    let other = app.application_layout_settings().unwrap().layouts[&application].clone();
    let mut bundle = app.entlayout_snapshot().unwrap();
    bundle.keyboard.layout_hash = "reordered source".into();
    bundle.data.source_layout.as_mut().unwrap().keys.swap(0, 1);
    for layer in &mut bundle.data.keymap {
        layer.swap(0, 1);
    }
    for layer in &mut bundle.data.native_keymap {
        layer.swap(0, 1);
    }
    bundle.data.keymap[0][0] = 11;
    bundle.data.keymap[0][1] = 12;
    bundle.data.encoder_keymap[0][0] = 0x0104;
    bundle.data.encoder_keymap[0][1] = 0x0205;
    let mapping = app.entlayout_import_mapping(&bundle).unwrap();
    assert!(!mapping.exact_layout);
    assert_eq!(mapping.key_mapping[0], Some(1));
    assert_eq!(mapping.key_mapping[1], Some(0));
    app.apply_entlayout(&bundle).unwrap();
    let layout = app.layout.as_ref().unwrap();
    assert_eq!(layout.get_keycode(0, 0), 12);
    assert_eq!(layout.get_keycode(0, 1), 11);
    assert_eq!(layout.get_encoder_keycode(0, 0), 0x0104);
    assert_eq!(layout.get_encoder_keycode(0, 1), 0x0205);
    let settings = app.application_layout_settings().unwrap();
    assert_eq!(
        settings.layouts[DEFAULT_APPLICATION_LAYOUT_ID].encoder_stacks,
        stacks
    );
    assert_eq!(settings.layouts[&application], other);
    let rendered = app.application_layout_rendered_copy(layout);
    assert_eq!(rendered.get_keycode(0, 0), 12);
    assert_eq!(rendered.get_encoder_keycode(0, 0), 0x0104);
    assert_eq!(rendered.get_encoder_keycode(0, 1), 0x0205);
}

#[test]
fn macropad_default_export_uses_default_storage_not_device_or_active_application() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, application) = fixture();
    let settings = app.application_layout_settings_mut().unwrap();
    settings.active_layout_id = application.clone();
    settings.editor_layout_id = application;
    let before = serde_json::to_value(app.application_layout_settings().unwrap()).unwrap();
    let bundle = app.entlayout_snapshot().unwrap();
    assert_eq!(
        bundle.data.keymap[0][0], 6,
        "Default export must include pending/local Default edits"
    );
    assert_eq!(bundle.data.layer_names[0], "My Default");
    assert_eq!(
        bundle.keyboard.layers, 8,
        "Default overlay must not invent device layers"
    );
    assert_eq!(bundle.data.layer_names.len(), bundle.keyboard.layers);
    assert_eq!(
        bundle.data.keymap[0][1], 4,
        "unset profile control must inherit firmware, not export 0xffff"
    );
    assert_eq!(bundle.data.encoder_keymap[0][0], 5);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exported-default.entlayout");
    let (tx, rx) = std::sync::mpsc::channel();
    app.pending_file_dialog = Some((
        crate::app::file_dialog::FileDialogAction::ExportEntlayout,
        app.connection_generation,
        rx,
    ));
    tx.send(Some(path.clone())).unwrap();
    app.poll_file_dialog(&egui::Context::default());
    let written: EntLayoutFile =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(written.data.keymap, bundle.data.keymap);
    assert_eq!(written.data.layer_names, bundle.data.layer_names);
    assert_eq!(
        serde_json::to_value(app.application_layout_settings().unwrap()).unwrap(),
        before
    );
}

#[test]
fn macropad_default_import_updates_rendered_default_and_preserves_application_profiles() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, recorder, application) = fixture();
    let profile_before =
        serde_json::to_value(&app.application_layout_settings().unwrap().layouts[&application])
            .unwrap();
    let mut bundle = app.entlayout_snapshot().unwrap();
    bundle.data.keymap[0][0] = 9;
    bundle.data.layer_names[0] = "Restored Default".into();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default.entlayout");
    write_entlayout_file(&path, &bundle, app.app_settings.language).unwrap();
    let ctx = egui::Context::default();
    let (tx, rx) = std::sync::mpsc::channel();
    app.pending_file_dialog = Some((
        crate::app::file_dialog::FileDialogAction::ImportEntlayout,
        app.connection_generation,
        rx,
    ));
    tx.send(Some(path)).unwrap();
    app.poll_file_dialog(&ctx);
    let (path, generation) = app
        .pending_entlayout_import_path
        .take()
        .expect("picker result queues import");
    assert_eq!(generation, app.connection_generation);
    let before_import = app.entlayout_snapshot().unwrap();
    app.import_entlayout_from_path(&path).unwrap();
    let backup_dir = _storage.path().join("entropy/backups");
    let backups: Vec<_> = std::fs::read_dir(&backup_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1, "import must persist its real auto-backup");
    let backup: EntLayoutFile =
        serde_json::from_slice(&std::fs::read(&backups[0]).unwrap()).unwrap();
    assert_eq!(backup.data.keymap, before_import.data.keymap);
    let persisted: AppSettings =
        serde_json::from_slice(&std::fs::read(app_settings_path()).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&persisted.application_layouts).unwrap(),
        serde_json::to_value(&app.app_settings.application_layouts).unwrap()
    );
    assert_eq!(
        read_pictogram_file(&pictogram_library_path()).unwrap(),
        app.app_settings.saved_pictograms
    );
    assert!(
        recorder
            .requests()
            .iter()
            .any(|r| r[..6] == [0x05, 0, 1, 0, 0, 9]),
        "real keymap write was not reached"
    );
    assert_eq!(app.layout.as_ref().unwrap().get_keycode(0, 0), 9);
    let rendered = app.application_layout_rendered_copy(app.layout.as_ref().unwrap());
    assert_eq!(
        rendered.get_keycode(0, 0),
        9,
        "import is invisible while cached Default still wins"
    );
    let settings = app.application_layout_settings().unwrap();
    assert_eq!(
        settings.layouts[DEFAULT_APPLICATION_LAYOUT_ID].layer_names[0],
        "Restored Default"
    );
    assert_eq!(
        serde_json::to_value(&settings.layouts[&application]).unwrap(),
        profile_before
    );
}

#[test]
fn macropad_import_while_application_active_preserves_profiles_and_unmapped_default_controls() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, application) = fixture();
    {
        let settings = app.application_layout_settings_mut().unwrap();
        settings.active_layout_id = application.clone();
        settings.editor_layout_id = application.clone();
        settings
            .layouts
            .get_mut(DEFAULT_APPLICATION_LAYOUT_ID)
            .unwrap()
            .set_keycode(0, 1, 10);
    }
    app.keycode_picker.macro_count = 1;
    app.keycode_picker.macro_texts = vec![b"macro body".to_vec()];
    app.keycode_picker.macro_names = vec!["Macro name".into()];
    app.combo_entries = vec![ComboEntry {
        keys: [4, 5, 0, 0],
        output: 6.into(),
        layer: None,
    }];
    app.combo_names = vec!["Combo name".into()];
    app.app_settings.saved_pictograms.push(SavedPictogram {
        origin: PictogramOrigin::Legacy,
        name: "Keep artwork".into(),
        color: [1, 2, 3],
        bitmap: vec![0x55; PICTOGRAM_BYTES],
    });
    let icons_before = serde_json::to_value(&app.app_settings.saved_pictograms).unwrap();
    let app_before =
        serde_json::to_value(&app.application_layout_settings().unwrap().layouts[&application])
            .unwrap();
    let mut bundle = app.entlayout_snapshot().unwrap();
    bundle.data.keymap[0][0] = 11;
    let mut mapping = app.entlayout_import_mapping(&bundle).unwrap();
    mapping.key_mapping[1] = None;
    app.apply_entlayout(&bundle).unwrap();
    // Exercise a partial universal mapping separately: only key 0 is imported.
    app.application_layout_settings_mut()
        .unwrap()
        .layouts
        .get_mut(DEFAULT_APPLICATION_LAYOUT_ID)
        .unwrap()
        .set_keycode(0, 1, 12);
    app.apply_entlayout_local_state(&bundle, &mapping).unwrap();
    let settings = app.application_layout_settings().unwrap();
    assert_eq!(
        settings.layouts[DEFAULT_APPLICATION_LAYOUT_ID].layers[0][0],
        11
    );
    assert_eq!(
        settings.layouts[DEFAULT_APPLICATION_LAYOUT_ID].layers[0][1],
        12
    );
    assert_eq!(settings.active_layout_id, application);
    assert_eq!(settings.editor_layout_id, application);
    assert_eq!(
        serde_json::to_value(&settings.layouts[&application]).unwrap(),
        app_before
    );
    assert_eq!(app.keycode_picker.macro_texts, vec![b"macro body".to_vec()]);
    assert_eq!(app.keycode_picker.macro_names, vec!["Macro name"]);
    assert_eq!(app.combo_entries[0].keys, [4, 5, 0, 0]);
    assert_eq!(
        serde_json::to_value(&app.app_settings.saved_pictograms).unwrap(),
        icons_before
    );
    let persisted: AppSettings =
        serde_json::from_str(&std::fs::read_to_string(app_settings_path()).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&persisted.application_layouts).unwrap(),
        serde_json::to_value(&app.app_settings.application_layouts).unwrap()
    );
}

#[test]
fn k04_standard_layout_round_trip_does_not_touch_saved_macropad_profiles() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    let (mut app, _, _) = fixture();
    let mut keyboard = app.device_manager.devices()[0].clone();
    keyboard.name = "K:04".into();
    keyboard.product_id = 0x0004;
    app.device_manager.replace_devices(vec![keyboard]);
    let before = serde_json::to_value(&app.app_settings.application_layouts).unwrap();
    assert!(!app.application_layouts_supported());
    let bundle = app.entlayout_snapshot().unwrap();
    assert_eq!(bundle.keyboard.layers, 8);
    assert_eq!(bundle.data.keymap[0][0], 4);
    assert_eq!(bundle.data.layer_names[0], "Firmware 0");
    app.apply_entlayout(&bundle).unwrap();
    assert_eq!(app.layout.as_ref().unwrap().get_keycode(0, 0), 4);
    assert_eq!(
        serde_json::to_value(&app.app_settings.application_layouts).unwrap(),
        before
    );
}

#[test]
fn macropad_menu_clicks_queue_standard_import_export_without_application_gating() {
    let _storage = crate::app::app_storage::ScopedTempConfig::new();
    for (label_key, expected) in [
        ("Import layout", DeferredFullLayoutAction::ImportEntlayout),
        ("Export layout", DeferredFullLayoutAction::ExportEntlayout),
    ] {
        let (mut app, _, application) = fixture();
        app.application_layout_settings_mut()
            .unwrap()
            .active_layout_id = application;
        // Force the real action to queue instead of invoking an OS file picker.
        app.deferred_device_load = DeferredDeviceLoadState::complete(8);
        app.deferred_device_load
            .set_layer_status(0, DeferredLoadStatus::NotLoaded);
        let ctx = egui::Context::default();
        let rect = egui::Rect::from_min_size(egui::pos2(400.0, 0.0), egui::vec2(80.0, 30.0));
        let draw = |app: &mut EntropyApp, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 700.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    app.draw_layout_device_dropdown(
                        ui,
                        &ctx,
                        crate::i18n::Language::English,
                        Some(rect),
                        true,
                        false,
                        false,
                    )
                },
            )
        };
        draw(&mut app, vec![]);
        let output = draw(&mut app, vec![]);
        let label = crate::i18n::tr_catalog(crate::i18n::Language::English, label_key);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == label => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("menu label missing: {label}"));
        for pressed in [true, false] {
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(app.deferred_full_layout_action, Some(expected));
    }
}
