//! The personal Rynk commands of a Qube, `0x7EF0..=0x7EFF`: the agent window,
//! the Claude Code limit bars and the halves' diagnostics. No release firmware
//! serves them; a board that answers `GetSideInfo` with `UnknownCmd` has none.
//!
//! postcard carries no field names, so these types copy the firmware's side
//! table field for field and in its order. `side.snap` is that table's wire
//! snapshot, and the tests hold this copy to it.

use rmk_types_rynk::protocol::rynk::command::Endpoint;
use rmk_types_rynk::protocol::rynk::Cmd;
use serde::{Deserialize, Serialize};

use super::battery_log;
use super::herdr::AgentCounts;
use super::usage::Usage;

/// The side table wire this copy speaks; the firmware bumps it on any change.
pub(crate) const SIDE_REVISION: u16 = 1;

/// [`SideInfo::caps`] bits.
pub(crate) mod caps {
    /// `SetAgents` is served.
    pub(crate) const AGENTS: u32 = 1 << 0;
    /// `SetUsage` is served.
    pub(crate) const USAGE: u32 = 1 << 1;
    /// `GetHalvesDiag` reports the halves.
    pub(crate) const HALVES_DIAG: u32 = 1 << 2;
}

/// What `GetSideInfo` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SideInfo {
    pub(crate) revision: u16,
    pub(crate) caps: u32,
}

impl SideInfo {
    /// Whether the board serves `cap` over the wire this copy speaks.
    pub(crate) fn serves(&self, cap: u32) -> bool {
        self.revision == SIDE_REVISION && self.caps & cap != 0
    }
}

/// Agent counts by state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AgentSummary {
    pub(crate) working: u8,
    pub(crate) idle: u8,
    pub(crate) blocked: u8,
    pub(crate) done: u8,
    pub(crate) unknown: u8,
}

impl From<AgentCounts> for AgentSummary {
    fn from(counts: AgentCounts) -> Self {
        Self {
            working: counts.working,
            idle: counts.idle,
            blocked: counts.blocked,
            done: counts.done,
            unknown: counts.unknown,
        }
    }
}

/// Rate-limit use in percent; `None` for a window we could not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UsageSummary {
    pub(crate) five_hour: Option<u8>,
    pub(crate) seven_day: Option<u8>,
    pub(crate) stale: bool,
}

impl From<Usage> for UsageSummary {
    fn from(usage: Usage) -> Self {
        Self {
            five_hour: usage.five_hour,
            seven_day: usage.seven_day,
            stale: usage.stale,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SleepStats {
    pub(crate) wakes: u16,
    pub(crate) awake_min: u16,
    pub(crate) uptime_min: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PointingStats {
    pub(crate) wakes: u32,
    pub(crate) reads: u32,
}

/// A half's power diagnostics. The order differs from the log's model:
/// `version` leads and `disconnects` comes before `conn_interval_us`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PowerDiag {
    pub(crate) version: u8,
    pub(crate) flags: u8,
    pub(crate) battery_mv: u16,
    pub(crate) reset_reason: u32,
    pub(crate) boot_count: u16,
    pub(crate) uptime_ms: u32,
    pub(crate) cpu_active_ms: u32,
    pub(crate) key_presses: u32,
    pub(crate) rssi: i8,
    pub(crate) last_disconnect_reason: u8,
    pub(crate) disconnects: u16,
    pub(crate) conn_interval_us: u32,
    pub(crate) peripheral_latency: u16,
    pub(crate) supervision_timeout_ms: u16,
    pub(crate) param_updates: u16,
    pub(crate) split_tx: u32,
    pub(crate) split_rx: u32,
}

/// One half's diagnostics; `None` where the half has not reported, and for
/// the central.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HalfDiag {
    pub(crate) sleep: Option<SleepStats>,
    pub(crate) pointing: Option<PointingStats>,
    pub(crate) power: Option<PowerDiag>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HalvesDiag {
    pub(crate) left: HalfDiag,
    pub(crate) right: HalfDiag,
}

impl From<HalfDiag> for battery_log::HalfDiag {
    fn from(diag: HalfDiag) -> Self {
        Self {
            sleep: diag.sleep.map(|sleep| battery_log::SleepStats {
                wakes: sleep.wakes,
                awake_min: sleep.awake_min,
                uptime_min: sleep.uptime_min,
            }),
            pointing: diag.pointing.map(|pointing| battery_log::PointingStats {
                wakes: pointing.wakes,
                reads: pointing.reads,
            }),
            power: diag.power.map(Into::into),
        }
    }
}

/// `version` names the half's record format, which [`SIDE_REVISION`] already
/// pins, so the log leaves it out.
impl From<PowerDiag> for battery_log::PowerDiag {
    fn from(power: PowerDiag) -> Self {
        Self {
            flags: power.flags,
            battery_mv: power.battery_mv,
            reset_reason: power.reset_reason,
            boot_count: power.boot_count,
            uptime_ms: power.uptime_ms,
            cpu_active_ms: power.cpu_active_ms,
            key_presses: power.key_presses,
            rssi: power.rssi,
            last_disconnect_reason: power.last_disconnect_reason,
            conn_interval_us: power.conn_interval_us,
            peripheral_latency: power.peripheral_latency,
            supervision_timeout_ms: power.supervision_timeout_ms,
            param_updates: power.param_updates,
            disconnects: power.disconnects,
            split_tx: power.split_tx,
            split_rx: power.split_rx,
        }
    }
}

/// The firmware's `endpoints!` stays private to rmk-types, so the markers are
/// spelled out here with the same rows.
macro_rules! endpoints {
    ($($name:ident = $cmd:literal: $req:ty => $resp:ty;)*) => {$(
        pub(crate) enum $name {}

        impl Endpoint for $name {
            const CMD: Cmd = Cmd::from_raw($cmd);
            type Request = $req;
            type Response = $resp;
        }
    )*};
}

endpoints! {
    SetAgents = 0x7EF0: AgentSummary => ();
    SetUsage = 0x7EF1: UsageSummary => ();
    GetHalvesDiag = 0x7EF8: () => HalvesDiag;
    GetSideInfo = 0x7EFF: () => SideInfo;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use rmk_types_rynk::protocol::rynk::{Deframer, RynkHeader};

    use super::*;
    use crate::rynk::client::{decode_reply, encode_request, testing};

    /// The firmware side table's snapshot: payload exemplars and SEQ 1 frames.
    const SNAPSHOT: &str = include_str!("side.snap");

    fn snapshot() -> BTreeMap<&'static str, Vec<u8>> {
        SNAPSHOT
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(|line| {
                let (label, bytes) = line.split_once("  ").expect("label, then bytes");
                let bytes = bytes
                    .split_whitespace()
                    .map(|byte| u8::from_str_radix(byte, 16).expect("hex byte"))
                    .collect();
                (label, bytes)
            })
            .collect()
    }

    // The exemplars of the firmware's snapshot test.

    fn agents() -> AgentSummary {
        AgentCounts {
            working: 2,
            idle: 3,
            blocked: 1,
            done: 4,
            unknown: 0,
        }
        .into()
    }

    fn usage() -> UsageSummary {
        Usage {
            five_hour: Some(42),
            seven_day: None,
            stale: false,
        }
        .into()
    }

    fn info() -> SideInfo {
        SideInfo {
            revision: SIDE_REVISION,
            caps: caps::AGENTS | caps::USAGE | caps::HALVES_DIAG,
        }
    }

    fn power() -> PowerDiag {
        PowerDiag {
            version: 1,
            flags: 0x21,
            battery_mv: 3900,
            reset_reason: 0x0001_0004,
            boot_count: 3,
            uptime_ms: 600_000,
            cpu_active_ms: 12_000,
            key_presses: 512,
            rssi: -60,
            last_disconnect_reason: 0x08,
            disconnects: 2,
            conn_interval_us: 7_500,
            peripheral_latency: 0,
            supervision_timeout_ms: 4_000,
            param_updates: 5,
            split_tx: 100_000,
            split_rx: 90_000,
        }
    }

    fn halves() -> HalvesDiag {
        HalvesDiag {
            left: HalfDiag {
                sleep: Some(SleepStats {
                    wakes: 7,
                    awake_min: 30,
                    uptime_min: 600,
                }),
                pointing: Some(PointingStats {
                    wakes: 70_000,
                    reads: 1_000_000,
                }),
                power: Some(power()),
            },
            right: HalfDiag {
                sleep: None,
                pointing: None,
                power: None,
            },
        }
    }

    fn payload<T: Serialize>(value: &T) -> Vec<u8> {
        let mut buf = [0; 256];
        postcard::to_slice(value, &mut buf).unwrap().to_vec()
    }

    fn request<E: Endpoint>(request: &E::Request) -> Vec<u8> {
        encode_request::<E>(1, request, 256).unwrap()
    }

    fn reply<E: Endpoint>(response: E::Response) -> Vec<u8> {
        testing::ok(
            RynkHeader {
                cmd: E::CMD,
                seq: 1,
            },
            response,
        )
    }

    /// The logical frame (header and payload) of one wire frame.
    fn deframe(wire: &[u8]) -> Vec<u8> {
        let mut buf = vec![0; 256];
        let mut deframer = Deframer::new();
        deframer.tail(&mut buf)[..wire.len()].copy_from_slice(wire);
        deframer.commit(wire.len());
        let len = deframer.next(&mut buf).expect("a whole frame");
        buf.truncate(len);
        buf
    }

    #[test]
    fn encodes_what_the_firmware_snapshot_holds() {
        let ours: BTreeMap<&str, Vec<u8>> = [
            ("AgentSummary", payload(&agents())),
            ("UsageSummary", payload(&usage())),
            ("HalvesDiag", payload(&halves())),
            ("SideInfo", payload(&info())),
            ("SetAgents request", request::<SetAgents>(&agents())),
            ("SetAgents reply", reply::<SetAgents>(())),
            ("SetUsage request", request::<SetUsage>(&usage())),
            ("SetUsage reply", reply::<SetUsage>(())),
            ("GetHalvesDiag request", request::<GetHalvesDiag>(&())),
            ("GetHalvesDiag reply", reply::<GetHalvesDiag>(halves())),
            ("GetSideInfo request", request::<GetSideInfo>(&())),
            ("GetSideInfo reply", reply::<GetSideInfo>(info())),
        ]
        .into_iter()
        .collect();
        let firmware = snapshot();

        assert_eq!(
            ours.keys().collect::<Vec<_>>(),
            firmware.keys().collect::<Vec<_>>()
        );
        for (label, bytes) in &firmware {
            assert_eq!(&ours[label], bytes, "{label}");
        }
    }

    #[test]
    fn decodes_the_firmware_replies() {
        let firmware = snapshot();
        let reply = |label: &str| deframe(&firmware[label]);

        assert_eq!(
            decode_reply::<GetHalvesDiag>(&reply("GetHalvesDiag reply")).unwrap(),
            halves()
        );
        assert_eq!(
            decode_reply::<GetSideInfo>(&reply("GetSideInfo reply")).unwrap(),
            info()
        );
        decode_reply::<SetAgents>(&reply("SetAgents reply")).unwrap();
        decode_reply::<SetUsage>(&reply("SetUsage reply")).unwrap();
    }

    #[test]
    fn serves_only_its_own_revision() {
        assert!(info().serves(caps::AGENTS));
        assert!(!SideInfo {
            caps: caps::AGENTS,
            ..info()
        }
        .serves(caps::HALVES_DIAG));
        assert!(!SideInfo {
            revision: SIDE_REVISION + 1,
            ..info()
        }
        .serves(caps::AGENTS));
    }

    #[test]
    fn diagnostics_land_in_the_log_by_name() {
        let left = battery_log::HalfDiag::from(halves().left);

        assert_eq!(
            left.sleep,
            Some(battery_log::SleepStats {
                wakes: 7,
                awake_min: 30,
                uptime_min: 600,
            })
        );
        assert_eq!(
            left.pointing,
            Some(battery_log::PointingStats {
                wakes: 70_000,
                reads: 1_000_000,
            })
        );
        assert_eq!(
            left.power,
            Some(battery_log::PowerDiag {
                flags: 0x21,
                battery_mv: 3900,
                reset_reason: 0x0001_0004,
                boot_count: 3,
                uptime_ms: 600_000,
                cpu_active_ms: 12_000,
                key_presses: 512,
                rssi: -60,
                last_disconnect_reason: 0x08,
                conn_interval_us: 7_500,
                peripheral_latency: 0,
                supervision_timeout_ms: 4_000,
                param_updates: 5,
                disconnects: 2,
                split_tx: 100_000,
                split_rx: 90_000,
            })
        );
        assert_eq!(
            battery_log::HalfDiag::from(halves().right),
            battery_log::HalfDiag::default()
        );
    }
}
