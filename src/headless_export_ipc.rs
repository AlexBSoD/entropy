//! `--export-layout` while the GUI is running: the console process asks the
//! running instance, which owns the keyboards, for the export, and writes the
//! file itself.
//!
//! The channel is a Unix socket in the user's runtime directory. It carries a
//! single command, and the GUI answers it by lending its connection, through a
//! read-only view, to the same pipeline a standalone export runs: whatever the
//! export asks for, only reads reach the keyboard, and nothing it loads
//! reaches the GUI's own state.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;

const PROTOCOL_VERSION: u32 = 1;
/// A connected client has this long to send its request line.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest export the GUI runs, whatever timeout the console asks for.
const MAX_EXPORT_TIME: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum Request {
    ExportLayout {
        version: u32,
        device: Option<String>,
        timeout_ms: u64,
    },
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(crate) enum Reply {
    Exported { entlayout: String },
    NoDevice { message: String },
    Incomplete { missing: Vec<String> },
    Failed { message: String },
}

/// `$XDG_RUNTIME_DIR/entropy/export.sock`, or the config directory where the
/// platform has no runtime directory.
pub(crate) fn socket_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(dirs::config_dir)?;
    Some(base.join("entropy").join("export.sock"))
}

/// One console request waiting for the GUI thread.
pub(crate) struct ExportJob {
    device: Option<String>,
    deadline: Instant,
    reply: mpsc::Sender<Reply>,
}

/// The listening end, owned by the GUI.
pub(crate) struct ExportServer {
    jobs: mpsc::Receiver<ExportJob>,
}

impl ExportServer {
    /// Listens at `path`; each request wakes `ctx` so a backgrounded window
    /// answers without waiting for its next timer.
    pub(crate) fn start(path: &Path, ctx: egui::Context) -> std::io::Result<Self> {
        let listener = bind_private(path)?;
        let (sender, jobs) = mpsc::channel();
        std::thread::Builder::new()
            .name("entropy-export-ipc".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => serve(stream, &sender, &ctx),
                        Err(error) => log::warn!("Export request was not accepted: {error}"),
                    }
                }
            })?;
        Ok(Self { jobs })
    }
}

/// Binds in a directory only the user can enter, replacing a socket that a
/// crashed instance left behind but refusing to steal a live one.
fn bind_private(path: &Path) -> std::io::Result<UnixListener> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(std::io::ErrorKind::AddrInUse.into());
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn serve(stream: UnixStream, jobs: &mpsc::Sender<ExportJob>, ctx: &egui::Context) {
    let mut line = String::new();
    let read = stream
        .set_read_timeout(Some(REQUEST_READ_TIMEOUT))
        .and_then(|()| BufReader::new(&stream).read_line(&mut line));
    let reply = match read {
        Err(error) => Reply::Failed {
            message: format!("request not read: {error}"),
        },
        Ok(_) => match serde_json::from_str::<Request>(&line) {
            Ok(Request::ExportLayout {
                version,
                device,
                timeout_ms,
            }) if version == PROTOCOL_VERSION => {
                let deadline =
                    Instant::now() + Duration::from_millis(timeout_ms).min(MAX_EXPORT_TIME);
                let (reply, answer) = mpsc::channel();
                if jobs
                    .send(ExportJob {
                        device,
                        deadline,
                        reply,
                    })
                    .is_err()
                {
                    return;
                }
                ctx.request_repaint();
                // The console side bounds the wait with its own timeout.
                answer.recv().unwrap_or(Reply::Failed {
                    message: "Entropy closed before the export finished".to_owned(),
                })
            }
            Ok(Request::ExportLayout { version, .. }) => Reply::Failed {
                message: format!("unsupported export protocol version {version}"),
            },
            Err(error) => Reply::Failed {
                message: format!("unsupported request: {error}"),
            },
        },
    };
    let mut stream = stream;
    if let Ok(mut json) = serde_json::to_string(&reply) {
        json.push('\n');
        let _ = stream.write_all(json.as_bytes());
    }
}

/// Asks the running instance for the export and writes it. `None` when no
/// instance listens, so the caller keeps reporting a running instance.
pub(crate) fn export_via_running_instance(request: &HeadlessExportRequest) -> Option<i32> {
    export_via_socket(&socket_path()?, request)
}

pub(super) fn export_via_socket(path: &Path, request: &HeadlessExportRequest) -> Option<i32> {
    let mut stream = UnixStream::connect(path).ok()?;
    log::info!("Asking the running Entropy for the layout");
    let mut line = serde_json::to_string(&Request::ExportLayout {
        version: PROTOCOL_VERSION,
        device: request.device.clone(),
        timeout_ms: request.timeout.as_millis().try_into().unwrap_or(u64::MAX),
    })
    .expect("export request serializes");
    line.push('\n');
    let mut answer = String::new();
    let exchanged = stream
        .set_read_timeout(Some(request.timeout))
        .and_then(|()| stream.write_all(line.as_bytes()))
        .and_then(|()| BufReader::new(&stream).read_line(&mut answer));
    if let Err(error) = exchanged {
        log::error!("The running Entropy did not answer the export: {error}");
        return Some(EXIT_INSTANCE_RUNNING);
    }
    let reply = match serde_json::from_str::<Reply>(&answer) {
        Ok(reply) => reply,
        Err(error) => {
            log::error!("The running Entropy sent an unreadable answer: {error}");
            return Some(EXIT_FAILED);
        }
    };
    Some(match reply {
        Reply::Exported { entlayout } => match write_export(&request.destination, &entlayout) {
            Ok(destination) => {
                log::info!("Exported layout to {destination}");
                EXIT_OK
            }
            Err(error) => {
                log::error!("Writing the layout failed: {error}");
                EXIT_FAILED
            }
        },
        Reply::NoDevice { message } => {
            log::error!("{message}");
            EXIT_NO_DEVICE
        }
        Reply::Incomplete { missing } => {
            ExportError::Incomplete(missing).report();
            EXIT_INCOMPLETE
        }
        Reply::Failed { message } => {
            log::error!("{message}");
            EXIT_FAILED
        }
    })
}

/// Runs the standalone export over `hid`, the running instance's own
/// connection, lent for the duration through a read-only view: every write
/// is refused at the transport, failed reads are listed instead of exported,
/// and what the export loads stays in its own app.
pub(crate) fn export_lent_keyboard(
    hid: &crate::hid::HidDevice,
    device: Device,
    deadline: Instant,
) -> Reply {
    let session = crate::hid::ReadOnlyHidSession::default();
    let Some(view) = hid.read_only_view(session.clone()) else {
        return Reply::Failed {
            message: "This keyboard's connection cannot be lent to an export".to_owned(),
        };
    };
    log::info!("Exporting layout of {} for the console", describe(&device));
    let mut app = EntropyApp::new_headless();
    app.device_manager.replace_devices(vec![device]);
    app.connect_hid = Some(view);
    let exported = export_entlayout(&mut app, 0, &session, deadline);
    // A worker the export leaves behind must not reach the keyboard once the
    // GUI has it back.
    session.close();
    let refused = session.refused_requests().len();
    if refused > 0 {
        log::info!("Refused {refused} keyboard write(s) during the console export");
    }
    match exported {
        Ok(entlayout) => Reply::Exported { entlayout },
        Err(ExportError::Incomplete(missing)) => Reply::Incomplete { missing },
        Err(ExportError::Failed(message)) => Reply::Failed { message },
    }
}

/// A console export waiting for the connection, or running on it.
pub(crate) struct PendingConsoleExport {
    reply: mpsc::Sender<Reply>,
    generation: u64,
    device: Device,
    deadline: Instant,
    started: bool,
}

impl EntropyApp {
    pub(crate) fn start_console_export_server(&mut self, ctx: &egui::Context) {
        let Some(path) = socket_path() else {
            return;
        };
        match ExportServer::start(&path, ctx.clone()) {
            Ok(server) => self.console_export_server = Some(server),
            Err(error) => log::warn!(
                "Console exports are unavailable ({}): {error}",
                path.display()
            ),
        }
    }

    /// Answers console exports, one at a time, from the selected keyboard.
    pub(crate) fn poll_console_export(&mut self, ctx: &egui::Context) {
        if self.pending_console_export.is_none() {
            let Some(job) = self
                .console_export_server
                .as_ref()
                .and_then(|server| server.jobs.try_recv().ok())
            else {
                return;
            };
            match self.accept_console_export(job.device.as_deref()) {
                Ok(device) => {
                    self.pending_console_export = Some(PendingConsoleExport {
                        reply: job.reply,
                        generation: self.connection_generation,
                        device,
                        deadline: job.deadline,
                        started: false,
                    })
                }
                Err(reply) => {
                    let _ = job.reply.send(reply);
                    return;
                }
            }
        }
        if let Some(reply) = self.console_export_progress(ctx) {
            self.finish_console_export(reply);
        }
    }

    fn accept_console_export(&self, filter: Option<&str>) -> Result<Device, Reply> {
        let device = self
            .selected_device
            .and_then(|index| self.device_manager.devices().get(index))
            .filter(|_| self.hid_device.is_some() || self.vial_hid_task_active())
            .filter(|_| self.layout.is_some() && matches!(self.connect_state, ConnectState::Idle))
            .ok_or_else(|| Reply::NoDevice {
                message: "The running Entropy has no keyboard connected".to_owned(),
            })?;
        if let Some(filter) = filter.filter(|filter| !device_matches(device, filter)) {
            return Err(Reply::NoDevice {
                message: format!(
                    "The running Entropy is connected to {}, not {filter:?}",
                    describe(device)
                ),
            });
        }
        Ok(device.clone())
    }

    /// Lends the connection to the export as soon as nothing else holds it;
    /// `None` until then, and while the export runs.
    fn console_export_progress(&mut self, ctx: &egui::Context) -> Option<Reply> {
        let connection_generation = self.connection_generation;
        let connected = matches!(self.connect_state, ConnectState::Idle);
        let pending = self.pending_console_export.as_ref()?;
        if pending.generation != connection_generation || !connected {
            return Some(Reply::Failed {
                message: "The keyboard connection was replaced during the export".to_owned(),
            });
        }
        if pending.started {
            return None;
        }
        if Instant::now() >= pending.deadline {
            return Some(Reply::Failed {
                message: "The keyboard stayed busy until the export timed out".to_owned(),
            });
        }
        let operation = vial_hid_task::VialHidOperation::ConsoleExport {
            device: pending.device.clone(),
            deadline: pending.deadline,
        };
        match self.start_vial_hid_operation(ctx, operation) {
            vial_hid_task::VialHidTaskStart::Started => {
                if let Some(pending) = self.pending_console_export.as_mut() {
                    pending.started = true;
                }
                None
            }
            vial_hid_task::VialHidTaskStart::Busy => {
                ctx.request_repaint_after(POLL_INTERVAL);
                None
            }
            vial_hid_task::VialHidTaskStart::NoDevice => Some(Reply::NoDevice {
                message: "The running Entropy has no keyboard connected".to_owned(),
            }),
        }
    }

    /// Hands the answer to the console waiting for it.
    pub(crate) fn finish_console_export(&mut self, reply: Reply) {
        if let Some(pending) = self.pending_console_export.take() {
            let _ = pending.reply.send(reply);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_answers_nothing_but_an_export() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("export.sock");
        let server = ExportServer::start(&socket, egui::Context::default()).unwrap();
        for line in [
            r#"{"command":"set-keycode"}"#,
            r#"{"command":"export-layout","version":2,"device":null,"timeout_ms":1000}"#,
        ] {
            let mut stream = UnixStream::connect(&socket).unwrap();
            stream.write_all(format!("{line}\n").as_bytes()).unwrap();
            let mut answer = String::new();
            BufReader::new(&stream).read_line(&mut answer).unwrap();
            assert!(answer.contains(r#""status":"failed""#), "{answer}");
        }
        assert!(
            server.jobs.try_recv().is_err(),
            "a rejected request reached the GUI"
        );
    }

    #[test]
    fn the_socket_is_private_to_the_user_and_never_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("entropy").join("export.sock");
        let _server = ExportServer::start(&socket, egui::Context::default()).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(socket.parent().unwrap()), 0o700);
        assert_eq!(mode(&socket), 0o600);
        assert!(ExportServer::start(&socket, egui::Context::default()).is_err());
    }

    #[test]
    fn a_socket_left_by_a_crashed_instance_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("export.sock");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        assert!(ExportServer::start(&socket, egui::Context::default()).is_ok());
    }
}
