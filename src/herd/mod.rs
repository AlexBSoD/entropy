//! Personal host feeds for a Qube dongle: herdr agent counts, Claude Code
//! limits and a log of the halves' batteries. They travel over the Rynk
//! commands of personal firmware builds (0x7EF0..=0x7EFF), not over anything
//! a release firmware or upstream Entropy knows about.
// Wired to the Rynk link owner once it exists; until then only tests use it.
#![allow(dead_code)]

pub(crate) mod battery_log;
pub(crate) mod herdr;
pub(crate) mod usage;
