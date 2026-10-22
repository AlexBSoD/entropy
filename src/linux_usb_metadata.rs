//! Independent completeness proof for hidapi's lossy Linux enumeration.
//! Only read sysfs metadata: never open hidraw, issue ioctls, or enter BlueZ.
use super::*;
use std::{collections::HashMap, fs, os::unix::fs::MetadataExt, path::Path};

fn text(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .map_err(|e| format!("USB metadata {}: {e}", path.display()))
}

fn hex(value: &str) -> Result<u16, String> {
    u16::from_str_radix(value, 16).map_err(|_| format!("Invalid USB/HID identifier: {value:?}"))
}

// Parse short HID items with global Push/Pop and collection-local usages.
// Unsupported long items and malformed/truncated streams are incomplete, not
// proof of a non-Vial endpoint. Finish parsing even after finding Vial usage.
fn vial_descriptor(bytes: &[u8]) -> Result<bool, String> {
    let invalid = || "USB report descriptor incomplete or unsupported".to_owned();
    if bytes.is_empty() || bytes.len() > 65536 {
        return Err(invalid());
    }
    let mut offset = 0;
    let mut page = 0u32;
    let mut globals = Vec::new();
    let mut usages = Vec::new();
    let mut depth = 0usize;
    let mut collections = 0usize;
    let mut found = false;
    while offset < bytes.len() {
        let prefix = bytes[offset];
        offset += 1;
        if prefix == 0xfe {
            return Err(invalid());
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        if offset + size > bytes.len() {
            return Err(invalid());
        }
        let mut value = 0u32;
        for (i, b) in bytes[offset..offset + size].iter().enumerate() {
            value |= u32::from(*b) << (i * 8);
        }
        offset += size;
        match prefix & 0xfc {
            0x04 => {
                if value > 0xffff {
                    return Err(invalid());
                }
                page = value;
            } // Usage Page
            0xa4 => globals.push(page),
            0xb4 => page = globals.pop().ok_or_else(invalid)?,
            0x08 => usages.push(if size == 4 {
                (value >> 16, value & 0xffff)
            } else {
                (page, value)
            }),
            0xa0 => {
                // Collection: local usage identifies this collection.
                if size != 1 || usages.is_empty() {
                    return Err(invalid());
                }
                found |= usages.contains(&(0xff60, 0x61));
                depth += 1;
                collections += 1;
            }
            0xc0 => {
                if size != 0 || depth == 0 {
                    return Err(invalid());
                }
                depth -= 1;
            }
            _ => {}
        }
        if prefix & 0x0c == 0 {
            usages.clear();
        } // every Main item resets local state
    }
    if depth != 0 || !globals.is_empty() || collections == 0 {
        return Err(invalid());
    }
    Ok(found)
}

fn endpoints(root: &Path) -> Result<Vec<Device>, String> {
    let mut devices = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| format!("USB hidraw class enumeration: {e}"))? {
        let entry = entry.map_err(|e| format!("USB hidraw class entry: {e}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "USB hidraw name is not UTF-8")?;
        if !name
            .strip_prefix("hidraw")
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(format!("Unexpected hidraw class entry: {name}"));
        }
        let hid = fs::canonicalize(entry.path().join("device"))
            .map_err(|e| format!("USB HID ancestry unavailable: {e}"))?;
        let metadata =
            fs::metadata(&hid).map_err(|e| format!("USB HID instance unavailable: {e}"))?;
        let event = text(&hid.join("uevent"))?;
        let mut fields = HashMap::new();
        for line in event.lines() {
            if let Some((key, value)) = line.split_once('=') {
                if fields.insert(key, value).is_some() {
                    return Err("Duplicate HID identity field".into());
                }
            }
        }
        let required = |key| {
            fields
                .get(key)
                .copied()
                .ok_or_else(|| format!("USB HID field missing: {key}"))
        };
        let ids: Vec<_> = required("HID_ID")?.split(':').collect();
        if ids.len() != 3 {
            return Err("Malformed HID_ID".into());
        }
        let bus = hex(ids[0])?;
        let vendor_id = hex(ids[1])?;
        let product_id = hex(ids[2])?;
        // HID bus, not USB radio ancestry, determines peripheral transport.
        if bus != 3 {
            continue;
        }
        required("HID_NAME")?;
        let serial_number = required("HID_UNIQ")?.to_owned();
        let descriptor = fs::read(hid.join("report_descriptor"))
            .map_err(|e| format!("USB report descriptor unavailable: {e}"))?;
        if !vial_descriptor(&descriptor)? {
            continue;
        }
        let mut usb = None;
        for ancestor in hid.ancestors().skip(1) {
            // A half-read USB ancestor must not be silently skipped.
            let has_vendor = ancestor
                .join("idVendor")
                .try_exists()
                .map_err(|e| format!("USB ancestry: {e}"))?;
            let has_product = ancestor
                .join("idProduct")
                .try_exists()
                .map_err(|e| format!("USB ancestry: {e}"))?;
            if has_vendor || has_product {
                usb = Some(ancestor);
                break;
            }
        }
        let usb = usb.ok_or("USB peripheral ancestor unavailable")?;
        if hex(&text(&usb.join("idVendor"))?)? != vendor_id
            || hex(&text(&usb.join("idProduct"))?)? != product_id
        {
            return Err("USB ancestor/HID identity mismatch".into());
        }
        // sysfs omits a string attribute legitimately only when the USB device
        // descriptor declares index zero. A missing/failed nonzero string is
        // incomplete metadata, not an absent identity. The descriptors file also
        // contains configurations; validate its leading 18-byte device header.
        let usb_descriptor = fs::read(usb.join("descriptors"))
            .map_err(|e| format!("USB device descriptor unavailable: {e}"))?;
        if usb_descriptor.len() < 18
            || usb_descriptor[0] != 18
            || usb_descriptor[1] != 1
            || u16::from_le_bytes([usb_descriptor[8], usb_descriptor[9]]) != vendor_id
            || u16::from_le_bytes([usb_descriptor[10], usb_descriptor[11]]) != product_id
        {
            return Err("USB device descriptor invalid or identity mismatch".into());
        }
        let identity_string = |field: &str, index: u8, absent: &str| -> Result<String, String> {
            let path = usb.join(field);
            if index != 0 {
                return text(&path);
            }
            match fs::read_to_string(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(absent.to_owned()),
                Err(e) => Err(format!("USB metadata {}: {e}", path.display())),
                Ok(_) => Err(format!("USB metadata {} contradicts string index zero", path.display())),
            }
        };
        let mut device = Device {
            name: identity_string("product", usb_descriptor[15], "Unknown Keyboard")?,
            manufacturer: identity_string("manufacturer", usb_descriptor[14], "")?,
            vendor_id,
            product_id,
            serial_number,
            path: format!("/dev/{name}"),
            bus_type: "Usb".into(),
            instance_token: format!(
                "sysfs:{}:{}:{}",
                hid.display(),
                metadata.dev(),
                metadata.ino()
            ),
            firmware: FirmwareProtocol::Vial,
        };
        device.normalize_discovered_identity();
        devices.push(device);
    }
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_usb_descriptor_absence_proof_requires_complete_collection_usage() {
        // A collection without a resolved usage is not evidence of non-Vial.
        for bytes in [
            &[0xa1, 0x01, 0xc0][..],
            &[0x06, 0x60, 0xff, 0xa1, 0x01, 0xc0][..],
        ] {
            assert!(vial_descriptor(bytes).is_err());
        }
    }

    #[test]
    fn linux_usb_descriptor_proof_handles_usage_scopes_and_rejects_truncation() {
        let vial = [0x06, 0x60, 0xff, 0x09, 0x61, 0xa1, 0x01, 0xc0];
        assert!(vial_descriptor(&vial).unwrap());
        assert!(vial_descriptor(&[0x0b, 0x61, 0x00, 0x60, 0xff, 0xa1, 0x01, 0xc0]).unwrap());
        // Push/Pop restores the page used by the collection-local usage.
        assert!(
            vial_descriptor(&[
                0x06, 0x60, 0xff, 0xa4, 0x05, 0x01, 0xb4, 0x09, 0x61, 0xa1, 0x01, 0xc0
            ])
            .unwrap()
        );
        assert!(!vial_descriptor(&[0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0xc0]).unwrap());
        for end in 0..vial.len() {
            assert!(vial_descriptor(&vial[..end]).is_err());
        }
        for suffix in [
            &[0x06][..],
            &[0xfe, 0x00, 0x00][..],
            &[0xb4][..],
            &[0xc0][..],
        ] {
            let mut broken = vial.to_vec();
            broken.extend_from_slice(suffix);
            assert!(
                vial_descriptor(&broken).is_err(),
                "must parse to the end after finding Vial"
            );
        }
    }
}

pub(super) fn validate(root: &Path, devices: Vec<Device>) -> Result<Vec<Device>, String> {
    let expected = endpoints(root)?;
    if expected.len() != devices.len()
        || expected.iter().any(|expected| {
            !devices.iter().any(|actual| {
                actual.path == expected.path
                    && actual.instance_token == expected.instance_token
                    && actual.stable_identity() == expected.stable_identity()
            })
        })
    {
        return Err(
            "USB metadata snapshot incomplete or identity changed; retaining inventory".into(),
        );
    }
    Ok(devices)
}
