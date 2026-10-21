//! Data-only boundary shared by native IORegistry discovery and portable tests.
//! Field names and DevSrvsID path follow pinned hidapi 2.6.6 mac/hid.c.
use super::*;

#[derive(Clone)]
pub(crate) struct MacUsbMetadata {
    pub entry_id: u64,
    pub transport: String,
    pub usb_ancestor: bool,
    pub bluetooth_ancestor: bool,
    pub vendor: u16,
    pub product: u16,
    pub name: String,
    pub manufacturer: String,
    pub serial: String,
    pub usages: Vec<(u32, u32)>,
    pub input_bytes: u32,
    pub output_bytes: u32,
}

pub(crate) fn device_from_metadata(m: MacUsbMetadata) -> Result<Option<Device>, String> {
    if m.transport != "USB" || m.bluetooth_ancestor || !m.usb_ancestor {
        return Err("USB transport ancestry incomplete or inconsistent".into());
    }
    if !m.usages.contains(&(0xff60, 0x61)) {
        return Ok(None);
    }
    if m.entry_id == 0
        || m.vendor == 0
        || m.product == 0
        || m.input_bytes < 32
        || m.output_bytes < 32
    {
        return Err("USB Vial identity/report metadata incomplete".into());
    }
    let path = format!("DevSrvsID:{}", m.entry_id);
    let mut device = Device {
        name: m.name,
        vendor_id: m.vendor,
        product_id: m.product,
        manufacturer: m.manufacturer,
        serial_number: m.serial,
        bus_type: "Usb".into(),
        instance_token: device_instance_token(&path),
        path,
        firmware: FirmwareProtocol::Vial,
    };
    device.normalize_discovered_identity();
    Ok(Some(device))
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(in crate::device) fn fixture() -> MacUsbMetadata {
        MacUsbMetadata {
            entry_id: 4294968123,
            transport: "USB".into(),
            usb_ancestor: true,
            bluetooth_ancestor: false,
            vendor: 0xe126,
            product: 0x0009,
            name: "Macropad".into(),
            manufacturer: "Ergohaven".into(),
            serial: "".into(),
            usages: vec![(1, 6), (0xff60, 0x61)],
            input_bytes: 32,
            output_bytes: 32,
        }
    }
    #[test]
    fn macos_usb_metadata_matches_hidapi_identity_and_rejects_incomplete_snapshot() {
        let m = fixture();
        let d = device_from_metadata(m.clone()).unwrap().unwrap();
        assert_eq!(d.path, "DevSrvsID:4294968123");
        let mut normal = Device {
            name: m.name.clone(),
            vendor_id: m.vendor,
            product_id: m.product,
            manufacturer: m.manufacturer.clone(),
            serial_number: m.serial.clone(),
            bus_type: "Usb".into(),
            path: d.path.clone(),
            instance_token: device_instance_token(&d.path),
            firmware: FirmwareProtocol::Vial,
        };
        normal.normalize_discovered_identity();
        assert_eq!(d.stable_identity(), normal.stable_identity());
        assert_eq!(d.display_name_cache_key(), normal.display_name_cache_key());
        assert_eq!(d.instance_token, normal.instance_token);
        for i in 0..6 {
            let mut bad = m.clone();
            match i {
                0 => bad.transport = "Bluetooth Low Energy".into(),
                1 => bad.bluetooth_ancestor = true,
                2 => bad.usb_ancestor = false,
                3 => bad.entry_id = 0,
                4 => bad.input_bytes = 0,
                _ => bad.output_bytes = 31,
            }
            assert!(device_from_metadata(bad).is_err(), "case {i}");
        }
        let mut other = m;
        other.usages = vec![(1, 6)];
        assert!(device_from_metadata(other).unwrap().is_none());
    }
}
