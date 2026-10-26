//! .entlayout files and Rynk boards.
//!
//! A file made from a Rynk board is version 11: its native fields hold key
//! actions as serde JSON by variant name, so the file outlives variant
//! reorders upstream. Older files cross over here. Version 7 native fields are
//! native v1 postcard; version 10 ones, from test builds of the merged RMK
//! layout (`rmk-types` 5896357), are postcard the Rynk revision decodes as is.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rmk_types_rynk::action::KeyAction;

use rmk_types_rynk::morse::Morse;
use rmk_types_rynk::protocol::rynk::DeviceCapabilities;

use super::dynamic::{combo, fork, merge_tap_dance, DynamicError};
use super::keycode::{binding_action, vial_keycode_action, KeyBindingError};
use super::macros::{macro_space_used, vial_macro_ops, MacroLoss};
use super::native_v1::{upgrade_native_v1_key_action, NativeV1Loss};
use crate::app::{ComboEntry, KeyOverrideEntry};
use crate::keyboard::KeyBinding;
use crate::keycode_picker::TapDanceEntry;

pub(crate) const RYNK_ENTLAYOUT_VERSION: u16 = 11;
/// Read only: written by test builds of the merged layout, never again.
pub(crate) const NATIVE_V2_ENTLAYOUT_VERSION: u16 = 10;

/// A version 10 native field. The whole field must be one action: bytes left
/// over mean the layouts differ after all.
pub(crate) fn decode_v10_native_field(encoded: &str) -> Result<KeyAction> {
    let bytes = native_field_bytes(encoded)?;
    match postcard::take_from_bytes::<KeyAction>(&bytes) {
        Ok((action, [])) => Ok(action),
        Ok(_) => bail!("RMK key action payload has trailing bytes"),
        Err(error) => Err(error).context("invalid RMK key action payload"),
    }
}

/// What an import cannot carry to a Rynk board. The import stops with the
/// whole list before it writes anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImportLoss {
    /// A native v1 action that changes on the way.
    NativeV1 {
        place: BindingPlace,
        loss: NativeV1Loss,
    },
    Binding {
        place: BindingPlace,
        error: KeyBindingError,
    },
    Encoder {
        layer: usize,
        encoder: usize,
        keycode: u16,
    },
    Macro {
        index: usize,
        loss: MacroLoss,
    },
    /// Macros past the board's slots.
    MacroSlot {
        index: usize,
    },
    /// All macros together, in the board's encoding, past its buffer.
    MacroSpace {
        used: usize,
        available: usize,
    },
    Combo {
        index: usize,
        error: DynamicError,
    },
    TapDance {
        index: usize,
        error: DynamicError,
    },
    KeyOverride {
        index: usize,
        error: DynamicError,
    },
    /// A combo, tap dance or key override past the board's slots.
    Slot {
        kind: SlotKind,
        index: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingPlace {
    Key { layer: usize, key: usize },
    ComboOutput { index: usize },
    TapDance { index: usize, field: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotKind {
    Combo,
    TapDance,
    KeyOverride,
}

/// Turns the native v1 actions an older file brought in into Rynk ones, in
/// place, and lists what changed on the way.
pub(crate) fn upgrade_native_v1_bindings(
    layers: &mut [Vec<KeyBinding>],
    combos: &mut [ComboEntry],
    tap_dances: &mut [TapDanceEntry],
) -> Vec<ImportLoss> {
    let mut losses = Vec::new();
    let mut upgrade = |binding: &mut KeyBinding, place| {
        if let KeyBinding::Rmk(action) = *binding {
            let upgrade = upgrade_native_v1_key_action(action);
            *binding = KeyBinding::Rynk(upgrade.action);
            losses.extend(
                upgrade
                    .losses
                    .into_iter()
                    .map(|loss| ImportLoss::NativeV1 { place, loss }),
            );
        }
    };
    for (layer, bindings) in layers.iter_mut().enumerate() {
        for (key, binding) in bindings.iter_mut().enumerate() {
            upgrade(binding, BindingPlace::Key { layer, key });
        }
    }
    for (index, entry) in combos.iter_mut().enumerate() {
        upgrade(&mut entry.output, BindingPlace::ComboOutput { index });
    }
    for (index, entry) in tap_dances.iter_mut().enumerate() {
        let fields = [
            &mut entry.on_tap,
            &mut entry.on_hold,
            &mut entry.on_double_tap,
            &mut entry.on_tap_hold,
        ];
        for (field, binding) in fields.into_iter().enumerate() {
            upgrade(binding, BindingPlace::TapDance { index, field });
        }
    }
    losses
}

/// The editor state an import is about to write to a Rynk board.
pub(crate) struct RynkImport<'a> {
    pub(crate) layers: &'a [Vec<KeyBinding>],
    pub(crate) encoder_layers: &'a [Vec<u16>],
    pub(crate) macros: &'a [Vec<u8>],
    pub(crate) combos: &'a [ComboEntry],
    pub(crate) tap_dances: &'a [TapDanceEntry],
    pub(crate) key_overrides: &'a [KeyOverrideEntry],
    /// The board's morse keys as read: a tap dance writes over them.
    pub(crate) morses: &'a [Morse],
}

/// Everything in `import` that would not reach the board as it is: what a
/// write would refuse, and what the firmware would drop.
pub(crate) fn rynk_import_losses(
    import: &RynkImport<'_>,
    capabilities: &DeviceCapabilities,
) -> Vec<ImportLoss> {
    let mut losses = Vec::new();
    for (layer, bindings) in import.layers.iter().enumerate() {
        for (key, &binding) in bindings.iter().enumerate() {
            if let Err(error) = binding_action(binding) {
                let place = BindingPlace::Key { layer, key };
                losses.push(ImportLoss::Binding { place, error });
            }
        }
    }
    for (layer, keycodes) in import.encoder_layers.iter().enumerate() {
        for (encoder, &keycode) in keycodes.iter().enumerate() {
            if vial_keycode_action(keycode).is_none() {
                losses.push(ImportLoss::Encoder {
                    layer,
                    encoder,
                    keycode,
                });
            }
        }
    }

    let slots = capabilities.max_macros as usize;
    let mut macros = Vec::new();
    for (index, bytes) in import.macros.iter().enumerate() {
        if index >= slots {
            if !bytes.is_empty() {
                losses.push(ImportLoss::MacroSlot { index });
            }
            continue;
        }
        let (ops, macro_losses) = vial_macro_ops(bytes);
        losses.extend(
            macro_losses
                .into_iter()
                .map(|loss| ImportLoss::Macro { index, loss }),
        );
        macros.push(ops);
    }
    let used = macro_space_used(macros.iter().map(Vec::as_slice), slots);
    let available = capabilities.macro_space_size as usize;
    if used > available {
        losses.push(ImportLoss::MacroSpace { used, available });
    }

    for (index, entry) in import.combos.iter().enumerate() {
        if index >= capabilities.max_combos as usize {
            if entry.keys.iter().any(|&key| key != 0) || !entry.output.is_no() {
                losses.push(ImportLoss::Slot {
                    kind: SlotKind::Combo,
                    index,
                });
            }
            continue;
        }
        if let Err(error) = combo(entry, capabilities.max_combo_keys as usize) {
            losses.push(ImportLoss::Combo { index, error });
        }
    }
    for (index, entry) in import.tap_dances.iter().enumerate() {
        if index >= capabilities.max_morse as usize {
            if *entry != TapDanceEntry::default() {
                losses.push(ImportLoss::Slot {
                    kind: SlotKind::TapDance,
                    index,
                });
            }
            continue;
        }
        let morse = import.morses.get(index).cloned().unwrap_or_default();
        if let Err(error) =
            merge_tap_dance(&morse, entry, capabilities.max_patterns_per_key as usize)
        {
            losses.push(ImportLoss::TapDance { index, error });
        }
    }
    for (index, entry) in import.key_overrides.iter().enumerate() {
        if index >= capabilities.max_forks as usize {
            if entry.trigger != 0 {
                losses.push(ImportLoss::Slot {
                    kind: SlotKind::KeyOverride,
                    index,
                });
            }
            continue;
        }
        if let Err(error) = fork(entry) {
            losses.push(ImportLoss::KeyOverride { index, error });
        }
    }
    losses
}

/// The import error for `losses`, one line each.
pub(crate) fn describe_import_losses(
    language: crate::i18n::Language,
    losses: &[ImportLoss],
) -> String {
    let header = crate::i18n::tr_catalog(language, "entlayout.rynk_losses");
    std::iter::once(header.to_owned())
        .chain(losses.iter().map(|loss| describe_loss(language, loss)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn describe_loss(language: crate::i18n::Language, loss: &ImportLoss) -> String {
    let tr = |key, vars: &[(&str, &str)]| crate::i18n::tr_catalog_format(language, key, vars);
    let keycode = |keycode: u16| format!("{keycode:#06x}");
    let at = |place: BindingPlace, detail: String| match place {
        BindingPlace::Key { layer, key } => tr(
            "rynk_import.key",
            &[
                ("layer", &layer.to_string()),
                ("key", &key.to_string()),
                ("detail", &detail),
            ],
        ),
        BindingPlace::ComboOutput { index } => tr(
            "rynk_import.combo_output",
            &[("index", &index.to_string()), ("detail", &detail)],
        ),
        BindingPlace::TapDance { index, field } => tr(
            "rynk_import.tap_dance_field",
            &[
                ("index", &index.to_string()),
                ("field", &(field + 1).to_string()),
                ("detail", &detail),
            ],
        ),
    };
    let binding = |error: &KeyBindingError| match error {
        KeyBindingError::UnsupportedKeycode(code) => tr(
            "rynk_import.unsupported_keycode",
            &[("keycode", &keycode(*code))],
        ),
        KeyBindingError::NativeV1 => tr("rynk_import.native_v1", &[]),
    };
    let dynamic = |error: &DynamicError| match error {
        DynamicError::Binding(error) => binding(error),
        DynamicError::NotAnAction(action) => tr(
            "rynk_import.not_an_action",
            &[("action", &format!("{action:?}"))],
        ),
        DynamicError::TooManyComboKeys { max } => tr(
            "rynk_import.too_many_combo_keys",
            &[("max", &max.to_string())],
        ),
        DynamicError::TooManyMorsePatterns { max } => tr(
            "rynk_import.too_many_patterns",
            &[("max", &max.to_string())],
        ),
    };
    let index = |index: &usize| index.to_string();
    match loss {
        ImportLoss::NativeV1 { place, loss } => at(
            *place,
            match loss {
                NativeV1Loss::KeyboardAction(action) => tr(
                    "rynk_import.removed_action",
                    &[("action", &format!("{action:?}"))],
                ),
                NativeV1Loss::TapHoldProfile(_) => tr("rynk_import.tap_hold_profile", &[]),
                NativeV1Loss::Unsupported => tr("rynk_import.unsupported_action", &[]),
            },
        ),
        ImportLoss::Binding { place, error } => at(*place, binding(error)),
        ImportLoss::Encoder {
            layer,
            encoder,
            keycode: code,
        } => tr(
            "rynk_import.encoder",
            &[
                ("layer", &layer.to_string()),
                ("encoder", &encoder.to_string()),
                ("keycode", &keycode(*code)),
            ],
        ),
        ImportLoss::Macro { index: slot, loss } => {
            let detail = match loss {
                MacroLoss::Byte(byte) => tr(
                    "rynk_import.macro_byte",
                    &[("byte", &format!("{byte:#04x}"))],
                ),
                MacroLoss::UnknownOp(kind) => tr(
                    "rynk_import.macro_unknown_op",
                    &[("kind", &format!("{kind:#04x}"))],
                ),
                MacroLoss::Truncated(_) => tr("rynk_import.macro_truncated", &[]),
                MacroLoss::Keycode(code) => {
                    tr("rynk_import.macro_keycode", &[("keycode", &keycode(*code))])
                }
                MacroLoss::Op(op) => tr("rynk_import.macro_op", &[("op", &format!("{op:?}"))]),
            };
            tr(
                "rynk_import.macro",
                &[("index", &index(slot)), ("detail", &detail)],
            )
        }
        ImportLoss::MacroSlot { index: slot } => {
            tr("rynk_import.macro_slot", &[("index", &index(slot))])
        }
        ImportLoss::MacroSpace { used, available } => tr(
            "rynk_import.macro_space",
            &[
                ("used", &used.to_string()),
                ("available", &available.to_string()),
            ],
        ),
        ImportLoss::Combo { index: slot, error } => tr(
            "rynk_import.combo",
            &[("index", &index(slot)), ("detail", &dynamic(error))],
        ),
        ImportLoss::TapDance { index: slot, error } => tr(
            "rynk_import.tap_dance",
            &[("index", &index(slot)), ("detail", &dynamic(error))],
        ),
        ImportLoss::KeyOverride { index: slot, error } => tr(
            "rynk_import.key_override",
            &[("index", &index(slot)), ("detail", &dynamic(error))],
        ),
        ImportLoss::Slot { kind, index: slot } => tr(
            match kind {
                SlotKind::Combo => "rynk_import.combo_slot",
                SlotKind::TapDance => "rynk_import.tap_dance_slot",
                SlotKind::KeyOverride => "rynk_import.key_override_slot",
            },
            &[("index", &index(slot))],
        ),
    }
}

fn native_field_bytes(encoded: &str) -> Result<Vec<u8>> {
    BASE64_STANDARD
        .decode(encoded)
        .context("invalid base64 RMK key action")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rynk::keycode::{catalog, identical};
    use crate::rynk::native_v1::{NativeV1Loss, NativeV1Upgrade};
    use rmk_types_rynk::action::{Action, KeyboardAction};
    use rmk_types_rynk::keycode::{HidKeyCode, KeyCode};
    use rmk_types_rynk::modifier::ModifierCombination;

    fn field(bytes: &[u8]) -> String {
        BASE64_STANDARD.encode(bytes)
    }

    /// A version 7 native field on its way to a Rynk board.
    fn upgrade_v7_native_field(encoded: &str) -> Result<NativeV1Upgrade> {
        let action: rmk_types::action::KeyAction =
            postcard::from_bytes(&native_field_bytes(encoded)?)
                .context("invalid RMK key action payload")?;
        Ok(upgrade_native_v1_key_action(action))
    }

    /// The bytes the v2 test build wrote, from the tests of its upgrader: the
    /// merged layout and the Rynk revision must read them alike.
    #[test]
    fn version_10_fields_decode_as_the_merged_layout_wrote_them() {
        let control = |action| KeyAction::Single(Action::KeyboardControl(action));
        for (bytes, action) in [
            (vec![2, 17, 6], control(KeyboardAction::CapsWordToggle)),
            (
                vec![3, 17, 2],
                KeyAction::Tap(Action::KeyboardControl(KeyboardAction::ClearEeprom)),
            ),
            (vec![2, 17, 1], control(KeyboardAction::Reboot)),
            (vec![2, 19, 3], KeyAction::Single(Action::User(3))),
            (vec![5, 2], KeyAction::Morse(2)),
            (
                vec![4, 3, 0x27, 0x02, 2, 0x01, 0xFF],
                KeyAction::TapHold(
                    Action::KeyWithModifier(HidKeyCode::Kc0, ModifierCombination::LSHIFT),
                    Action::Modifier(ModifierCombination::LCTRL),
                    u8::MAX,
                ),
            ),
        ] {
            let decoded = decode_v10_native_field(&field(&bytes)).unwrap();
            assert!(identical(decoded, action), "{bytes:?}: {decoded:?}");
            let mut buf = [0u8; 16];
            assert_eq!(postcard::to_slice(&action, &mut buf).unwrap(), bytes);
        }
        assert!(
            decode_v10_native_field(&field(&[2, 17, 7])).is_err(),
            "no eighth keyboard action"
        );
        assert!(
            decode_v10_native_field(&field(&[5, 2, 0])).is_err(),
            "trailing byte"
        );
        assert!(decode_v10_native_field("not base64!").is_err());
    }

    #[test]
    fn version_7_fields_upgrade_with_their_losses() {
        let upgrade = |bytes: &[u8]| {
            let upgrade = upgrade_v7_native_field(&field(bytes)).unwrap();
            (upgrade.action, upgrade.losses)
        };
        assert_eq!(
            upgrade(&[2, 17, 10]),
            (
                KeyAction::Single(Action::KeyboardControl(KeyboardAction::CapsWordToggle)),
                vec![]
            )
        );
        assert_eq!(
            upgrade(&[2, 17, 6]),
            (KeyAction::Single(Action::User(37)), vec![])
        );
        assert_eq!(
            upgrade(&[2, 17, 2]),
            (
                KeyAction::No,
                vec![NativeV1Loss::KeyboardAction(
                    rmk_types::action::KeyboardAction::DebugToggle
                )]
            )
        );
        // A tap-hold with a packed v1 profile: PermissiveHold, 200 ms.
        let (action, losses) = upgrade(&[4, 1, 0, 4, 2, 0x01, 0xC8, 0x81, 0x80, 0x80, 0x04]);
        assert!(identical(
            action,
            KeyAction::TapHold(
                Action::Key(KeyCode::Hid(HidKeyCode::A)),
                Action::Modifier(ModifierCombination::LCTRL),
                u8::MAX
            )
        ));
        assert!(matches!(losses[..], [NativeV1Loss::TapHoldProfile(_)]));
        assert!(upgrade_v7_native_field(&field(&[2, 17, 11])).is_err());
    }

    /// Version 11 keeps native actions as serde JSON: every variant of every
    /// enum a key action holds must come back from it unchanged. Modifier
    /// bytes, which are named flags in JSON, are sampled.
    #[test]
    fn key_actions_round_trip_through_json() {
        let sampled = |modifiers: ModifierCombination| {
            [0, 0x01, 0x22, 0x90, 0xFF].contains(&modifiers.into_bits())
        };
        let actions = catalog::actions()
            .into_iter()
            .filter(|action| match *action {
                Action::Modifier(m)
                | Action::KeyWithModifier(_, m)
                | Action::LayerOnWithModifier(_, m)
                | Action::OneShotModifier(m) => sampled(m),
                _ => true,
            });
        let hold = Action::Modifier(ModifierCombination::LCTRL);
        let mut checked = 0;
        for action in actions {
            for key in [
                KeyAction::Single(action),
                KeyAction::Tap(action),
                KeyAction::TapHold(action, hold, 7),
            ] {
                let json = serde_json::to_string(&key).unwrap();
                let back: KeyAction = serde_json::from_str(&json).unwrap();
                assert!(identical(back, key), "{json}");
                checked += 1;
            }
        }
        assert!(checked > 5_000, "{checked}");
        for key in [KeyAction::No, KeyAction::Transparent, KeyAction::Morse(200)] {
            let back: KeyAction =
                serde_json::from_str(&serde_json::to_string(&key).unwrap()).unwrap();
            assert!(identical(back, key));
        }
        let json = serde_json::to_value(KeyAction::TapHold(
            Action::Key(KeyCode::Hid(HidKeyCode::A)),
            Action::Modifier(ModifierCombination::LCTRL),
            u8::MAX,
        ))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"TapHold": [
                {"Key": {"Hid": "A"}},
                {"Modifier": {
                    "left_ctrl": true, "left_shift": false, "left_alt": false, "left_gui": false,
                    "right_ctrl": false, "right_shift": false, "right_alt": false, "right_gui": false
                }},
                255
            ]})
        );
    }

    fn capabilities() -> DeviceCapabilities {
        // K:04 on Rynk: 32 of each, the doubled macro buffer.
        DeviceCapabilities {
            max_combos: 32,
            max_combo_keys: 4,
            max_macros: 32,
            macro_space_size: 4096,
            max_morse: 32,
            max_patterns_per_key: 8,
            max_forks: 32,
            ..Default::default()
        }
    }

    #[test]
    fn native_v1_bindings_upgrade_in_place_with_their_losses() {
        use rmk_types::action::{
            Action as V1Action, KeyAction as V1KeyAction, KeyboardAction as V1Control,
        };
        let control =
            |action| KeyBinding::Rmk(V1KeyAction::Single(V1Action::KeyboardControl(action)));
        let mut layers = vec![vec![
            KeyBinding::Vial(0x04),
            control(V1Control::OutputUsb),
            control(V1Control::DebugToggle),
        ]];
        let mut combos = vec![ComboEntry {
            keys: [0x04, 0x05, 0, 0],
            output: control(V1Control::OutputBluetooth),
            layer: None,
        }];
        let mut tap_dances = vec![TapDanceEntry {
            on_hold: control(V1Control::OutputAuto),
            ..Default::default()
        }];

        let losses = upgrade_native_v1_bindings(&mut layers, &mut combos, &mut tap_dances);
        assert_eq!(layers[0][0], KeyBinding::Vial(0x04));
        assert_eq!(
            layers[0][1],
            KeyBinding::Rynk(KeyAction::Single(Action::User(38)))
        );
        assert_eq!(layers[0][2], KeyBinding::Rynk(KeyAction::No));
        assert_eq!(
            combos[0].output,
            KeyBinding::Rynk(KeyAction::Single(Action::User(37)))
        );
        assert_eq!(tap_dances[0].on_hold, KeyBinding::Rynk(KeyAction::No));
        assert_eq!(
            losses,
            [
                ImportLoss::NativeV1 {
                    place: BindingPlace::Key { layer: 0, key: 2 },
                    loss: NativeV1Loss::KeyboardAction(V1Control::DebugToggle),
                },
                ImportLoss::NativeV1 {
                    place: BindingPlace::TapDance { index: 0, field: 1 },
                    loss: NativeV1Loss::KeyboardAction(V1Control::OutputAuto),
                },
            ]
        );
    }

    #[test]
    fn an_import_that_fits_has_no_losses_and_every_misfit_is_listed() {
        let layers = vec![vec![
            KeyBinding::Vial(0x04),
            KeyBinding::Rynk(KeyAction::Single(Action::User(90))),
        ]];
        let combos = vec![ComboEntry {
            keys: [0x04, 0x05, 0, 0],
            output: KeyBinding::Vial(0x29),
            layer: Some(1),
        }];
        let tap_dances = vec![TapDanceEntry {
            on_tap: KeyBinding::Vial(0x04),
            tapping_term: 200,
            ..Default::default()
        }];
        let key_overrides = vec![crate::app::KeyOverrideEntry {
            trigger: 0x2A,
            replacement: 0x4C,
            layers: 0xFFFF,
            ..Default::default()
        }];
        let macros = vec![b"hello".to_vec(), Vec::new()];
        let encoders = vec![vec![0x04, 0x05]];
        let import = RynkImport {
            layers: &layers,
            encoder_layers: &encoders,
            macros: &macros,
            combos: &combos,
            tap_dances: &tap_dances,
            key_overrides: &key_overrides,
            morses: &[],
        };
        assert_eq!(rynk_import_losses(&import, &capabilities()), []);

        let layers = vec![vec![KeyBinding::Vial(0x7000)]];
        let encoders = vec![vec![0x7800]];
        let macros = vec![b"\xd0\x9f".to_vec(), b"x".to_vec(), b"x".to_vec()];
        let combos = vec![
            ComboEntry {
                keys: [0x04, 0x05, 0x06, 0],
                output: KeyBinding::Vial(0x29),
                layer: None,
            },
            ComboEntry {
                keys: [0x04, 0, 0, 0],
                output: KeyBinding::Vial(0),
                layer: None,
            },
        ];
        let tap_dances = vec![TapDanceEntry {
            on_tap: KeyBinding::Vial(0x4104),
            ..Default::default()
        }];
        let key_overrides = vec![crate::app::KeyOverrideEntry {
            trigger: 0x7000,
            ..Default::default()
        }];
        let import = RynkImport {
            layers: &layers,
            encoder_layers: &encoders,
            macros: &macros,
            combos: &combos,
            tap_dances: &tap_dances,
            key_overrides: &key_overrides,
            morses: &[],
        };
        let tight = DeviceCapabilities {
            max_macros: 2,
            macro_space_size: 3,
            max_combos: 1,
            max_combo_keys: 2,
            ..capabilities()
        };
        assert_eq!(
            rynk_import_losses(&import, &tight),
            [
                ImportLoss::Binding {
                    place: BindingPlace::Key { layer: 0, key: 0 },
                    error: KeyBindingError::UnsupportedKeycode(0x7000),
                },
                ImportLoss::Encoder {
                    layer: 0,
                    encoder: 0,
                    keycode: 0x7800
                },
                ImportLoss::Macro {
                    index: 0,
                    loss: MacroLoss::Byte(0xD0)
                },
                ImportLoss::Macro {
                    index: 0,
                    loss: MacroLoss::Byte(0x9F)
                },
                ImportLoss::MacroSlot { index: 2 },
                ImportLoss::MacroSpace {
                    used: 4,
                    available: 3
                },
                ImportLoss::Combo {
                    index: 0,
                    error: DynamicError::TooManyComboKeys { max: 2 }
                },
                ImportLoss::Slot {
                    kind: SlotKind::Combo,
                    index: 1
                },
                ImportLoss::TapDance {
                    index: 0,
                    error: DynamicError::NotAnAction(KeyAction::TapHold(
                        Action::Key(KeyCode::Hid(HidKeyCode::A)),
                        Action::LayerOn(1),
                        u8::MAX,
                    )),
                },
                ImportLoss::KeyOverride {
                    index: 0,
                    error: DynamicError::Binding(KeyBindingError::UnsupportedKeycode(0x7000)),
                },
            ]
        );
    }

    /// Real .entlayout files through the whole migration: `ENTROPY_RYNK_CORPUS`
    /// names the files, separated by `:`. They stay out of the repository.
    #[test]
    #[ignore = "needs ENTROPY_RYNK_CORPUS"]
    fn real_files_migrate_without_losses() {
        let corpus = std::env::var("ENTROPY_RYNK_CORPUS").expect("ENTROPY_RYNK_CORPUS");
        let mut failed = Vec::new();
        for path in corpus.split(':').filter(|path| !path.is_empty()) {
            let file: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            let version = file["version"].as_u64().unwrap() as u16;
            let data = &file["data"];
            let binding =
                |keycode: &serde_json::Value, native: &serde_json::Value| match native.as_str() {
                    Some(encoded) if version == NATIVE_V2_ENTLAYOUT_VERSION => {
                        KeyBinding::Rynk(decode_v10_native_field(encoded).unwrap())
                    }
                    Some(encoded) => KeyBinding::Rmk(
                        postcard::from_bytes(&native_field_bytes(encoded).unwrap()).unwrap(),
                    ),
                    None => KeyBinding::Vial(keycode.as_u64().unwrap() as u16),
                };
            let none = serde_json::Value::Null;
            let mut layers: Vec<Vec<KeyBinding>> = data["keymap"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(layer, keys)| {
                    keys.as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                        .map(|(key, keycode)| {
                            binding(
                                keycode,
                                data["native_keymap"]
                                    .get(layer)
                                    .and_then(|l| l.get(key))
                                    .unwrap_or(&none),
                            )
                        })
                        .collect()
                })
                .collect();
            let encoders: Vec<Vec<u16>> =
                serde_json::from_value(data["encoder_keymap"].clone()).unwrap();
            let macros: Vec<Vec<u8>> = data["macros"]["bytecode_base64"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .map(|m| BASE64_STANDARD.decode(m.as_str().unwrap()).unwrap())
                        .collect()
                })
                .unwrap_or_default();
            let mut combos: Vec<ComboEntry> = data["combos"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| ComboEntry {
                    keys: serde_json::from_value(entry["keys"].clone()).unwrap(),
                    output: binding(&entry["output"], &entry["native_output"]),
                    layer: entry["layer"].as_u64().map(|layer| layer as u8),
                })
                .collect();
            let mut tap_dances: Vec<TapDanceEntry> = data["tap_dance"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| TapDanceEntry {
                    on_tap: binding(&entry["on_tap"], &entry["native_on_tap"]),
                    on_hold: binding(&entry["on_hold"], &entry["native_on_hold"]),
                    on_double_tap: binding(&entry["on_double_tap"], &entry["native_on_double_tap"]),
                    on_tap_hold: binding(&entry["on_tap_hold"], &entry["native_on_tap_hold"]),
                    tapping_term: entry["tapping_term"].as_u64().unwrap() as u16,
                })
                .collect();
            let key_overrides: Vec<crate::app::KeyOverrideEntry> = data["key_overrides"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| crate::app::KeyOverrideEntry {
                    trigger: entry["trigger"].as_u64().unwrap() as u16,
                    replacement: entry["replacement"].as_u64().unwrap() as u16,
                    layers: entry["layers"].as_u64().unwrap() as u16,
                    trigger_mods: entry["trigger_mods"].as_u64().unwrap() as u8,
                    negative_mod_mask: entry["negative_mod_mask"].as_u64().unwrap() as u8,
                    suppressed_mods: entry["suppressed_mods"].as_u64().unwrap() as u8,
                    options: crate::app::KeyOverrideOptionsState::from_bits(
                        entry["options"].as_u64().unwrap() as u8,
                    ),
                })
                .collect();

            let mut losses = upgrade_native_v1_bindings(&mut layers, &mut combos, &mut tap_dances);
            let import = RynkImport {
                layers: &layers,
                encoder_layers: &encoders,
                macros: &macros,
                combos: &combos,
                tap_dances: &tap_dances,
                key_overrides: &key_overrides,
                morses: &[],
            };
            losses.extend(rynk_import_losses(&import, &capabilities()));
            let native = layers
                .iter()
                .flatten()
                .filter(|b| matches!(b, KeyBinding::Rynk(_)))
                .count();
            eprintln!(
                "{path}: v{version}, {} layers, {native} native keys, {} losses",
                layers.len(),
                losses.len()
            );
            for loss in &losses {
                eprintln!("  {loss:?}");
            }
            if !losses.is_empty() {
                failed.push(path.to_owned());
            }
        }
        assert!(failed.is_empty(), "losses in {failed:?}");
    }

    #[test]
    fn every_loss_reads_in_both_languages() {
        use crate::rynk::macros::MacroLoss;
        use rmk_types_rynk::keyboard_macros::MacroOp;
        let key = BindingPlace::Key { layer: 1, key: 2 };
        let losses = [
            ImportLoss::NativeV1 {
                place: key,
                loss: NativeV1Loss::KeyboardAction(rmk_types::action::KeyboardAction::OutputAuto),
            },
            ImportLoss::NativeV1 {
                place: BindingPlace::TapDance { index: 0, field: 3 },
                loss: NativeV1Loss::TapHoldProfile(Default::default()),
            },
            ImportLoss::NativeV1 {
                place: BindingPlace::ComboOutput { index: 4 },
                loss: NativeV1Loss::Unsupported,
            },
            ImportLoss::Binding {
                place: key,
                error: KeyBindingError::UnsupportedKeycode(0x7000),
            },
            ImportLoss::Binding {
                place: key,
                error: KeyBindingError::NativeV1,
            },
            ImportLoss::Encoder {
                layer: 0,
                encoder: 1,
                keycode: 0x7827,
            },
            ImportLoss::Macro {
                index: 3,
                loss: MacroLoss::Byte(0xD0),
            },
            ImportLoss::Macro {
                index: 3,
                loss: MacroLoss::UnknownOp(9),
            },
            ImportLoss::Macro {
                index: 3,
                loss: MacroLoss::Truncated(vec![1]),
            },
            ImportLoss::Macro {
                index: 3,
                loss: MacroLoss::Keycode(0x4104),
            },
            ImportLoss::Macro {
                index: 3,
                loss: MacroLoss::Op(MacroOp::PauseForRelease),
            },
            ImportLoss::MacroSlot { index: 40 },
            ImportLoss::MacroSpace {
                used: 5000,
                available: 4096,
            },
            ImportLoss::Combo {
                index: 2,
                error: DynamicError::TooManyComboKeys { max: 4 },
            },
            ImportLoss::TapDance {
                index: 2,
                error: DynamicError::TooManyMorsePatterns { max: 8 },
            },
            ImportLoss::TapDance {
                index: 2,
                error: DynamicError::NotAnAction(KeyAction::Morse(1)),
            },
            ImportLoss::KeyOverride {
                index: 2,
                error: DynamicError::Binding(KeyBindingError::UnsupportedKeycode(0x7800)),
            },
            ImportLoss::Slot {
                kind: SlotKind::Combo,
                index: 33,
            },
            ImportLoss::Slot {
                kind: SlotKind::TapDance,
                index: 33,
            },
            ImportLoss::Slot {
                kind: SlotKind::KeyOverride,
                index: 33,
            },
        ];
        for language in [
            crate::i18n::Language::English,
            crate::i18n::Language::Russian,
        ] {
            let message = describe_import_losses(language, &losses);
            assert_eq!(message.lines().count(), losses.len() + 1, "{message}");
            assert!(
                !message.contains('{') && !message.contains("rynk_import."),
                "{message}"
            );
        }
    }
}
