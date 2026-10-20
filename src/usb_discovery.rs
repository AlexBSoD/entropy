//! Discovery while a Bluetooth owner is active. Never send VIA/Vial requests,
//! open a Bluetooth HID endpoint, or invoke the BlueZ discovery/cache pipeline.
use super::*;

#[cfg(target_os = "linux")]
pub(super) fn scan() -> Result<Vec<Device>, String> {
    // Pinned hidapi linux-native-basic-udev enumerates sysfs metadata/report
    // descriptors only (no hidraw open/ioctl). Do not call scan_devices(): it
    // additionally enters BlueZ. Return USB endpoints only.
    let api = hidapi::HidApi::new().map_err(|e| format!("USB metadata scan failed: {e}"))?;
    Ok(api
        .device_list()
        .filter(|i| {
            matches!(i.bus_type(), hidapi::BusType::Usb)
                && i.usage_page() == 0xFF60
                && i.usage() == 0x61
        })
        .map(|i| {
            let path = i.path().to_string_lossy().to_string();
            let mut d = Device {
                name: i.product_string().unwrap_or("Unknown Keyboard").into(),
                vendor_id: i.vendor_id(),
                product_id: i.product_id(),
                manufacturer: i.manufacturer_string().unwrap_or("").into(),
                serial_number: i.serial_number().unwrap_or("").into(),
                bus_type: "Usb".into(),
                instance_token: device_instance_token(&path),
                path,
                firmware: FirmwareProtocol::Vial,
            };
            d.normalize_discovered_identity();
            d
        })
        .collect())
}

// Stop before an adapter's USB ancestor: Bluetooth peripherals often sit under
// an USB Bluetooth radio. Only USB VID/PID ancestors prove a USB peripheral.
#[cfg(any(target_os = "windows", test))]
fn windows_usb_ancestry(ids: impl IntoIterator<Item = String>) -> bool {
    for id in ids {
        let id = id.to_ascii_uppercase();
        if id.starts_with("BTH") || id.starts_with("BLUETOOTH\\") {
            return false;
        }
        if id.starts_with("USB\\VID_") {
            return true;
        }
    }
    false
}

#[cfg(target_os = "windows")]
pub(super) fn scan() -> Result<Vec<Device>, String> {
    windows_usb::scan()
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub(super) fn scan() -> Result<Vec<Device>, String> {
    Err("USB-only discovery is not available on this platform; Bluetooth protection remains enabled".into())
}

#[cfg(target_os = "windows")]
mod windows_usb {
    use super::*;
    use std::{
        mem::{size_of, zeroed},
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*, Properties::*},
        Foundation::*,
        Storage::FileSystem::*,
    };
    fn text(w: &[u16]) -> String {
        String::from_utf16_lossy(&w[..w.iter().position(|c| *c == 0).unwrap_or(w.len())])
    }
    // Registry/Config Manager calls only. No HID handle exists until USB ancestry
    // has been established, unlike hidapi's generic Windows enumeration.
    unsafe fn is_usb(interface: *const u16) -> Result<bool, String> {
        let mut kind = 0;
        let mut id = [0u16; 512];
        let mut bytes = size_of_val(&id) as u32;
        if CM_Get_Device_Interface_PropertyW(
            interface,
            &DEVPKEY_Device_InstanceId,
            &mut kind,
            id.as_mut_ptr().cast(),
            &mut bytes,
            0,
        ) != CR_SUCCESS
            || kind != DEVPROP_TYPE_STRING
        {
            return Err("USB ancestry instance lookup incomplete".into());
        }
        let mut node = 0;
        if CM_Locate_DevNodeW(&mut node, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
            return Err("USB ancestry node lookup incomplete".into());
        }
        let mut ids = Vec::new();
        for _ in 0..32 {
            let mut count = 0;
            if CM_Get_Device_ID_Size(&mut count, node, 0) != CR_SUCCESS || count > 32768 {
                return Err("USB ancestry ID size unavailable".into());
            }
            let mut id = vec![0u16; count as usize + 1];
            if CM_Get_Device_IDW(node, id.as_mut_ptr(), id.len() as u32, 0) != CR_SUCCESS {
                return Err("USB ancestry ID unavailable".into());
            }
            let id = text(&id);
            let upper = id.to_ascii_uppercase();
            ids.push(id);
            if upper.starts_with("BTH")
                || upper.starts_with("BLUETOOTH\\")
                || upper.starts_with("USB\\VID_")
            {
                return Ok(windows_usb_ancestry(ids));
            }
            // Only an observed root terminates a non-USB ancestry normally.
            // A failed parent query on any other node is an incomplete scan.
            if upper.starts_with("HTREE\\ROOT\\") {
                return Ok(false);
            }
            let mut parent = 0;
            if CM_Get_Parent(&mut parent, node, 0) != CR_SUCCESS {
                return Err("USB ancestry parent lookup incomplete".into());
            }
            node = parent;
        }
        Err("USB ancestry depth exceeded".into())
    }
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    struct Preparsed(PHIDP_PREPARSED_DATA);
    impl Drop for Preparsed {
        fn drop(&mut self) {
            unsafe {
                HidD_FreePreparsedData(self.0);
            }
        }
    }
    unsafe fn string(
        handle: HANDLE,
        get: unsafe extern "system" fn(HANDLE, *mut std::ffi::c_void, u32) -> bool,
    ) -> Result<String, String> {
        let mut buffer = [0u16; 256];
        if get(
            handle,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
        ) {
            Ok(text(&buffer))
        } else {
            Err(format!(
                "USB identity string unavailable: {}",
                GetLastError()
            ))
        }
    }
    unsafe fn usb_info(path: &[u16]) -> Result<Option<Device>, String> {
        if !is_usb(path.as_ptr())? {
            return Ok(None);
        }
        // Metadata-only, shared handle; never send/read reports or firmware data.
        let handle = CreateFileW(
            path.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!("USB metadata open incomplete: {}", GetLastError()));
        }
        let handle = Handle(handle);
        let mut pp = 0;
        if !HidD_GetPreparsedData(handle.0, &mut pp) {
            return Err("USB capabilities unavailable".into());
        }
        let pp = Preparsed(pp);
        let mut caps: HIDP_CAPS = zeroed();
        if HidP_GetCaps(pp.0, &mut caps) != HIDP_STATUS_SUCCESS {
            return Err("USB capabilities incomplete".into());
        }
        if caps.UsagePage != 0xFF60 || caps.Usage != 0x61 {
            return Ok(None);
        }
        let mut attrs: HIDD_ATTRIBUTES = zeroed();
        attrs.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
        if !HidD_GetAttributes(handle.0, &mut attrs) {
            return Err("USB attributes unavailable".into());
        }
        let path = text(path);
        let product = string(handle.0, HidD_GetProductString)?;
        let mut d = Device {
            name: if product.is_empty() {
                "Unknown Keyboard".into()
            } else {
                product
            },
            vendor_id: attrs.VendorID,
            product_id: attrs.ProductID,
            manufacturer: string(handle.0, HidD_GetManufacturerString)?,
            serial_number: string(handle.0, HidD_GetSerialNumberString)?,
            bus_type: "Usb".into(),
            instance_token: device_instance_token(&path),
            path,
            firmware: FirmwareProtocol::Vial,
        };
        d.normalize_discovered_identity();
        Ok(Some(d))
    }
    pub(super) fn scan() -> Result<Vec<Device>, String> {
        unsafe {
            let mut guid = zeroed();
            HidD_GetHidGuid(&mut guid);
            for _ in 0..3 {
                let mut len = 0;
                let status = CM_Get_Device_Interface_List_SizeW(
                    &mut len,
                    &guid,
                    null(),
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                );
                if status != CR_SUCCESS || len > 16 * 1024 * 1024 {
                    return Err(format!("USB interface-list size failed: {status}"));
                }
                let mut list = vec![0u16; len.max(1) as usize];
                let status = CM_Get_Device_Interface_ListW(
                    &guid,
                    null(),
                    list.as_mut_ptr(),
                    list.len() as u32,
                    CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                );
                if status == CR_BUFFER_SMALL {
                    continue;
                }
                if status != CR_SUCCESS {
                    return Err(format!("USB interface-list read failed: {status}"));
                }
                let mut devices = Vec::new();
                for entry in list.split_inclusive(|c| *c == 0) {
                    if entry.len() <= 1 {
                        break;
                    }
                    if let Some(device) = usb_info(entry)? {
                        devices.push(device);
                    }
                }
                return Ok(devices);
            }
            Err("USB interface list changed repeatedly; retaining previous inventory".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usb_hotplug_windows_filters_bluetooth_before_any_hid_open() {
        let cases: &[(&[&str], bool)] = &[
            (
                &["HID\\VID_FEED&PID_0001", "USB\\VID_FEED&PID_0001&MI_01"],
                true,
            ),
            (
                &[
                    "HID\\VID_FEED&PID_0001",
                    "BTHLEDEVICE\\{fixture}",
                    "USB\\VID_8087&PID_0033",
                ],
                false,
            ),
            (
                &["HID\\fixture", "BTHENUM\\fixture", "USB\\VID_8087&PID_0033"],
                false,
            ),
            (&["HID\\fixture", "ACPI\\fixture"], false),
            (&[], false),
            (&["hid\\fixture", "usb\\vid_feed&pid_0001"], true),
        ];
        for (ids, expected) in cases {
            assert_eq!(
                windows_usb_ancestry(ids.iter().map(|s| s.to_string())),
                *expected
            );
        }
    }
}
