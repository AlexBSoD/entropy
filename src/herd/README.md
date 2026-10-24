# herd: a personal branch on top of Entropy

Feeds a Qube on personal firmware what qubeherd used to: the herdr agent counts
(`SetAgents` 0x7EF0), the Claude Code limits (`SetUsage` 0x7EF1), and a CSV log
of the halves (`GetBatteryHalves`, `GetHalvesDiag` 0x7EF8). It never goes
upstream: no release firmware serves these commands, and a board that answers
`GetSideInfo` 0x7EFF with `UnknownCmd` is left alone.

## Outside `src/herd/`

Three hunks, all `#[cfg(target_os = "linux")]`:

| File | Hunk |
|---|---|
| `src/main.rs` | `mod herd;` |
| `src/app_init.rs` | `crate::herd::start();` in `EntropyApp::new`, after `smart_input::start()` |
| `src/ui/app_lifecycle.rs` | `crate::herd::stop();` at the end of `on_exit` |

`EntropyApp::new` is the GUI path only (`--export-layout` builds the app with
`new_headless`). `start()` does nothing under `cfg(test)`, since the GUI tests
build the app too.

## What it leans on

- `rynk::presence::subscribe` and `LinkEvent` (E5): boards as they come and go.
- `RynkLink::open`, `post`, `request_background` (E4): one shared link per
  board, the editor first.
- `rynk::client::{encode_request, decode_reply, testing}` (E3), in tests.
- `rmk_types_rynk::protocol::rynk::{command::Endpoint, Cmd, ergohaven}`.

A rebase that renames any of these breaks only `src/herd/`; nothing else here
reaches into Entropy.

## The side table

`side.rs` copies the firmware's side table field for field: postcard carries no
names. `side.snap` is the firmware's own snapshot of it; replace it whenever the
firmware's changes, and the tests say whether this copy still matches.

## Environment

- `ENTROPY_HERD=0` leaves it off.
- `HERDR_SOCKET_PATH`, `CLAUDE_CONFIG_DIR`: where herdr and Claude Code live.
- `ENTROPY_HERD_LOG_DIR`: where the logs go, one directory per keyboard id;
  by default `$XDG_STATE_HOME/entropy/herd/`.
