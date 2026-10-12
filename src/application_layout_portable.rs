//! Single-profile portability. Import copies content into an explicit existing
//! target; it never changes app matching, selection, or global resources.
use super::*;
use crate::application_layouts::{
    ApplicationLayout, APPLICATION_LAYOUT_LAYER_COUNT, APPLICATION_LAYOUT_UNSET_KEYCODE,
    DEFAULT_APPLICATION_LAYOUT_ID,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct ApplicationLayoutFile {
    format: String,
    version: u16,
    profile: ApplicationLayout,
}

impl EntropyApp {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn application_layout_file_dialog(&mut self, id: String, export: bool) {
        self.commit_pending_application_layout_edits();
        let action = if export {
            crate::app::file_dialog::FileDialogAction::ExportApplicationLayout(id.clone())
        } else {
            crate::app::file_dialog::FileDialogAction::ImportApplicationLayout(id.clone())
        };
        self.spawn_file_dialog(
            action,
            rfd::FileDialog::new()
                .add_filter("Entropy application layout", &["entapp"])
                .set_file_name(format!("{}.entapp", device_id_slug(&id))),
            export,
        );
    }

    pub(super) fn write_application_layout_export(&self, path: &Path, id: &str) -> Result<()> {
        let profile = self
            .application_layout_settings()
            .and_then(|settings| settings.layouts.get(id))
            .context("Application layout no longer exists")?
            .clone();
        if id == DEFAULT_APPLICATION_LAYOUT_ID {
            bail!("Use Export layout for Default");
        }
        let file = ApplicationLayoutFile {
            format: "entropy.application-layout".into(),
            version: 1,
            profile,
        };
        std::fs::write(path, serde_json::to_vec_pretty(&file)?)
            .context("Failed to export application layout")
    }

    pub(super) fn import_application_layout_from_path(
        &mut self,
        path: &Path,
        id: &str,
    ) -> Result<()> {
        let file: ApplicationLayoutFile = serde_json::from_slice(&std::fs::read(path)?)
            .context("Invalid application layout file (expected .entapp)")?;
        if file.format != "entropy.application-layout" || file.version != 1 {
            bail!("Unsupported application layout format or version");
        }
        if id == DEFAULT_APPLICATION_LAYOUT_ID {
            bail!("Use Import layout for Default; application imports require an explicit application target");
        }
        let source = file.profile;
        if source.layers.len() != APPLICATION_LAYOUT_LAYER_COUNT
            || source.layer_names.len() != APPLICATION_LAYOUT_LAYER_COUNT
            || source.encoder_stacks.len() != APPLICATION_LAYOUT_LAYER_COUNT
            || source
                .encoder_stacks
                .iter()
                .any(|stack| !matches!(stack.len(), 0 | 2..=4))
        {
            bail!("Invalid application layout dimensions or encoder stacks");
        }
        // Dynamic resources and custom keycodes use global/device IDs. Until
        // allocation/remapping exists, refuse them rather than point at unrelated
        // macros, Tap Dances or custom actions on another installation.
        if source
            .layers
            .iter()
            .flatten()
            .copied()
            .chain(
                source
                    .encoder_stacks
                    .iter()
                    .flatten()
                    .flat_map(|action| [action.counter_clockwise, action.clockwise]),
            )
            .any(|code| code >= 0x2000 && code != APPLICATION_LAYOUT_UNSET_KEYCODE)
        {
            bail!("Application layout uses device-specific actions (macros, Tap Dance, layers or custom keycodes). Safe resource remapping is not supported; nothing was imported. Use the full app-settings backup for shared resources.");
        }
        let key = self
            .application_layout_device_key()
            .context("Select a Macropad")?;
        let mut settings = self.app_settings.clone();
        let target = settings
            .application_layouts
            .get_mut(&key)
            .and_then(|settings| settings.layouts.get_mut(id))
            .context("Application target no longer exists")?;
        target.layers = source.layers;
        target.layer_names = source.layer_names;
        target.encoder_stacks = source.encoder_stacks;
        target.bump_revision();
        // Persist before publishing in memory; never touch the shared pictogram
        // library (save_app_settings intentionally writes that separate file).
        let destination = app_settings_path();
        let parent = destination.parent().context("Invalid settings path")?;
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        use std::io::Write;
        temporary.write_all(&serde_json::to_vec_pretty(&settings)?)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&destination)
            .context("Failed to save application layout")?;
        self.app_settings = settings;
        self.refresh_layer_picker_content_flags();
        Ok(())
    }
}
