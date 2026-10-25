//! How a link reaches its board: finding Rynk boards on their buses, opening
//! one by its [`PhysicalId`], and following boards as they come and go.
//!
//! Finding a board opens nothing. Opening claims the board's interface, which
//! the OS gives to one handle at a time, so only [`RynkLink`] opens boards.
//!
//! [`RynkLink`]: super::link::RynkLink

use std::io;

use super::link::{BoxedTransport, PhysicalId};

#[cfg(target_os = "linux")]
pub(crate) mod ble_linux;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod hid_reports;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) mod usb;

/// A Rynk board seen on a bus, as the bus describes it; nothing about it has
/// been read from the board yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FoundBoard {
    pub(crate) id: PhysicalId,
    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,
    pub(crate) manufacturer: Option<String>,
    pub(crate) product: Option<String>,
    pub(crate) serial: Option<String>,
    pub(crate) bluetooth: bool,
}

/// A Bluetooth board the OS is not connected to: it is off or out of range,
/// and opening it would only page it in vain.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct NotConnected {
    pub(crate) board: String,
}

#[cfg(target_os = "linux")]
impl std::fmt::Display for NotConnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is not connected over Bluetooth", self.board)
    }
}

#[cfg(target_os = "linux")]
impl std::error::Error for NotConnected {}

/// A board arriving on or leaving a bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BusEvent {
    Arrived(PhysicalId),
    Left(PhysicalId),
}

/// The Rynk boards on every bus Entropy can reach.
pub(crate) fn scan() -> io::Result<Vec<FoundBoard>> {
    let boards = scan_usb()?;
    // Elsewhere a Bluetooth board shows through the OS HID stack instead.
    #[cfg(target_os = "linux")]
    let boards = match ble_linux::scan() {
        Ok(found) => boards.into_iter().chain(found).collect(),
        Err(error) => {
            log::warn!("Rynk Bluetooth scan failed: {error}");
            boards
        }
    };
    Ok(boards)
}

/// The Rynk boards on USB alone, without asking the Bluetooth stack.
pub(crate) fn scan_usb() -> io::Result<Vec<FoundBoard>> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        usb::scan()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Ok(Vec::new())
    }
}

/// Whether `id` names a board reached over Bluetooth.
pub(crate) fn is_bluetooth(id: &PhysicalId) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    if hid_reports::owns(id) {
        return true;
    }
    #[cfg(target_os = "linux")]
    if ble_linux::owns(id) {
        return true;
    }
    let _ = id;
    false
}

/// Whether the board `id` names can be opened without paging it: a bonded
/// Bluetooth board is listed while it is away, and opening it would connect.
pub(crate) fn connected(id: &PhysicalId) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    if ble_linux::owns(id) {
        return ble_linux::connected(id);
    }
    let _ = id;
    Ok(true)
}

/// Opens the board `id` names and claims its interface.
pub(crate) fn open(id: &PhysicalId) -> io::Result<BoxedTransport> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    if usb::owns(id) {
        return usb::open(id).map(|transport| Box::new(transport) as BoxedTransport);
    }
    #[cfg(target_os = "linux")]
    if ble_linux::owns(id) {
        return ble_linux::open(id).map(|transport| Box::new(transport) as BoxedTransport);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if hid_reports::owns(id) {
        return hid_reports::open(id).map(|transport| Box::new(transport) as BoxedTransport);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no transport reaches {}", id.0),
    ))
}

/// Reports the boards present now as arrivals, then every change, until
/// `on_event` returns false. Blocks the calling thread meanwhile. Only USB
/// boards come and go here so far.
pub(crate) fn watch(on_event: impl FnMut(BusEvent) -> bool) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        usb::watch(on_event)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = on_event;
        Err(io::ErrorKind::Unsupported.into())
    }
}
