//! Synchronous Rynk client over any byte transport.
//!
//! One request at a time, as device operations run today; the link owner
//! serializes its callers and adds the window for requests nobody waits on.
//! Requests are generic over `Endpoint`, so endpoint tables declared elsewhere
//! (the ergohaven vendor table, a private branch's) work without a change here.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use rmk_types_rynk::action::KeyAction;
use rmk_types_rynk::combo::Combo;
use rmk_types_rynk::keyboard_macros::{Macro, MacroOp};
use rmk_types_rynk::morse::Morse;
use rmk_types_rynk::protocol::rynk::command::{self, Endpoint};
use rmk_types_rynk::protocol::rynk::{
    encode_frame, max_wire_size, Cmd, Deframer, DeviceCapabilities, GetComboBulkRequest,
    GetKeymapBulkRequest, GetMorseBulkRequest, ProtocolVersion, RynkError, RynkHeader,
    SetMacroRequest, RYNK_HEADER_SIZE,
};
use serde::Serialize;

use super::read_only::{read_only_rynk_session, ReadOnlyRynkSession};

/// Payload budget before the board reports its own: enough for the handshake.
const HANDSHAKE_MAX_PAYLOAD: usize = 64;
/// Receive buffer: the largest frame a `u16` payload size allows, COBS-encoded.
const RX_BUFFER_SIZE: usize = max_wire_size(RYNK_HEADER_SIZE + u16::MAX as usize);

/// A byte link to one board: USB bulk endpoints, BLE GATT, or 32-byte HID
/// reports. The client only frames; the transport moves bytes.
pub(crate) trait RynkTransport {
    /// Sends one whole COBS frame, split as the medium needs.
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
    /// Bytes that arrived, waiting up to `timeout`; `Ok(0)` when none did.
    /// Zero padding after a frame, as HID reports carry, is fine.
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RynkTimeouts {
    /// Until the first byte of a reply arrives.
    pub(crate) reply: Duration,
    /// Between bytes of one reply. A 4 KB reply over 32-byte HID reports is a
    /// hundred reports or more, so the reply as a whole has no fixed limit;
    /// it only has to keep moving.
    pub(crate) idle: Duration,
    /// Hard limit on one request, however slowly its reply moves.
    pub(crate) total: Duration,
}

impl Default for RynkTimeouts {
    fn default() -> Self {
        Self {
            reply: Duration::from_secs(8),
            idle: Duration::from_millis(2500),
            total: Duration::from_secs(60),
        }
    }
}

#[derive(Debug)]
pub(crate) enum RynkClientError {
    Transport(io::Error),
    Timeout {
        cmd: u16,
    },
    /// The board answered with an error.
    Device {
        cmd: u16,
        error: RynkError,
    },
    /// The reply does not decode as the endpoint's response, or has bytes
    /// left over: the board speaks another revision.
    Malformed {
        cmd: u16,
    },
    CmdMismatch {
        sent: u16,
        got: u16,
    },
    /// The request does not fit the board's frame.
    TooLarge {
        cmd: u16,
        len: usize,
        max: usize,
    },
    VersionMismatch {
        board: ProtocolVersion,
        host: ProtocolVersion,
    },
    /// A bulk reply that cannot be right, such as an empty page short of the end.
    BulkProtocol {
        cmd: u16,
    },
    /// A read-only session refused a command that is not a known read.
    ReadOnly {
        cmd: u16,
    },
}

impl fmt::Display for RynkClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "Rynk transport error: {error}"),
            Self::Timeout { cmd } => write!(f, "Rynk command {cmd:#06x} timed out"),
            Self::Device { cmd, error } => write!(f, "Rynk command {cmd:#06x} failed: {error:?}"),
            Self::Malformed { cmd } => write!(f, "Rynk command {cmd:#06x}: malformed reply"),
            Self::CmdMismatch { sent, got } => {
                write!(f, "Rynk command {sent:#06x} answered as {got:#06x}")
            }
            Self::TooLarge { cmd, len, max } => {
                write!(
                    f,
                    "Rynk command {cmd:#06x}: {len} bytes exceed the board's {max}"
                )
            }
            Self::VersionMismatch { board, host } => write!(
                f,
                "Rynk protocol {}.{} on the board, {}.{} in Entropy",
                board.major, board.minor, host.major, host.minor
            ),
            Self::BulkProtocol { cmd } => write!(f, "Rynk command {cmd:#06x}: bad bulk page"),
            Self::ReadOnly { cmd } => {
                write!(f, "Read-only Rynk session refused command {cmd:#06x}")
            }
        }
    }
}

impl std::error::Error for RynkClientError {}

impl From<io::Error> for RynkClientError {
    fn from(error: io::Error) -> Self {
        Self::Transport(error)
    }
}

/// The frame for request `E` with `seq`, refused when its payload passes
/// `max_payload`.
pub(crate) fn encode_request<E: Endpoint>(
    seq: u8,
    request: &E::Request,
    max_payload: usize,
) -> Result<Vec<u8>, RynkClientError> {
    let cmd = E::CMD.raw();
    let len = postcard_len(request).ok_or(RynkClientError::TooLarge {
        cmd,
        len: usize::MAX,
        max: max_payload,
    })?;
    if len > max_payload {
        return Err(RynkClientError::TooLarge {
            cmd,
            len,
            max: max_payload,
        });
    }
    let mut frame = vec![0; max_wire_size(RYNK_HEADER_SIZE + len)];
    let header = RynkHeader { cmd: E::CMD, seq };
    let frame_len =
        encode_frame(&mut frame, header, request).map_err(|_| RynkClientError::TooLarge {
            cmd,
            len,
            max: max_payload,
        })?;
    frame.truncate(frame_len);
    Ok(frame)
}

/// The response in a decoded reply frame (header included) for request `E`.
pub(crate) fn decode_reply<E: Endpoint>(frame: &[u8]) -> Result<E::Response, RynkClientError> {
    let cmd = E::CMD.raw();
    let header = frame
        .first_chunk::<RYNK_HEADER_SIZE>()
        .map(RynkHeader::parse)
        .ok_or(RynkClientError::Malformed { cmd })?;
    if header.cmd != E::CMD {
        return Err(RynkClientError::CmdMismatch {
            sent: cmd,
            got: header.cmd.raw(),
        });
    }
    match postcard::take_from_bytes::<Result<E::Response, RynkError>>(&frame[RYNK_HEADER_SIZE..]) {
        Ok((reply, [])) => reply.map_err(|error| RynkClientError::Device { cmd, error }),
        _ => Err(RynkClientError::Malformed { cmd }),
    }
}

fn postcard_len<T: Serialize>(value: &T) -> Option<usize> {
    postcard::serialize_with_flavor(value, postcard::ser_flavors::Size::default()).ok()
}

pub(crate) struct RynkClient<T> {
    transport: T,
    rx: Vec<u8>,
    deframer: Deframer,
    next_seq: u8,
    max_payload: usize,
    timeouts: RynkTimeouts,
    read_only: Option<ReadOnlyRynkSession>,
}

impl<T: RynkTransport> RynkClient<T> {
    pub(crate) fn new(transport: T, timeouts: RynkTimeouts) -> Self {
        Self {
            transport,
            rx: vec![0; RX_BUFFER_SIZE],
            deframer: Deframer::new(),
            next_seq: 1,
            max_payload: HANDSHAKE_MAX_PAYLOAD,
            timeouts,
            read_only: read_only_rynk_session(),
        }
    }

    /// A client inside `session`, as `enforce_read_only_rynk` would make it.
    #[cfg(test)]
    pub(crate) fn test_read_only(
        transport: T,
        timeouts: RynkTimeouts,
        session: ReadOnlyRynkSession,
    ) -> Self {
        Self {
            read_only: Some(session),
            ..Self::new(transport, timeouts)
        }
    }

    /// Checks the protocol version and reads the board's capabilities, which
    /// size every later request.
    pub(crate) fn connect(
        transport: T,
        timeouts: RynkTimeouts,
    ) -> Result<(Self, DeviceCapabilities), RynkClientError> {
        let mut client = Self::new(transport, timeouts);
        let board = client.request::<command::GetVersion>(&())?;
        if board != ProtocolVersion::CURRENT {
            return Err(RynkClientError::VersionMismatch {
                board,
                host: ProtocolVersion::CURRENT,
            });
        }
        let capabilities = client.request::<command::GetCapabilities>(&())?;
        client.max_payload = capabilities.max_payload_size as usize;
        Ok((client, capabilities))
    }

    /// One request and its reply.
    pub(crate) fn request<E: Endpoint>(
        &mut self,
        request: &E::Request,
    ) -> Result<E::Response, RynkClientError> {
        let seq = self.send::<E>(request)?;
        let reply = self
            .wait_reply(E::CMD, seq)
            .and_then(|frame| decode_reply::<E>(&frame));
        if let (Err(_), Some(session)) = (&reply, &self.read_only) {
            session.record_failed_read(E::CMD);
        }
        reply
    }

    /// Sends a request without waiting; its reply is dropped when it comes.
    /// Returns the request's SEQ. Every frame leaves through here, so this is
    /// where a read-only session refuses what is not a read.
    pub(crate) fn send<E: Endpoint>(
        &mut self,
        request: &E::Request,
    ) -> Result<u8, RynkClientError> {
        if self
            .read_only
            .as_ref()
            .is_some_and(|session| !session.admit(E::CMD))
        {
            return Err(RynkClientError::ReadOnly { cmd: E::CMD.raw() });
        }
        let seq = self.next_seq;
        self.next_seq = if seq == u8::MAX { 1 } else { seq + 1 };
        let frame = encode_request::<E>(seq, request, self.max_payload)?;
        self.transport.send(&frame)?;
        Ok(seq)
    }

    fn wait_reply(&mut self, cmd: Cmd, seq: u8) -> Result<Vec<u8>, RynkClientError> {
        let start = Instant::now();
        let mut last_byte = None::<Instant>;
        loop {
            while let Some(len) = self.deframer.next(&mut self.rx) {
                let frame = &self.rx[..len];
                let header = RynkHeader::parse(frame.first_chunk().expect("frames hold a header"));
                // Nothing on this side listens to topics; any other reply
                // answers a request nobody waits for any more.
                if !header.cmd.is_topic() && header.seq == seq {
                    return Ok(frame.to_vec());
                }
            }
            let now = Instant::now();
            let deadline = match last_byte {
                Some(at) if self.deframer.has_pending() => at + self.timeouts.idle,
                _ => start + self.timeouts.reply,
            }
            .min(start + self.timeouts.total);
            let Some(wait) = deadline
                .checked_duration_since(now)
                .filter(|wait| !wait.is_zero())
            else {
                return Err(RynkClientError::Timeout { cmd: cmd.raw() });
            };
            let n = self
                .transport
                .recv(self.deframer.tail(&mut self.rx), wait)?;
            if n > 0 {
                self.deframer.commit(n);
                last_byte = Some(Instant::now());
            }
        }
    }

    /// Every key of `layer`, row by row, in as few pages as the board sends.
    pub(crate) fn read_keymap_layer(
        &mut self,
        layer: u8,
        rows: u8,
        cols: u8,
    ) -> Result<Vec<KeyAction>, RynkClientError> {
        let total = rows as usize * cols as usize;
        let mut actions = Vec::with_capacity(total);
        while actions.len() < total {
            let at = actions.len();
            let request = GetKeymapBulkRequest {
                layer,
                start_row: (at / cols as usize) as u8,
                start_col: (at % cols as usize) as u8,
            };
            let page = self.request::<command::GetKeymapBulk>(&request)?.actions;
            if page.is_empty() || at + page.len() > total {
                return Err(RynkClientError::BulkProtocol {
                    cmd: command::GetKeymapBulk::CMD.raw(),
                });
            }
            actions.extend(page);
        }
        Ok(actions)
    }

    pub(crate) fn read_combos(&mut self, count: u8) -> Result<Vec<Combo>, RynkClientError> {
        self.read_bulk::<command::GetComboBulk, _>(
            count,
            |start_index| GetComboBulkRequest { start_index },
            |page| page.configs,
        )
    }

    pub(crate) fn read_morses(&mut self, count: u8) -> Result<Vec<Morse>, RynkClientError> {
        self.read_bulk::<command::GetMorseBulk, _>(
            count,
            |start_index| GetMorseBulkRequest { start_index },
            |page| page.configs,
        )
    }

    fn read_bulk<E: Endpoint, I>(
        &mut self,
        count: u8,
        request: impl Fn(u8) -> E::Request,
        items: impl Fn(E::Response) -> Vec<I>,
    ) -> Result<Vec<I>, RynkClientError> {
        let mut all = Vec::with_capacity(count as usize);
        while all.len() < count as usize {
            let page = items(self.request::<E>(&request(all.len() as u8))?);
            if page.is_empty() || all.len() + page.len() > count as usize {
                return Err(RynkClientError::BulkProtocol { cmd: E::CMD.raw() });
            }
            all.extend(page);
        }
        Ok(all)
    }

    pub(crate) fn read_macro(&mut self, index: u8) -> Result<Vec<MacroOp>, RynkClientError> {
        Ok(self.request::<command::GetMacro>(&index)?.ops().collect())
    }

    pub(crate) fn write_macro(
        &mut self,
        index: u8,
        ops: &[MacroOp],
    ) -> Result<(), RynkClientError> {
        let cmd = command::SetMacro::CMD.raw();
        let macro_ops = Macro::from_slice(ops).map_err(|_| RynkClientError::TooLarge {
            cmd,
            len: super::macros::macro_slot_size(ops),
            max: self.max_payload,
        })?;
        self.request::<command::SetMacro>(&SetMacroRequest { index, macro_ops })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use rmk_types_rynk::action::Action;
    use rmk_types_rynk::protocol::rynk::GetKeymapBulkResponse;

    type Handler = Box<dyn FnMut(RynkHeader, &[u8]) -> Vec<Vec<u8>>>;

    /// A board in a box: decodes each frame the client sends and queues the
    /// frames its handler answers with, cut into `chunk`-byte reads.
    struct FakeBoard {
        handler: Handler,
        pending: VecDeque<u8>,
        chunk: usize,
        sent: Vec<RynkHeader>,
    }

    impl FakeBoard {
        fn new(handler: impl FnMut(RynkHeader, &[u8]) -> Vec<Vec<u8>> + 'static) -> Self {
            Self {
                handler: Box::new(handler),
                pending: VecDeque::new(),
                chunk: usize::MAX,
                sent: Vec::new(),
            }
        }

        /// Replies in 32-byte HID reports, zero-padded like `RynkHidService`.
        fn hid_reports(mut self) -> Self {
            self.chunk = 32;
            self
        }
    }

    impl RynkTransport for &mut FakeBoard {
        fn send(&mut self, frame: &[u8]) -> io::Result<()> {
            let mut buf = frame.to_vec();
            let mut deframer = Deframer::new();
            let tail = deframer.tail(&mut buf);
            let n = tail.len();
            deframer.commit(n);
            let len = deframer.next(&mut buf).expect("one whole frame");
            let header = RynkHeader::parse(buf.first_chunk().unwrap());
            self.sent.push(header);
            for mut reply in (self.handler)(header, &buf[RYNK_HEADER_SIZE..len]) {
                if self.chunk != usize::MAX {
                    reply.resize(reply.len().div_ceil(self.chunk) * self.chunk, 0);
                }
                self.pending.extend(reply);
            }
            Ok(())
        }

        fn recv(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<usize> {
            let n = buf.len().min(self.chunk).min(self.pending.len());
            for (slot, byte) in buf.iter_mut().zip(self.pending.drain(..n)) {
                *slot = byte;
            }
            Ok(n)
        }
    }

    fn frame<T: Serialize>(cmd: Cmd, seq: u8, value: &T) -> Vec<u8> {
        let mut buf = vec![0; 8192];
        let len = encode_frame(&mut buf, RynkHeader { cmd, seq }, value).unwrap();
        buf.truncate(len);
        buf
    }

    fn ok<T: Serialize>(header: RynkHeader, value: T) -> Vec<u8> {
        frame(header.cmd, header.seq, &Ok::<T, RynkError>(value))
    }

    fn err(header: RynkHeader, error: RynkError) -> Vec<u8> {
        frame(header.cmd, header.seq, &Err::<(), RynkError>(error))
    }

    fn quick() -> RynkTimeouts {
        RynkTimeouts {
            reply: Duration::from_millis(30),
            idle: Duration::from_millis(30),
            total: Duration::from_millis(100),
        }
    }

    fn capabilities(max_payload_size: u16) -> DeviceCapabilities {
        DeviceCapabilities {
            max_payload_size,
            num_layers: 2,
            num_rows: 3,
            num_cols: 5,
            ..Default::default()
        }
    }

    fn handshake(header: RynkHeader, max_payload_size: u16) -> Option<Vec<u8>> {
        match header.cmd {
            Cmd::GetVersion => Some(ok(header, ProtocolVersion::CURRENT)),
            Cmd::GetCapabilities => Some(ok(header, capabilities(max_payload_size))),
            _ => None,
        }
    }

    #[test]
    fn connect_checks_the_version_and_takes_the_payload_size() {
        let mut board = FakeBoard::new(|header, _| vec![handshake(header, 4000).unwrap()]);
        let (client, capabilities) = RynkClient::connect(&mut board, quick()).unwrap();
        assert_eq!(client.max_payload, 4000);
        assert_eq!(capabilities.num_cols, 5);

        let mut old =
            FakeBoard::new(|header, _| vec![ok(header, ProtocolVersion { major: 0, minor: 1 })]);
        assert!(matches!(
            RynkClient::connect(&mut old, quick()),
            Err(RynkClientError::VersionMismatch {
                board: ProtocolVersion { major: 0, minor: 1 },
                ..
            })
        ));
    }

    #[test]
    fn replies_match_by_seq_through_reports_topics_and_stale_frames() {
        let mut board = FakeBoard::new(|header, _| {
            if let Some(reply) = handshake(header, 512) {
                return vec![reply];
            }
            let stale = frame(
                header.cmd,
                header.seq.wrapping_sub(1),
                &Ok::<u8, RynkError>(9),
            );
            vec![frame(Cmd::LayerChange, 0, &3u8), stale, ok(header, 2u8)]
        })
        .hid_reports();
        let (mut client, _) = RynkClient::connect(&mut board, quick()).unwrap();
        assert_eq!(client.request::<command::GetCurrentLayer>(&()).unwrap(), 2);
    }

    #[test]
    fn board_errors_and_bad_replies_are_told_apart() {
        let mut board = FakeBoard::new(|header, _| {
            let reply = match header.cmd.raw() {
                0x0801 => err(header, RynkError::UnknownCmd),
                0x0802 => err(header, RynkError::Locked),
                // A u8 reply with a byte too many.
                0x0805 => frame(header.cmd, header.seq, &Ok::<(u16, u8), RynkError>((7, 1))),
                // Answered under another command.
                0x0806 => frame(Cmd::GetWpm, header.seq, &Ok::<u16, RynkError>(7)),
                _ => return vec![],
            };
            vec![reply]
        });
        let mut client = RynkClient::new(&mut board, quick());
        assert!(matches!(
            client.request::<command::GetCurrentLayer>(&()),
            Err(RynkClientError::Device {
                error: RynkError::UnknownCmd,
                ..
            })
        ));
        assert!(matches!(
            client.request::<command::GetMatrixState>(&()),
            Err(RynkClientError::Device {
                error: RynkError::Locked,
                ..
            })
        ));
        assert!(matches!(
            client.request::<command::GetWpm>(&()),
            Err(RynkClientError::Malformed { cmd: 0x0805 })
        ));
        assert!(matches!(
            client.request::<command::GetSleepState>(&()),
            Err(RynkClientError::CmdMismatch {
                sent: 0x0806,
                got: 0x0805
            })
        ));
        assert!(matches!(
            client.request::<command::GetLedIndicator>(&()),
            Err(RynkClientError::Timeout { cmd: 0x0807 })
        ));
    }

    #[test]
    fn requests_past_the_board_frame_are_refused_before_sending() {
        let mut board = FakeBoard::new(|header, _| vec![handshake(header, 64).unwrap()]);
        let (mut client, _) = RynkClient::connect(&mut board, quick()).unwrap();
        let ops = vec![MacroOp::Char(b'a'); 40];
        assert!(matches!(
            client.write_macro(0, &ops),
            Err(RynkClientError::TooLarge {
                cmd: 0x0202,
                len: 82,
                max: 64
            })
        ));
        drop(client);
        assert_eq!(board.sent.len(), 2, "only the handshake went out");
    }

    /// Endpoints declared outside the client, as the ergohaven vendor table and
    /// a private branch declare theirs.
    #[test]
    fn endpoints_from_other_tables_go_through_request() {
        enum SetAgents {}
        impl Endpoint for SetAgents {
            const CMD: Cmd = Cmd::from_raw(0x7EF0);
            type Request = [u8; 3];
            type Response = ();
        }
        let mut board = FakeBoard::new(|header, payload| {
            assert_eq!(header.cmd.raw(), 0x7EF0);
            assert_eq!(payload, [1, 2, 3]);
            vec![ok(header, ())]
        });
        let mut client = RynkClient::new(&mut board, quick());
        client.request::<SetAgents>(&[1, 2, 3]).unwrap();
    }

    #[test]
    fn keymap_pages_are_read_in_order() {
        let keys: Vec<KeyAction> = (0..15u8)
            .map(|i| KeyAction::Single(Action::User(i)))
            .collect();
        let stored = keys.clone();
        let mut board = FakeBoard::new(move |header, payload| {
            if let Some(reply) = handshake(header, 40) {
                return vec![reply];
            }
            match header.cmd {
                Cmd::GetKeymapBulk => {
                    let request: GetKeymapBulkRequest = postcard::from_bytes(payload).unwrap();
                    let at = request.start_row as usize * 5 + request.start_col as usize;
                    let actions = stored[at..stored.len().min(at + 4)].to_vec();
                    vec![ok(header, GetKeymapBulkResponse { actions })]
                }
                _ => vec![],
            }
        });
        let (mut client, _) = RynkClient::connect(&mut board, quick()).unwrap();
        assert_eq!(client.read_keymap_layer(1, 3, 5).unwrap(), keys);
    }

    #[test]
    fn read_only_sessions_send_reads_and_refuse_everything_else() {
        let mut board = FakeBoard::new(|header, _| {
            vec![match header.cmd {
                Cmd::GetVersion => ok(header, ProtocolVersion::CURRENT),
                Cmd::GetCapabilities => ok(header, capabilities(512)),
                Cmd::GetKeyAction => ok(header, KeyAction::No),
                Cmd::GetMacro => ok(header, Macro::new()),
                _ => err(header, RynkError::Locked),
            }]
        });
        let session = ReadOnlyRynkSession::default();
        let mut client = RynkClient::test_read_only(&mut board, quick(), session.clone());
        client.request::<command::GetVersion>(&()).unwrap();
        client.max_payload = client
            .request::<command::GetCapabilities>(&())
            .unwrap()
            .max_payload_size as usize;
        let position = rmk_types_rynk::protocol::rynk::KeyPosition {
            layer: 0,
            row: 0,
            col: 0,
        };
        assert_eq!(
            client.request::<command::GetKeyAction>(&position).unwrap(),
            KeyAction::No
        );
        assert_eq!(client.read_macro(0).unwrap(), []);
        assert!(matches!(
            client.request::<command::GetMatrixState>(&()),
            Err(RynkClientError::Device {
                error: RynkError::Locked,
                ..
            })
        ));

        enum SetAgents {}
        impl Endpoint for SetAgents {
            const CMD: Cmd = Cmd::from_raw(0x7EF0);
            type Request = ();
            type Response = ();
        }
        let refused = [
            client.request::<command::SetKeyAction>(
                &rmk_types_rynk::protocol::rynk::SetKeyRequest {
                    position,
                    action: KeyAction::No,
                },
            ),
            client.write_macro(0, &[]),
            client.request::<command::StorageReset>(
                &rmk_types_rynk::protocol::rynk::StorageResetMode::Full,
            ),
            client.request::<command::Reboot>(&()),
            client.request::<command::BootloaderJump>(&()),
            client.request::<command::Lock>(&()),
            client.request::<command::UnlockPoll>(&()).map(|_| ()),
            client.request::<command::SwitchBleProfile>(&1),
            client.request::<SetAgents>(&()),
            client.send::<command::SetDefaultLayer>(&1).map(|_| ()),
        ];
        for result in refused {
            assert!(
                matches!(result, Err(RynkClientError::ReadOnly { .. })),
                "{result:?}"
            );
        }
        drop(client);
        let sent: Vec<u16> = board.sent.iter().map(|header| header.cmd.raw()).collect();
        assert_eq!(sent, [0x0001, 0x0002, 0x0101, 0x0201, 0x0802]);
        assert_eq!(
            session.refused_requests(),
            [0x0102, 0x0202, 0x0005, 0x0003, 0x0004, 0x0008, 0x0007, 0x0704, 0x7EF0, 0x0104]
        );
        assert_eq!(session.failed_reads(), [0x0802]);
    }
}
