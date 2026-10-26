//! The connected keyboard as the UI sees it, whatever protocol configures it.
//!
//! Every write the UI makes goes through a method here, so the protocol is
//! chosen in one place. The methods speak Entropy's model — Vial keycodes,
//! Vial macro bytecode, QMK setting ids — because that is what the editors
//! hold; a session for another protocol translates at this boundary. Loaders
//! and features that exist only on Vial take the transport from
//! [`DeviceSession::as_vial`].

use anyhow::{Context, Result};

use crate::hid::HidDevice;
use crate::keyboard::KeyBinding;

/// Unlocked, unlocking, and the keys to hold.
pub(crate) type UnlockStatus = (bool, bool, Vec<(u8, u8)>);

pub(crate) enum DeviceSession {
    Vial(HidDevice),
}

impl From<HidDevice> for DeviceSession {
    fn from(hid: HidDevice) -> Self {
        Self::Vial(hid)
    }
}

impl DeviceSession {
    /// The Vial transport, for Vial-only loaders and features.
    pub(crate) fn as_vial(&self) -> Option<&HidDevice> {
        match self {
            Self::Vial(hid) => Some(hid),
        }
    }

    /// Hands the Vial transport to a new owner, or gives the session back.
    pub(crate) fn into_vial(self) -> Result<HidDevice, Box<Self>> {
        match self {
            Self::Vial(hid) => Ok(hid),
        }
    }

    /// [`Self::as_vial`] for an operation that only exists on Vial.
    pub(crate) fn require_vial(&self) -> Result<&HidDevice> {
        self.as_vial()
            .context("this keyboard does not support the operation")
    }

    pub(crate) fn is_bluetooth_transport(&self) -> bool {
        match self {
            Self::Vial(hid) => hid.is_bluetooth_transport(),
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn macos_hid_operation_lock(&self) -> Option<std::sync::MutexGuard<'static, ()>> {
        match self {
            Self::Vial(hid) => hid.macos_hid_operation_lock(),
        }
    }

    pub(crate) fn is_transport_available(&self) -> bool {
        match self {
            Self::Vial(hid) => hid.is_transport_available(),
        }
    }

    pub(crate) fn with_host_output_paused_for_settings<T>(
        &self,
        operation: impl FnOnce() -> T,
    ) -> T {
        match self {
            Self::Vial(hid) => hid.with_host_output_paused_for_settings(operation),
        }
    }

    pub(crate) fn set_keycode(&self, layer: u8, row: u8, col: u8, keycode: u16) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_keycode(layer, row, col, keycode),
        }
    }

    pub(crate) fn set_rmk_key_action(
        &self,
        layer: u8,
        row: u8,
        col: u8,
        action: rmk_types::action::KeyAction,
    ) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_rmk_key_action(layer, row, col, action),
        }
    }

    pub(crate) fn set_key_binding(
        &self,
        layer: u8,
        row: u8,
        col: u8,
        binding: KeyBinding,
    ) -> Result<()> {
        match binding {
            KeyBinding::Vial(keycode) => self.set_keycode(layer, row, col, keycode),
            KeyBinding::Rmk(action) => self.set_rmk_key_action(layer, row, col, action),
            KeyBinding::Rynk(_) => {
                anyhow::bail!("Rynk key action cannot be written to a Vial board")
            }
        }
    }

    pub(crate) fn set_encoder(
        &self,
        layer: u8,
        idx: u8,
        direction: u8,
        keycode: u16,
    ) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_encoder(layer, idx, direction, keycode),
        }
    }

    pub(crate) fn get_macro_buffer_size(&self) -> Result<u16> {
        match self {
            Self::Vial(hid) => hid.get_macro_buffer_size(),
        }
    }

    pub(crate) fn get_macro_buffer(&self, size: u16, count: u8) -> Result<Vec<u8>> {
        match self {
            Self::Vial(hid) => hid.get_macro_buffer(size, count),
        }
    }

    pub(crate) fn set_macro_buffer(&self, data: &[u8]) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_macro_buffer(data),
        }
    }

    pub(crate) fn set_combo_binding(
        &self,
        idx: u8,
        keys: [u16; 4],
        output: KeyBinding,
    ) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_combo_binding(idx, keys, output),
        }
    }

    pub(crate) fn set_rmk_combo_layer(&self, index: u8, layer: Option<u8>) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_rmk_combo_layer(index, layer),
        }
    }

    pub(crate) fn set_tap_dance_bindings(
        &self,
        idx: u8,
        actions: [KeyBinding; 4],
        tapping_term: u16,
    ) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_tap_dance_bindings(idx, actions, tapping_term),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_key_override(
        &self,
        idx: u8,
        trigger: u16,
        replacement: u16,
        layers: u16,
        trigger_mods: u8,
        negative_mod_mask: u8,
        suppressed_mods: u8,
        options: u8,
    ) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_key_override(
                idx,
                trigger,
                replacement,
                layers,
                trigger_mods,
                negative_mod_mask,
                suppressed_mods,
                options,
            ),
        }
    }

    pub(crate) fn set_alt_repeat_key(
        &self,
        idx: u8,
        keycode: u16,
        alt_keycode: u16,
        allowed_mods: u8,
        options: u8,
    ) -> Result<()> {
        match self {
            Self::Vial(hid) => {
                hid.set_alt_repeat_key(idx, keycode, alt_keycode, allowed_mods, options)
            }
        }
    }

    pub(crate) fn get_qmk_setting_u8(&self, qsid: u16) -> Result<u8> {
        match self {
            Self::Vial(hid) => hid.get_qmk_setting_u8(qsid),
        }
    }

    pub(crate) fn set_qmk_setting_u8(&self, qsid: u16, value: u8) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_qmk_setting_u8(qsid, value),
        }
    }

    pub(crate) fn get_qmk_setting_u16(&self, qsid: u16) -> Result<u16> {
        match self {
            Self::Vial(hid) => hid.get_qmk_setting_u16(qsid),
        }
    }

    pub(crate) fn set_qmk_setting_u16(&self, qsid: u16, value: u16) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_qmk_setting_u16(qsid, value),
        }
    }

    pub(crate) fn get_qmk_setting_string(&self, qsid: u16) -> Result<String> {
        match self {
            Self::Vial(hid) => hid.get_qmk_setting_string(qsid),
        }
    }

    pub(crate) fn set_qmk_setting_string(&self, qsid: u16, value: &str) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_qmk_setting_string(qsid, value),
        }
    }

    pub(crate) fn set_layout_options(&self, options: u32) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.set_layout_options(options),
        }
    }

    pub(crate) fn get_unlock_status_with_progress(&self) -> Result<UnlockStatus> {
        match self {
            Self::Vial(hid) => hid.get_unlock_status_with_progress(),
        }
    }

    pub(crate) fn unlock_start(&self) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.unlock_start(),
        }
    }

    pub(crate) fn unlock_poll(&self) -> Result<(bool, bool, u8)> {
        match self {
            Self::Vial(hid) => hid.unlock_poll(),
        }
    }

    pub(crate) fn lock(&self) -> Result<()> {
        match self {
            Self::Vial(hid) => hid.lock(),
        }
    }

    pub(crate) fn get_switch_matrix(&self, rows: usize, cols: usize) -> Result<Vec<bool>> {
        match self {
            Self::Vial(hid) => hid.get_switch_matrix(rows, cols),
        }
    }

    pub(crate) fn get_battery_halves(&self) -> Result<Option<crate::hid::BatteryHalves>> {
        match self {
            Self::Vial(hid) => hid.get_battery_halves(),
        }
    }
}
