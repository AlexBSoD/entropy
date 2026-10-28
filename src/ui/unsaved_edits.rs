use super::qmk_settings_write_queue::QmkSettingWriteRequest;
use super::settings_write_queue::SettingsWriteRequest;
use super::*;

/// Edits a board did not confirm before its connection dropped: the write
/// that failed and the ones queued behind it. They wait while no other board
/// connects, and are written again once the same board is back.
pub(super) struct UnsavedEdits {
    /// The board they were made on.
    keyboard_id: u64,
    /// The latest of each setting.
    settings: Vec<SettingsWriteRequest>,
    touchpad: Vec<QmkSettingWriteRequest>,
    tap_hold_numeric: std::collections::BTreeMap<u16, u16>,
    /// Keys and encoders by layer. They are written a layer at a time once
    /// the board is back, as each write owns the board while it runs.
    layers: std::collections::BTreeMap<usize, LayerEdits>,
}

/// What a layer's keys and encoders should hold, by index.
#[derive(Default)]
pub(super) struct LayerEdits {
    pub(super) keys: std::collections::BTreeMap<usize, crate::keyboard::KeyBinding>,
    pub(super) encoders: std::collections::BTreeMap<usize, u16>,
}

impl UnsavedEdits {
    fn new(keyboard_id: u64) -> Self {
        Self {
            keyboard_id,
            settings: Vec::new(),
            touchpad: Vec::new(),
            tap_hold_numeric: std::collections::BTreeMap::new(),
            layers: std::collections::BTreeMap::new(),
        }
    }

    fn len(&self) -> usize {
        self.settings.len()
            + self.touchpad.len()
            + self.tap_hold_numeric.len()
            + self
                .layers
                .values()
                .map(|layer| layer.keys.len() + layer.encoders.len())
                .sum::<usize>()
    }

    pub(super) fn keep_settings(&mut self, requests: Vec<SettingsWriteRequest>) {
        for request in requests {
            self.settings.retain(|kept| kept.qsid != request.qsid);
            self.settings.push(request);
        }
    }

    pub(super) fn keep_key(
        &mut self,
        layer: usize,
        key_index: usize,
        binding: crate::keyboard::KeyBinding,
    ) {
        self.layers
            .entry(layer)
            .or_default()
            .keys
            .insert(key_index, binding);
    }

    pub(super) fn keep_encoder(&mut self, layer: usize, encoder_index: usize, keycode: u16) {
        self.layers
            .entry(layer)
            .or_default()
            .encoders
            .insert(encoder_index, keycode);
    }
}

impl EntropyApp {
    /// The unsaved edits of the board connected now. Another board's are
    /// dropped: they wait for their own board only.
    pub(super) fn unsaved_edits_mut(&mut self) -> Option<&mut UnsavedEdits> {
        let keyboard_id = self.current_keyboard_id?;
        if self
            .unsaved_edits
            .as_ref()
            .is_some_and(|edits| edits.keyboard_id != keyboard_id)
        {
            self.unsaved_edits = None;
        }
        Some(
            self.unsaved_edits
                .get_or_insert_with(|| UnsavedEdits::new(keyboard_id)),
        )
    }

    /// Keeps what is still queued for the board as its connection drops,
    /// which would otherwise be dropped with it.
    pub(super) fn keep_unsaved_edits(&mut self) {
        self.keep_unsaved_layer_write();
        let settings = self.settings_write_queue.pending_requests();
        let touchpad = self.qmk_settings_write_queue.take_all();
        let tap_hold_numeric = std::mem::take(&mut self.pending_tap_hold_numeric_writes);
        let Some(edits) = self.unsaved_edits_mut() else {
            return;
        };
        edits.keep_settings(settings);
        for request in touchpad {
            edits.touchpad.retain(|kept| kept.qsid != request.qsid);
            edits.touchpad.push(request);
        }
        edits.tap_hold_numeric.extend(tap_hold_numeric);
        let kept = edits.len();
        if kept > 0 {
            log::info!(
                "Keeping {kept} unsaved edit(s) until {} is back",
                self.current_device_name
            );
        }
    }

    /// Writes the edits kept for `keyboard_id` again, now that it connected.
    /// Another board connecting drops them.
    pub(super) fn rewrite_unsaved_edits(&mut self, keyboard_id: u64) {
        if let Some(edits) = self
            .unsaved_edits
            .take_if(|edits| edits.keyboard_id != keyboard_id)
        {
            log::info!(
                "Dropping {} unsaved edit(s) of another keyboard",
                edits.len()
            );
            return;
        }
        let Some(edits) = self.unsaved_edits.as_mut() else {
            return;
        };
        let count = edits.len();
        let settings = std::mem::take(&mut edits.settings);
        let touchpad = std::mem::take(&mut edits.touchpad);
        let tap_hold_numeric = std::mem::take(&mut edits.tap_hold_numeric);
        if edits.layers.is_empty() {
            self.unsaved_edits = None;
        }
        if count == 0 {
            return;
        }
        log::info!(
            "Writing {count} edit(s) {} had not saved before it went away",
            self.current_device_name
        );
        self.rewrite_unsaved_settings_writes(settings);
        // Debounced values follow: they were edited after anything queued.
        self.rewrite_unsaved_touchpad_values(touchpad);
        if !tap_hold_numeric.is_empty() {
            self.pending_tap_hold_numeric_writes
                .extend(tap_hold_numeric);
            self.tap_hold_numeric_write_due = Some(std::time::Instant::now());
        }
    }

    /// The next layer whose edits wait for the board connected now.
    pub(super) fn take_unsaved_layer(&mut self) -> Option<(usize, LayerEdits)> {
        let keyboard_id = self.current_keyboard_id?;
        let edits = self
            .unsaved_edits
            .as_mut()
            .filter(|edits| edits.keyboard_id == keyboard_id)?;
        let layer = edits.layers.pop_first();
        if edits.len() == 0 {
            self.unsaved_edits = None;
        }
        layer
    }
}
