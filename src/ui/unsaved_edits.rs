use super::qmk_settings_write_queue::QmkSettingWriteRequest;
use super::settings_write_queue::SettingsWriteRequest;
use super::vial_hid_task::LayerNameWrite;
use super::*;

/// Macro bytes and the actions they were edited as.
type KeptMacros = (Vec<Vec<u8>>, Vec<Vec<crate::keycode_picker::MacroAction>>);

/// Edits a board did not confirm before its connection dropped: the write
/// that failed and the ones queued behind it. They wait while no other board
/// connects, and are written again once the same board is back.
pub(super) struct UnsavedEdits {
    /// The board they were made on: the keyboard id names its model only,
    /// the device tells two boards of one model apart.
    owner: UnsavedEditsOwner,
    /// The latest of each setting.
    settings: Vec<SettingsWriteRequest>,
    touchpad: Vec<QmkSettingWriteRequest>,
    tap_hold_numeric: std::collections::BTreeMap<u16, u16>,
    /// Keys and encoders by layer. They are written a layer at a time once
    /// the board is back, as each write owns the board while it runs.
    layers: std::collections::BTreeMap<usize, LayerEdits>,
    /// Whole tables the editor writes as a diff or in full: the edited copy
    /// replaces what the board sends on connect.
    macros: Option<KeptMacros>,
    combos: Option<Vec<ComboEntry>>,
    combo_term: Option<u16>,
    tap_dance: Option<Vec<crate::keycode_picker::TapDanceEntry>>,
    key_overrides: Option<Vec<KeyOverrideEntry>>,
    /// Names an application layout gives the board's layers.
    layer_name_writes: Vec<LayerNameWrite>,
    /// Writes made outside any queue, by what they set and its index. Each
    /// puts its value back on screen and writes it again.
    rewrites: Vec<((&'static str, u16), Rewrite)>,
}

type Rewrite = Box<dyn FnOnce(&mut EntropyApp)>;

/// What a layer's keys and encoders should hold, by index.
#[derive(Default)]
pub(super) struct LayerEdits {
    pub(super) keys: std::collections::BTreeMap<usize, crate::keyboard::KeyBinding>,
    pub(super) encoders: std::collections::BTreeMap<usize, u16>,
}

type UnsavedEditsOwner = (u64, crate::device::DeviceIdentity);

impl UnsavedEdits {
    fn new(owner: UnsavedEditsOwner) -> Self {
        Self {
            owner,
            settings: Vec::new(),
            touchpad: Vec::new(),
            tap_hold_numeric: std::collections::BTreeMap::new(),
            layers: std::collections::BTreeMap::new(),
            macros: None,
            combos: None,
            combo_term: None,
            tap_dance: None,
            key_overrides: None,
            layer_name_writes: Vec::new(),
            rewrites: Vec::new(),
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
            + usize::from(self.macros.is_some())
            + usize::from(self.combos.is_some())
            + usize::from(self.combo_term.is_some())
            + usize::from(self.tap_dance.is_some())
            + usize::from(self.key_overrides.is_some())
            + self.layer_name_writes.len()
            + self.rewrites.len()
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

    pub(super) fn keep_layer_name_write(&mut self, request: LayerNameWrite) {
        self.layer_name_writes
            .retain(|kept| kept.layout_id != request.layout_id || kept.layer != request.layer);
        self.layer_name_writes.push(request);
    }
}

impl EntropyApp {
    /// The board selected now, as unsaved edits name it.
    fn unsaved_edits_owner(&self) -> Option<UnsavedEditsOwner> {
        let device = self
            .selected_device
            .and_then(|index| self.device_manager.devices().get(index))?;
        Some((self.current_keyboard_id?, device.stable_identity()))
    }

    /// The unsaved edits of the board selected now. Another board's are
    /// dropped: they wait for their own board only.
    pub(super) fn unsaved_edits_mut(&mut self) -> Option<&mut UnsavedEdits> {
        let owner = self.unsaved_edits_owner()?;
        if self
            .unsaved_edits
            .as_ref()
            .is_some_and(|edits| edits.owner != owner)
        {
            self.unsaved_edits = None;
        }
        Some(
            self.unsaved_edits
                .get_or_insert_with(|| UnsavedEdits::new(owner)),
        )
    }

    /// The unsaved edits of the board selected now, if it has any.
    fn own_unsaved_edits_mut(&mut self) -> Option<&mut UnsavedEdits> {
        let owner = self.unsaved_edits_owner()?;
        self.unsaved_edits
            .as_mut()
            .filter(|edits| edits.owner == owner)
    }

    /// Forgets a kept key the user set again: the newer edit is written.
    pub(super) fn forget_unsaved_key(&mut self, layer: usize, key_index: usize) {
        if let Some(edits) = self.own_unsaved_edits_mut() {
            if let Some(kept) = edits.layers.get_mut(&layer) {
                kept.keys.remove(&key_index);
            }
        }
    }

    pub(super) fn forget_unsaved_encoder(&mut self, layer: usize, encoder_index: usize) {
        if let Some(edits) = self.own_unsaved_edits_mut() {
            if let Some(kept) = edits.layers.get_mut(&layer) {
                kept.encoders.remove(&encoder_index);
            }
        }
    }

    /// Forgets the kept keys and encoders of a layer the user set as a whole.
    pub(super) fn forget_unsaved_layer(&mut self, layer: usize) {
        if let Some(edits) = self.own_unsaved_edits_mut() {
            edits.layers.remove(&layer);
        }
    }

    /// Keeps a write that failed as the board went away, to make again once
    /// it is back. `rewrite` puts the value back on screen and writes it; a
    /// later one for the same `what` and `index` replaces it.
    pub(super) fn keep_unsaved_rewrite(
        &mut self,
        error: &anyhow::Error,
        what: &'static str,
        index: u16,
        rewrite: impl FnOnce(&mut EntropyApp) + 'static,
    ) {
        if !crate::hid::is_disconnect_error(error) {
            return;
        }
        let Some(edits) = self.unsaved_edits_mut() else {
            return;
        };
        edits.rewrites.retain(|(kept, _)| *kept != (what, index));
        edits.rewrites.push(((what, index), Box::new(rewrite)));
    }

    /// Keeps a layer's name the board went away under. The connect takes the
    /// board's names over local ones, so it is set and written again.
    pub(super) fn keep_unsaved_layer_name(
        &mut self,
        error: &anyhow::Error,
        layer: usize,
        name: String,
    ) {
        self.keep_unsaved_rewrite(error, "layer_name", layer as u16, move |app| {
            while app.layer_names.len() <= layer {
                app.layer_names.push(app.layer_names.len().to_string());
            }
            app.layer_names[layer] = name.clone();
            app.keycode_picker.layer_names = app.layer_names.clone();
            save_layer_names(&app.layer_names, &app.current_device_name);
            let written = app
                .hid_device
                .as_ref()
                .map(|dev| dev.set_qmk_setting_string(200 + layer as u16, &name));
            if let Some(Err(error)) = written {
                log::warn!("Layer {layer} name not written again: {error}");
                app.keep_unsaved_layer_name(&error, layer, name);
            }
        });
    }

    /// Keeps what is still queued for the board as its connection drops,
    /// which would otherwise be dropped with it.
    pub(super) fn keep_unsaved_edits(&mut self) {
        // Without a board to keep them for, the reconnect drops them as before.
        if self.unsaved_edits_owner().is_none() {
            return;
        }
        self.keep_unsaved_layer_write();
        let settings = self.settings_write_queue.pending_requests();
        let touchpad = self.qmk_settings_write_queue.take_all();
        let tap_hold_numeric = std::mem::take(&mut self.pending_tap_hold_numeric_writes);
        let layer_name_writes = std::mem::take(&mut self.pending_layer_name_writes);
        let picker = &self.keycode_picker;
        let macros = picker
            .macros_dirty
            .then(|| (picker.macro_texts.clone(), picker.macro_actions.clone()));
        let tap_dance = (picker.tap_dance_entries != picker.tap_dance_synced_entries)
            .then(|| picker.tap_dance_entries.clone());
        let combos =
            (self.combo_entries != self.combo_synced_entries).then(|| self.combo_entries.clone());
        let combo_term = self.combo_term.filter(|_| self.combo_term_dirty);
        let key_overrides = self
            .key_override_dirty
            .then(|| self.key_override_entries.clone());
        let Some(edits) = self.unsaved_edits_mut() else {
            return;
        };
        edits.keep_settings(settings);
        for request in touchpad {
            edits.touchpad.retain(|kept| kept.qsid != request.qsid);
            edits.touchpad.push(request);
        }
        edits.tap_hold_numeric.extend(tap_hold_numeric);
        for request in layer_name_writes {
            edits.keep_layer_name_write(request);
        }
        edits.macros = macros.or(edits.macros.take());
        edits.combos = combos.or(edits.combos.take());
        edits.combo_term = combo_term.or(edits.combo_term);
        edits.tap_dance = tap_dance.or(edits.tap_dance.take());
        edits.key_overrides = key_overrides.or(edits.key_overrides.take());
        let kept = edits.len();
        if kept > 0 {
            log::info!(
                "Keeping {kept} unsaved edit(s) until {} is back",
                self.current_device_name
            );
        }
    }

    /// Writes the edits kept for the board just connected again. Another
    /// board connecting drops them.
    pub(super) fn rewrite_unsaved_edits(&mut self) {
        let owner = self.unsaved_edits_owner();
        if let Some(edits) = self
            .unsaved_edits
            .take_if(|edits| Some(&edits.owner) != owner.as_ref())
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
        let macros = edits.macros.take();
        let combos = edits.combos.take();
        let combo_term = edits.combo_term.take();
        let tap_dance = edits.tap_dance.take();
        let key_overrides = edits.key_overrides.take();
        let rewrites = std::mem::take(&mut edits.rewrites);
        let layer_name_writes = std::mem::take(&mut edits.layer_name_writes);
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
        // Writes made outside any queue go first, while no worker holds the
        // board they need.
        for (_, rewrite) in rewrites {
            rewrite(self);
        }
        self.rewrite_unsaved_settings_writes(settings);
        // Debounced values follow: they were edited after anything queued.
        self.rewrite_unsaved_touchpad_values(touchpad);
        if !tap_hold_numeric.is_empty() {
            self.pending_tap_hold_numeric_writes
                .extend(tap_hold_numeric);
            self.tap_hold_numeric_write_due = Some(std::time::Instant::now());
        }
        // A table is taken back only while the board has as many entries as
        // when it was edited; their writers then write what differs.
        let picker = &mut self.keycode_picker;
        if let Some((texts, actions)) =
            macros.filter(|(texts, _)| texts.len() == picker.macro_texts.len())
        {
            picker.macro_texts = texts;
            picker.macro_actions = actions;
            picker.mark_macros_dirty();
        }
        if let Some(entries) =
            tap_dance.filter(|entries| entries.len() == picker.tap_dance_entries.len())
        {
            picker.tap_dance_entries = entries;
            picker.tap_dance_dirty = true;
        }
        if let Some(entries) = combos.filter(|entries| entries.len() == self.combo_entries.len()) {
            // The list shows as many combos as the board's use; an edited
            // one further down stays in view.
            let edited = entries
                .iter()
                .zip(&self.combo_synced_entries)
                .rposition(|(edited, held)| edited != held)
                .map_or(0, |index| index + 1);
            self.combo_visible_count = self.combo_visible_count.max(edited);
            self.combo_entries = entries;
            self.mark_combo_dirty();
        }
        if let Some(term) = combo_term {
            self.combo_term = Some(term);
            self.combo_term_dirty = true;
        }
        if let Some(entries) =
            key_overrides.filter(|entries| entries.len() == self.key_override_entries.len())
        {
            self.key_override_entries = entries;
            self.key_override_dirty = true;
        }
        // Their queue checks each against this connection.
        let device = self
            .selected_device
            .and_then(|index| self.device_manager.devices().get(index))
            .cloned();
        for mut request in layer_name_writes {
            request.generation = self.connection_generation;
            if let Some(device) = &device {
                request.device = device.clone();
            }
            self.pending_layer_name_writes.push_back(request);
        }
    }

    /// The next layer whose edits wait for the board connected now.
    pub(super) fn take_unsaved_layer(&mut self) -> Option<(usize, LayerEdits)> {
        let edits = self.own_unsaved_edits_mut()?;
        let layer = edits.layers.pop_first();
        if edits.len() == 0 {
            self.unsaved_edits = None;
        }
        layer
    }
}
