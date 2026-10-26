use super::*;
use crate::keyboard::KeyBinding;
use crate::rynk::keycode::identical;
use rmk_types::action::{
    Action as V1Action, KeyAction as V1KeyAction, KeyboardAction as V1Control,
};
use rmk_types_rynk::action::{Action, KeyAction, KeyboardAction};
use rmk_types_rynk::keycode::{HidKeyCode, KeyCode};

/// A one-row, three-key keyboard with the given first layer.
fn app_with(layer: Vec<KeyBinding>) -> EntropyApp {
    let json = serde_json::json!({
        "matrix": {"rows": 1, "cols": 3},
        "layouts": {"keymap": [["0,0", "0,1", "0,2"]]}
    });
    let mut layout = KeyboardLayout::from_vial_json(&json).unwrap();
    layout.layers = vec![layer];
    let mut app = EntropyApp::new_inert_for_test();
    app.layout = Some(layout);
    app
}

fn exported(app: &EntropyApp) -> EntLayoutFile {
    let json = serde_json::to_string(&app.entlayout_snapshot().unwrap()).unwrap();
    match read_entlayout_document(&json).unwrap() {
        EntLayoutDocument::Device(file) => file,
        EntLayoutDocument::Application(_) => panic!("device layout expected"),
    }
}

#[test]
fn rynk_actions_export_as_version_11_and_read_back() {
    let user = KeyAction::Single(Action::User(90));
    // A profile index no keycode spells: the JSON keeps it.
    let tap_hold = KeyAction::TapHold(
        Action::Key(KeyCode::Hid(HidKeyCode::A)),
        Action::LayerOn(2),
        5,
    );
    let mut app = app_with(vec![
        KeyBinding::Rynk(user),
        KeyBinding::Vial(0x05),
        KeyBinding::Rynk(tap_hold),
    ]);
    app.combo_entries = vec![ComboEntry {
        keys: [0x04, 0x05, 0, 0],
        output: KeyBinding::Rynk(user),
        layer: Some(1),
    }];
    app.keycode_picker.tap_dance_entries = vec![crate::keycode_picker::TapDanceEntry {
        on_hold: KeyBinding::Rynk(user),
        tapping_term: 200,
        ..Default::default()
    }];

    let bundle = exported(&app);
    assert_eq!(
        bundle.version,
        crate::rynk::entlayout::RYNK_ENTLAYOUT_VERSION
    );
    assert!(native_keymap_is_empty(&bundle.data.native_keymap));
    let key = |index| entlayout_native_key_action(&bundle, 0, index, true).unwrap();
    assert!(matches!(key(0), Some(KeyBinding::Rynk(action)) if identical(action, user)));
    assert_eq!(key(1), None);
    assert_eq!(bundle.data.keymap[0][1], 0x05);
    assert!(matches!(key(2), Some(KeyBinding::Rynk(action)) if identical(action, tap_hold)));
    let combo = &bundle.data.combos.entries[0];
    assert_eq!(
        entlayout_dynamic_binding(
            bundle.version,
            combo.output,
            combo.native_output.as_deref(),
            combo.rynk_output,
            true
        )
        .unwrap(),
        KeyBinding::Rynk(user)
    );
    assert_eq!(bundle.data.tap_dance.entries[0].rynk_on_hold, Some(user));

    // A keyboard on the old firmware cannot take the file.
    let error = app
        .validate_entlayout_file(&bundle)
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        crate::i18n::tr_catalog(app.app_settings.language, "entlayout.rynk_only")
    );

    // Without Rynk actions an export stays version 7.
    let plain = exported(&app_with(vec![KeyBinding::Vial(0x04); 3]));
    assert_eq!(plain.version, ENTLAYOUT_VERSION);
    assert!(rynk_keymap_is_empty(&plain.data.rynk_keymap));
    assert!(serde_json::to_value(&plain).unwrap()["data"]
        .get("rynk_keymap")
        .is_none());
}

#[test]
fn native_fields_decode_by_file_version_and_target() {
    let usb = V1KeyAction::Single(V1Action::KeyboardControl(V1Control::OutputUsb));
    let encoded = encode_native_key_action(usb).unwrap();
    assert_eq!(
        entlayout_native_binding(7, Some(&encoded), None, false).unwrap(),
        Some(KeyBinding::Rmk(usb))
    );
    assert_eq!(
        entlayout_native_binding(7, Some(&encoded), None, true).unwrap(),
        Some(KeyBinding::Rynk(KeyAction::Single(Action::User(38))))
    );
    // The same bytes mean different actions: version 10 wrote the Rynk layout.
    let bytes = BASE64_STANDARD.encode([2, 17, 6]);
    assert_eq!(
        entlayout_native_binding(10, Some(&bytes), None, false).unwrap(),
        Some(KeyBinding::Rynk(KeyAction::Single(
            Action::KeyboardControl(KeyboardAction::CapsWordToggle)
        )))
    );
    assert_eq!(
        entlayout_native_binding(7, Some(&bytes), None, false).unwrap(),
        Some(KeyBinding::Rmk(V1KeyAction::Single(
            V1Action::KeyboardControl(V1Control::OutputBluetooth)
        )))
    );
    let user = KeyAction::Single(Action::User(70));
    assert_eq!(
        entlayout_native_binding(11, Some(&encoded), Some(user), false).unwrap(),
        Some(KeyBinding::Rynk(user)),
        "a Rynk field wins over a native one"
    );
    assert_eq!(entlayout_native_binding(7, None, None, true).unwrap(), None);

    // Version 10 files with native actions are for Rynk boards only; the
    // version itself is readable.
    let mut bundle = exported(&app_with(vec![KeyBinding::Vial(0x04); 3]));
    bundle.version = crate::rynk::entlayout::NATIVE_V2_ENTLAYOUT_VERSION;
    let app = app_with(vec![KeyBinding::Vial(0x04); 3]);
    app.validate_entlayout_file(&bundle).unwrap();
    bundle.data.native_keymap = vec![vec![Some(bytes), None, None]];
    assert!(app.validate_entlayout_file(&bundle).is_err());
}

#[test]
fn a_rynk_import_lists_what_it_would_lose_before_writing() {
    let debug = V1KeyAction::Single(V1Action::KeyboardControl(V1Control::DebugToggle));
    let source = app_with(vec![
        KeyBinding::Rmk(debug),
        KeyBinding::Vial(0x7000),
        KeyBinding::Vial(0x06),
    ]);
    let bundle = exported(&source);
    let target = app_with(vec![KeyBinding::Vial(0x04); 3]);
    let mapping = target.entlayout_import_mapping(&bundle).unwrap();
    let capabilities = rmk_types_rynk::protocol::rynk::DeviceCapabilities {
        max_macros: 32,
        macro_space_size: 4096,
        ..Default::default()
    };
    let losses = target
        .entlayout_rynk_losses(&bundle, &mapping, &capabilities, &[])
        .unwrap();
    use crate::rynk::entlayout::{BindingPlace, ImportLoss};
    assert_eq!(
        losses,
        [
            ImportLoss::NativeV1 {
                place: BindingPlace::Key { layer: 0, key: 0 },
                loss: crate::rynk::native_v1::NativeV1Loss::KeyboardAction(V1Control::DebugToggle),
            },
            ImportLoss::Binding {
                place: BindingPlace::Key { layer: 0, key: 1 },
                error: crate::rynk::keycode::KeyBindingError::UnsupportedKeycode(0x7000),
            },
        ]
    );
    let message =
        crate::rynk::entlayout::describe_import_losses(target.app_settings.language, &losses);
    assert_eq!(message.lines().count(), 3, "{message}");
    assert!(!message.contains('{'), "{message}");

    // Nothing to lose: an import of what a Rynk board holds.
    let clean = exported(&app_with(vec![
        KeyBinding::Vial(0x04),
        KeyBinding::Vial(0x05),
        KeyBinding::Vial(0x06),
    ]));
    let mapping = target.entlayout_import_mapping(&clean).unwrap();
    assert_eq!(
        target
            .entlayout_rynk_losses(&clean, &mapping, &capabilities, &[])
            .unwrap(),
        []
    );
}
