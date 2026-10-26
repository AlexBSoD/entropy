//! Rynk key actions next to the u16 keycodes the key editor works in.

use rmk_types_rynk::action::{Action, KeyAction};
use rmk_types_rynk::modifier::ModifierCombination;

/// Moves left-hand modifiers to the right hand and back, as
/// `rmk_native::toggle_handed_key_action` does for native v1 actions.
pub(crate) fn toggle_handed_key_action(action: KeyAction) -> KeyAction {
    fn swap_modifiers(modifiers: ModifierCombination) -> ModifierCombination {
        ModifierCombination::from_bits(modifiers.into_bits().rotate_right(4))
    }

    fn swap_action(action: Action) -> Action {
        match action {
            Action::Modifier(modifiers) => Action::Modifier(swap_modifiers(modifiers)),
            Action::KeyWithModifier(key, modifiers) => {
                Action::KeyWithModifier(key, swap_modifiers(modifiers))
            }
            other => other,
        }
    }

    match action {
        KeyAction::Single(action) => KeyAction::Single(swap_action(action)),
        KeyAction::Tap(action) => KeyAction::Tap(swap_action(action)),
        KeyAction::TapHold(tap, hold, profile) => {
            KeyAction::TapHold(swap_action(tap), swap_action(hold), profile)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmk_types_rynk::keycode::{HidKeyCode, KeyCode};

    #[test]
    fn handed_toggle_swaps_modifiers_and_keeps_the_profile() {
        let action = KeyAction::TapHold(
            Action::KeyWithModifier(HidKeyCode::A, ModifierCombination::LSHIFT),
            Action::Modifier(ModifierCombination::LCTRL),
            3,
        );
        let toggled = toggle_handed_key_action(action);
        assert!(matches!(
            toggled,
            KeyAction::TapHold(
                Action::KeyWithModifier(HidKeyCode::A, tap),
                Action::Modifier(hold),
                3,
            ) if tap == ModifierCombination::RSHIFT && hold == ModifierCombination::RCTRL
        ));
        assert!(matches!(
            toggle_handed_key_action(toggled),
            KeyAction::TapHold(_, _, 3)
        ));
        assert_eq!(toggle_handed_key_action(toggled), action);

        let key = KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::B)));
        assert_eq!(toggle_handed_key_action(key), key);
    }
}
