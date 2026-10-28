use super::*;

/// QSIDs of the layer colors, LED brightness and LED timeout. Bluetooth
/// profile colors follow (318..) and stay with the Bluetooth settings.
const LAYER_LED_QSIDS: std::ops::RangeInclusive<u16> = 300..=317;

/// Serialized meaning of a Layer LED setting: which value it holds and how,
/// so a backup lands only where the setting means the same.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct EntLayerLedField {
    role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer: Option<u8>,
    max: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    variants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unit: Option<String>,
}

pub(super) fn layer_led_setting_valid(setting: &EntPortableSetting) -> bool {
    let Some(field) = &setting.layer_led else {
        return false;
    };
    let limit = match setting.width {
        EntPortableSettingWidth::U8 => u8::MAX as u16,
        EntPortableSettingWidth::U16 => u16::MAX,
    };
    if setting.id != "layer_led"
        || !LAYER_LED_QSIDS.contains(&setting.qsid)
        || setting.module.is_some()
        || !setting.variants.is_empty()
        || field.max > limit
        || setting.value > field.max
        || (!field.variants.is_empty() && usize::from(setting.value) >= field.variants.len())
    {
        return false;
    }
    match (field.role.as_str(), field.layer, field.unit.as_deref()) {
        ("layer_color", Some(_), None) => {
            setting.width == EntPortableSettingWidth::U8
                && usize::from(field.max) == LAYER_LED_PALETTE.len() - 1
                && field.variants.is_empty()
        }
        ("brightness", None, None) => true,
        ("timeout", None, Some("minutes" | "seconds")) => true,
        _ => false,
    }
}

impl EntropyApp {
    /// The layer colors, brightness and timeout the board reported, each with
    /// what it means: a color by its layer, the timeout with its unit.
    pub(super) fn entlayout_layer_led_settings_snapshot(&self) -> Vec<EntPortableSetting> {
        let leds = &self.layer_led_settings;
        if !leds.supported {
            return Vec::new();
        }
        let reported = |qsid: u16| {
            LAYER_LED_QSIDS.contains(&qsid)
                && self.supported_qmk_settings.contains(&qsid)
                && leds.loaded_qsids.contains(&qsid)
        };
        let setting = |qsid, width: u8, value, field| EntPortableSetting {
            id: "layer_led".to_owned(),
            qsid,
            width: if width > 1 {
                EntPortableSettingWidth::U16
            } else {
                EntPortableSettingWidth::U8
            },
            value,
            variants: Vec::new(),
            module: None,
            layer_led: Some(field),
        };
        let mut settings = Vec::new();
        for (layer, color) in leds.layer_colors.iter().enumerate() {
            let (true, Ok(layer)) = (reported(color.qsid), u8::try_from(layer)) else {
                continue;
            };
            let field = EntLayerLedField {
                role: "layer_color".to_owned(),
                layer: Some(layer),
                max: (LAYER_LED_PALETTE.len() - 1) as u16,
                variants: Vec::new(),
                unit: None,
            };
            settings.push(setting(color.qsid, 1, color.value.into(), field));
        }
        let unit = match leds.timeout_unit {
            LayerLedTimeoutUnit::Minutes => "minutes",
            LayerLedTimeoutUnit::Seconds => "seconds",
        };
        for (role, numeric, unit) in [
            ("brightness", &leds.brightness, None),
            ("timeout", &leds.timeout, Some(unit)),
        ] {
            let Some(numeric) = numeric.as_ref().filter(|numeric| reported(numeric.qsid)) else {
                continue;
            };
            let field = EntLayerLedField {
                role: role.to_owned(),
                layer: None,
                max: numeric.max,
                variants: numeric.variants.clone(),
                unit: unit.map(str::to_owned),
            };
            settings.push(setting(numeric.qsid, numeric.width, numeric.value, field));
        }
        settings.retain(layer_led_setting_valid);
        settings
    }

    /// The other QSIDs a layer color is written to on this board, as the
    /// LED page writes them.
    pub(in crate::app) fn entlayout_layer_led_linked_qsids(
        &self,
        setting: &EntPortableSetting,
    ) -> Vec<u16> {
        if setting.layer_led.is_none() {
            return Vec::new();
        }
        self.layer_led_settings
            .layer_colors
            .iter()
            .find(|color| color.qsid == setting.qsid)
            .map(|color| color.linked_qsids.clone())
            .unwrap_or_default()
    }
}
