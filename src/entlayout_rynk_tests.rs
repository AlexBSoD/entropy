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

/// A board with `slots` of each dynamic entry and of macros.
fn with_slots(mut app: EntropyApp, slots: usize) -> EntropyApp {
    app.combo_entries = vec![ComboEntry::default(); slots];
    app.keycode_picker.tap_dance_entries = vec![Default::default(); slots];
    app.key_override_entries = vec![KeyOverrideEntry::default(); slots];
    app.keycode_picker.macro_count = slots;
    app.keycode_picker.macro_texts = vec![Vec::new(); slots];
    app
}

fn macros_of(bundle: &EntLayoutFile) -> Vec<Vec<u8>> {
    entmacro_bytecode(&bundle.data.macros).unwrap()
}

#[test]
fn rynk_losses_wait_for_consent_and_then_go_as_over_vial() {
    use crate::rynk::entlayout::{describe_import_losses, EntLayoutLossesNeedConsent};
    let debug = V1KeyAction::Single(V1Action::KeyboardControl(V1Control::DebugToggle));
    let mut source = with_slots(
        app_with(vec![
            KeyBinding::Rmk(debug),
            KeyBinding::Vial(0x7000),
            KeyBinding::Vial(0x06),
        ]),
        4,
    );
    source.combo_entries[0] = ComboEntry {
        keys: [0x04, 0x7000, 0, 0],
        output: KeyBinding::Vial(0x7000),
        layer: None,
    };
    // The import skips a combo for a layer the target lacks: it stays.
    source.combo_entries[1] = ComboEntry {
        keys: [0x7000, 0x04, 0, 0],
        output: KeyBinding::Vial(0x05),
        layer: Some(9),
    };
    source.combo_entries[2] = ComboEntry {
        keys: [0x04, 0x05, 0x06, 0],
        output: KeyBinding::Vial(0x07),
        layer: None,
    };
    source.keycode_picker.tap_dance_entries[0] = crate::keycode_picker::TapDanceEntry {
        on_tap: KeyBinding::Vial(0x04),
        on_hold: KeyBinding::Vial(0x4104),
        tapping_term: 200,
        ..Default::default()
    };
    source.key_override_entries[0] = KeyOverrideEntry {
        trigger: 0x04,
        replacement: 0x7000,
        layers: 0xFFFF,
        ..Default::default()
    };
    source.keycode_picker.macro_texts[0] = b"h\xd0i\x01\x01\x00".to_vec();
    source.keycode_picker.macro_texts[1] = b"xyz".to_vec();
    let mut bundle = exported(&source);
    let target = with_slots(app_with(vec![KeyBinding::Vial(0x04); 3]), 4);
    // Room for "hi" and the empty slots, not for "xyz" as well.
    let capabilities = rmk_types_rynk::protocol::rynk::DeviceCapabilities {
        max_combos: 4,
        max_combo_keys: 2,
        max_macros: 4,
        macro_space_size: 10,
        max_morse: 4,
        max_patterns_per_key: 8,
        max_forks: 4,
        ..Default::default()
    };
    let mapping = target.entlayout_import_mapping(&bundle).unwrap();
    let losses = target
        .entlayout_rynk_losses(&bundle, &mapping, &capabilities, &[])
        .unwrap();
    assert_eq!(losses.len(), 9, "{losses:#?}");

    let untouched = serde_json::to_value(&bundle).unwrap();
    let error = target
        .settle_rynk_losses(&mut bundle, &capabilities, &[], false)
        .unwrap_err();
    let consent = error.downcast::<EntLayoutLossesNeedConsent>().unwrap();
    assert_eq!(
        consent.message,
        describe_import_losses(target.app_settings.language, &losses)
    );
    assert_eq!(serde_json::to_value(&bundle).unwrap(), untouched);

    target
        .settle_rynk_losses(&mut bundle, &capabilities, &[], true)
        .unwrap();
    let data = &bundle.data;
    assert_eq!(data.keymap[0][1..], [0, 0x06]);
    assert!(
        matches!(
            entlayout_native_key_action(&bundle, 0, 0, true).unwrap(),
            Some(KeyBinding::Rynk(_))
        ),
        "a native v1 action goes as its upgrade"
    );
    let combos = &data.combos.entries;
    assert_eq!((combos[0].keys, combos[0].output), ([0x04, 0, 0, 0], 0));
    assert_eq!((combos[1].keys[0], combos[1].layer), (0x7000, Some(9)));
    assert_eq!((combos[2].keys, combos[2].output), ([0; 4], 0));
    let tap_dance = &data.tap_dance.entries[0];
    assert_eq!(
        (tap_dance.on_tap, tap_dance.on_hold, tap_dance.tapping_term),
        (0x04, 0, 200)
    );
    let key_override = &data.key_overrides.entries[0];
    assert_eq!((key_override.trigger, key_override.replacement), (0x04, 0));
    assert_eq!(
        macros_of(&bundle)[..2],
        [b"hi".to_vec(), Vec::new()],
        "the lost op goes, then the macro past the buffer"
    );
    let left = target
        .entlayout_rynk_losses(&bundle, &mapping, &capabilities, &[])
        .unwrap();
    assert!(
        left.iter()
            .all(crate::rynk::entlayout::ImportLoss::stays_when_accepted),
        "{left:#?}"
    );
    assert_eq!(left.len(), 1);
}

#[test]
fn a_rynk_import_without_losses_never_asks() {
    let source = with_slots(app_with(vec![KeyBinding::Vial(0x04); 3]), 4);
    let mut bundle = exported(&source);
    let untouched = serde_json::to_value(&bundle).unwrap();
    let target = with_slots(app_with(vec![KeyBinding::Vial(0x05); 3]), 4);
    let capabilities = rmk_types_rynk::protocol::rynk::DeviceCapabilities {
        max_combos: 4,
        max_combo_keys: 4,
        max_macros: 4,
        macro_space_size: 4096,
        max_morse: 4,
        max_patterns_per_key: 8,
        max_forks: 4,
        ..Default::default()
    };
    target
        .settle_rynk_losses(&mut bundle, &capabilities, &[], false)
        .unwrap();
    assert_eq!(serde_json::to_value(&bundle).unwrap(), untouched);
}

/// A target with the file's own geometry and slots, so that the import maps
/// every keycode to itself.
fn corpus_target(bundle: &EntLayoutFile) -> (EntropyApp, EntLayoutImportMapping) {
    let data = &bundle.data;
    let mut app = app_with(Vec::new());
    let layout = app.layout.as_mut().unwrap();
    layout.layers = data
        .keymap
        .iter()
        .map(|keys| vec![KeyBinding::Vial(0); keys.len()])
        .collect();
    layout.encoder_layers = data
        .encoder_keymap
        .iter()
        .map(|encoders| vec![0; encoders.len()])
        .collect();
    let custom = |name: String, label: String, title: String| crate::keyboard::CustomKeycode {
        name,
        label,
        title,
    };
    layout.custom_keycodes = match &data.source_layout {
        Some(source) => {
            let mut keycodes = source.custom_keycodes.clone();
            keycodes.sort_by_key(|keycode| keycode.index);
            keycodes
                .into_iter()
                .map(|keycode| custom(keycode.name, keycode.label, keycode.title))
                .collect()
        }
        None => (0..64)
            .map(|index| custom(format!("USER{index:02}"), String::new(), String::new()))
            .collect(),
    };
    app.layer_count = data.keymap.len();
    app.combo_entries = vec![ComboEntry::default(); data.combos.entries.len()];
    app.keycode_picker.tap_dance_entries = vec![Default::default(); data.tap_dance.entries.len()];
    app.key_override_entries = vec![KeyOverrideEntry::default(); data.key_overrides.entries.len()];
    app.keycode_picker.macro_count = entmacro_count(&data.macros);
    app.keycode_picker.macro_texts = vec![Vec::new(); app.keycode_picker.macro_count];
    let width = |layers: &[Vec<u16>]| layers.iter().map(Vec::len).max().unwrap_or(0);
    let mapping = EntLayoutImportMapping {
        exact_layout: true,
        key_mapping: (0..width(&data.keymap)).map(Some).collect(),
        encoder_mapping: (0..width(&data.encoder_keymap)).map(Some).collect(),
    };
    (app, mapping)
}

/// Real .entlayout files on a K:04 on Rynk: `ENTROPY_RYNK_CORPUS` names them,
/// separated by `:`, and `ENTROPY_RYNK_CORPUS_LOSSY` the ones that must ask
/// first. They stay out of the repository. Accepted, a file must leave
/// nothing behind but native v1 upgrades.
#[test]
#[ignore = "needs ENTROPY_RYNK_CORPUS"]
fn real_files_ask_only_where_they_lose() {
    let paths = |name| {
        let mut paths: Vec<String> = std::env::var(name)
            .unwrap_or_default()
            .split(':')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        paths.sort();
        paths
    };
    let corpus = paths("ENTROPY_RYNK_CORPUS");
    assert!(!corpus.is_empty(), "ENTROPY_RYNK_CORPUS");
    // K:04 on Rynk: 32 of each, the doubled macro buffer.
    let capabilities = rmk_types_rynk::protocol::rynk::DeviceCapabilities {
        max_combos: 32,
        max_combo_keys: 4,
        max_macros: 32,
        macro_space_size: 4096,
        max_morse: 32,
        max_patterns_per_key: 8,
        max_forks: 32,
        ..Default::default()
    };
    let mut asked = Vec::new();
    for path in corpus {
        let json = std::fs::read_to_string(&path).unwrap();
        let mut bundle = match read_entlayout_document(&json).unwrap() {
            EntLayoutDocument::Device(file) => file,
            EntLayoutDocument::Application(_) => panic!("{path}: device layout expected"),
        };
        let (app, mapping) = corpus_target(&bundle);
        let losses = app
            .entlayout_rynk_losses(&bundle, &mapping, &capabilities, &[])
            .unwrap();
        eprintln!("{path}: v{}, {} losses", bundle.version, losses.len());
        for loss in &losses {
            eprintln!("  {loss:?}");
        }
        if losses.is_empty() {
            continue;
        }
        app.lenient_rynk_entlayout(&mut bundle, &capabilities, &[])
            .unwrap();
        let left: Vec<_> = app
            .entlayout_rynk_losses(&bundle, &mapping, &capabilities, &[])
            .unwrap()
            .into_iter()
            .filter(|loss| !loss.stays_when_accepted())
            .collect();
        assert_eq!(left, [], "{path}");
        asked.push(path);
    }
    assert_eq!(asked, paths("ENTROPY_RYNK_CORPUS_LOSSY"));
}
