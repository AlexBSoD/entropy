//! Rynk over USB: the vendor bulk interface (class FF/52/52) that the firmware
//! adds beside its HID interfaces, through nusb's blocking API.
//!
//! Boards are recognized by that interface alone, so no VID or PID list is
//! kept here. Listing reads what the OS already knows and opens nothing.

use std::collections::HashMap;
use std::io::{self, Read};
use std::time::Duration;

use nusb::descriptors::TransferType;
use nusb::hotplug::HotplugEvent;
use nusb::io::EndpointRead;
use nusb::transfer::{Bulk, Direction, In, Out, TransferError};
use nusb::{DeviceId, DeviceInfo, Endpoint, MaybeFuture};
use rmk_types_rynk::protocol::rynk::{
    RYNK_USB_INTERFACE_CLASS, RYNK_USB_INTERFACE_PROTOCOL, RYNK_USB_INTERFACE_SUBCLASS,
};

use super::super::client::RynkTransport;
use super::super::link::PhysicalId;
use super::{BusEvent, FoundBoard};

const PREFIX: &str = "usb:";
/// Covers a whole frame per transfer and is a multiple of both full- and
/// high-speed bulk packets.
const READ_TRANSFER_SIZE: usize = 4096;
/// How long one frame may take to leave; the board stops taking bytes only
/// while it is busy, never for longer than a request may take.
const WRITE_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a board that just arrived may take to show its interfaces.
const SETTLE: Duration = Duration::from_secs(2);
const SETTLE_STEP: Duration = Duration::from_millis(100);

/// One bus device as scanning sees it, apart from nusb's types so that tests
/// can describe devices.
#[derive(Clone, Debug)]
struct Seen {
    bus: String,
    ports: Vec<u8>,
    address: u8,
    vendor_id: u16,
    product_id: u16,
    manufacturer: Option<String>,
    product: Option<String>,
    serial: Option<String>,
    /// Number, class, subclass and protocol of each interface.
    interfaces: Vec<[u8; 4]>,
}

impl Seen {
    fn of(info: &DeviceInfo) -> Self {
        Self {
            bus: info.bus_id().to_owned(),
            ports: info.port_chain().to_vec(),
            address: info.device_address(),
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            manufacturer: info.manufacturer_string().map(str::to_owned),
            product: info.product_string().map(str::to_owned),
            serial: info.serial_number().map(str::to_owned),
            interfaces: info
                .interfaces()
                .map(|interface| {
                    [
                        interface.interface_number(),
                        interface.class(),
                        interface.subclass(),
                        interface.protocol(),
                    ]
                })
                .collect(),
        }
    }

    /// The number of the Rynk interface, if the device has one.
    fn rynk_interface(&self) -> Option<u8> {
        self.interfaces
            .iter()
            .find(|[_, class, subclass, protocol]| {
                *class == RYNK_USB_INTERFACE_CLASS
                    && *subclass == RYNK_USB_INTERFACE_SUBCLASS
                    && *protocol == RYNK_USB_INTERFACE_PROTOCOL
            })
            .map(|[number, ..]| *number)
    }

    /// The port path survives the board's reboots, unlike its address, so a
    /// board that restarts comes back under the same id.
    fn id(&self) -> PhysicalId {
        if self.ports.is_empty() {
            return PhysicalId(format!("{PREFIX}{}@{}", self.bus, self.address));
        }
        let ports: Vec<String> = self.ports.iter().map(u8::to_string).collect();
        PhysicalId(format!("{PREFIX}{}-{}", self.bus, ports.join(".")))
    }

    fn found(&self) -> Option<FoundBoard> {
        self.rynk_interface()?;
        Some(FoundBoard {
            id: self.id(),
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            manufacturer: self.manufacturer.clone(),
            product: self.product.clone(),
            serial: self.serial.clone(),
            bluetooth: false,
        })
    }
}

/// nusb reports a claimed interface as `Other`; the link retries only what is
/// `ResourceBusy`.
fn usb_error(error: nusb::Error) -> io::Error {
    match error.kind() {
        nusb::ErrorKind::Busy => io::Error::new(io::ErrorKind::ResourceBusy, error),
        _ => error.into(),
    }
}

fn devices() -> io::Result<impl Iterator<Item = DeviceInfo>> {
    nusb::list_devices().wait().map_err(usb_error)
}

pub(crate) fn owns(id: &PhysicalId) -> bool {
    id.0.starts_with(PREFIX)
}

pub(crate) fn scan() -> io::Result<Vec<FoundBoard>> {
    Ok(devices()?
        .filter_map(|info| Seen::of(&info).found())
        .collect())
}

pub(crate) fn open(id: &PhysicalId) -> io::Result<UsbTransport> {
    for info in devices()? {
        let seen = Seen::of(&info);
        if seen.id() != *id {
            continue;
        }
        if let Some(interface) = seen.rynk_interface() {
            return UsbTransport::open(&info, interface);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} is not on USB", id.0),
    ))
}

/// The device as it stands once its interfaces show. Linux announces a device
/// before the kernel has configured it, so a fresh arrival may list none yet.
fn settled(info: DeviceInfo) -> DeviceInfo {
    let device = info.id();
    let mut latest = info;
    let mut waited = Duration::ZERO;
    while latest.interfaces().next().is_none() && waited < SETTLE {
        std::thread::sleep(SETTLE_STEP);
        waited += SETTLE_STEP;
        let Ok(mut listed) = devices() else {
            continue;
        };
        match listed.find(|info| info.id() == device) {
            Some(info) => latest = info,
            // Gone already; its removal follows.
            None => break,
        }
    }
    latest
}

pub(crate) fn watch(mut on_event: impl FnMut(BusEvent) -> bool) -> io::Result<()> {
    // Watching starts before listing, so a board arriving in between is
    // reported by one or the other; one reported by both counts once.
    let events = nusb::watch_devices().map_err(usb_error)?;
    let mut present = HashMap::<DeviceId, PhysicalId>::new();
    for info in devices()? {
        if !arrived(&info, &mut present, &mut on_event) {
            return Ok(());
        }
    }
    for event in futures_lite::stream::block_on(events) {
        let more = match event {
            HotplugEvent::Connected(info) => arrived(&settled(info), &mut present, &mut on_event),
            HotplugEvent::Disconnected(device) => match present.remove(&device) {
                Some(id) => on_event(BusEvent::Left(id)),
                None => true,
            },
        };
        if !more {
            break;
        }
    }
    Ok(())
}

/// Reports `info` if it is a Rynk board not reported yet; false once
/// `on_event` wants no more.
fn arrived(
    info: &DeviceInfo,
    present: &mut HashMap<DeviceId, PhysicalId>,
    on_event: &mut impl FnMut(BusEvent) -> bool,
) -> bool {
    let Some(board) = Seen::of(info).found() else {
        return true;
    };
    if present.insert(info.id(), board.id.clone()).is_some() {
        return true;
    }
    on_event(BusEvent::Arrived(board.id))
}

/// A claimed Rynk interface. Dropping it releases the claim.
pub(crate) struct UsbTransport {
    reader: EndpointRead<Bulk>,
    writer: Endpoint<Bulk, Out>,
}

impl UsbTransport {
    fn open(info: &DeviceInfo, number: u8) -> io::Result<Self> {
        let device = info.open().wait().map_err(usb_error)?;
        let interface = device.claim_interface(number).wait().map_err(usb_error)?;
        let (mut input, mut output) = (None, None);
        for endpoint in interface
            .descriptor()
            .into_iter()
            .flat_map(|descriptor| descriptor.endpoints().collect::<Vec<_>>())
        {
            if endpoint.transfer_type() != TransferType::Bulk {
                continue;
            }
            match endpoint.direction() {
                Direction::In => input = input.or(Some(endpoint.address())),
                Direction::Out => output = output.or(Some(endpoint.address())),
            }
        }
        let (Some(input), Some(output)) = (input, output) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the Rynk interface has no bulk endpoint pair",
            ));
        };
        let reader = interface.endpoint::<Bulk, In>(input).map_err(usb_error)?;
        let writer = interface.endpoint::<Bulk, Out>(output).map_err(usb_error)?;
        Ok(Self {
            reader: EndpointRead::new(reader, READ_TRANSFER_SIZE),
            writer,
        })
    }
}

impl RynkTransport for UsbTransport {
    /// One bulk transfer per frame. The board reads a byte stream, so a frame
    /// that fills its last packet needs no zero-length packet after it.
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        let mut buffer = self.writer.allocate(frame.len());
        buffer.extend_from_slice(frame);
        match self.writer.transfer_blocking(buffer, WRITE_TIMEOUT).status {
            Ok(()) => Ok(()),
            Err(TransferError::Cancelled) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the board took no frame",
            )),
            Err(error) => Err(error.into()),
        }
    }

    /// A read that times out leaves its transfer queued, so bytes arriving
    /// later are kept for the next call.
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.reader.set_read_timeout(timeout);
        match self.reader.read(buf) {
            Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(0),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HID: u8 = 0x03;

    fn seen(bus: &str, ports: &[u8], address: u8, interfaces: &[[u8; 4]]) -> Seen {
        Seen {
            bus: bus.to_owned(),
            ports: ports.to_vec(),
            address,
            vendor_id: 0xE126,
            product_id: 0x0004,
            manufacturer: Some("Ergohaven".to_owned()),
            product: Some("K:04".to_owned()),
            serial: Some("rynk:6a1f20".to_owned()),
            interfaces: interfaces.to_vec(),
        }
    }

    /// Keyboard and mouse HID, then the Rynk interface, as a Rynk K:04 has.
    fn rynk_k04(ports: &[u8], address: u8) -> Seen {
        seen(
            "3",
            ports,
            address,
            &[[0, HID, 1, 1], [1, HID, 0, 0], [2, 0xFF, 0x52, 0x52]],
        )
    }

    /// A Vial board: its configurator channel is a HID usage page, not an
    /// interface class.
    fn vial_k04() -> Seen {
        let mut board = seen("3", &[2], 7, &[[0, HID, 1, 1], [1, HID, 0, 0]]);
        board.serial = Some("vial:f64c2b3c:6a1f20".to_owned());
        board
    }

    #[test]
    fn a_board_is_recognized_by_its_rynk_interface() {
        assert_eq!(rynk_k04(&[5], 9).rynk_interface(), Some(2));
        assert_eq!(vial_k04().rynk_interface(), None);
        // Other vendor interfaces are not Rynk, whatever the VID says.
        let near_misses = seen(
            "3",
            &[1],
            4,
            &[
                [0, 0xFF, 0x00, 0x00],
                [1, 0xFF, 0x52, 0x00],
                [2, 0xFF, 0x00, 0x52],
            ],
        );
        assert_eq!(near_misses.rynk_interface(), None);
    }

    #[test]
    fn a_mixed_set_of_boards_yields_only_the_rynk_ones() {
        let devices = [
            vial_k04(),
            rynk_k04(&[5], 9),
            seen("3", &[1], 4, &[[0, 0xFF, 0x00, 0x00]]),
            rynk_k04(&[5, 2], 11),
        ];
        let found: Vec<_> = devices.iter().filter_map(Seen::found).collect();
        let ids: Vec<_> = found.iter().map(|board| board.id.0.as_str()).collect();
        assert_eq!(ids, ["usb:3-5", "usb:3-5.2"]);
        assert_eq!(found[0].product.as_deref(), Some("K:04"));
        assert_eq!(found[0].serial.as_deref(), Some("rynk:6a1f20"));
        assert!(found.iter().all(|board| owns(&board.id)));
    }

    #[test]
    fn a_board_keeps_its_id_when_it_restarts_on_the_same_port() {
        assert_eq!(rynk_k04(&[5, 2], 11).id(), rynk_k04(&[5, 2], 12).id());
        assert_ne!(rynk_k04(&[5, 2], 11).id(), rynk_k04(&[5, 3], 11).id());
        assert_ne!(seen("3", &[5], 11, &[]).id(), seen("4", &[5], 11, &[]).id());
        assert_eq!(seen("3", &[], 11, &[]).id().0, "usb:3@11");
    }
}
