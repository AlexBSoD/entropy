//! `--export-layout` while the GUI is running: the console process asks the
//! running instance, which owns the keyboards, for the same full snapshot its
//! Layout menu would save, and writes the file itself.
//!
//! The channel is a Unix socket in the user's runtime directory. It carries a
//! single command, so whatever reaches it can only read the keyboard: the GUI
//! answers it from its existing connection and deferred-load pipeline.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;

const PROTOCOL_VERSION: u32 = 1;
/// A connected client has this long to send its request line.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum Request {
    ExportLayout {
        version: u32,
        device: Option<String>,
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
            Ok(Request::ExportLayout { version, device }) if version == PROTOCOL_VERSION => {
                let (reply, answer) = mpsc::channel();
                if jobs.send(ExportJob { device, reply }).is_err() {
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

/// A console export the GUI is loading for.
pub(crate) struct PendingConsoleExport {
    reply: mpsc::Sender<Reply>,
    generation: u64,
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
                Ok(generation) => {
                    self.pending_console_export = Some(PendingConsoleExport {
                        reply: job.reply,
                        generation,
                    })
                }
                Err(reply) => {
                    let _ = job.reply.send(reply);
                    return;
                }
            }
        }
        let Some(reply) = self.console_export_progress(ctx) else {
            ctx.request_repaint_after(POLL_INTERVAL);
            return;
        };
        if let Some(pending) = self.pending_console_export.take() {
            let _ = pending.reply.send(reply);
        }
    }

    fn accept_console_export(&self, filter: Option<&str>) -> Result<u64, Reply> {
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
        if self.deferred_full_layout_action.is_some() {
            return Err(Reply::Failed {
                message: "Entropy is finishing another layout action; retry later".to_owned(),
            });
        }
        Ok(self.connection_generation)
    }

    /// `None` while the snapshot is still loading.
    fn console_export_progress(&mut self, ctx: &egui::Context) -> Option<Reply> {
        let generation = self.pending_console_export.as_ref()?.generation;
        if generation != self.connection_generation
            || !matches!(self.connect_state, ConnectState::Idle)
        {
            return Some(Reply::Failed {
                message: "The keyboard connection was replaced during the export".to_owned(),
            });
        }
        // A window hidden to the tray stops loading on its own; this export
        // still needs every layer and section.
        self.maybe_start_deferred_device_load(ctx, false);
        let gaps = self.entlayout_export_gaps();
        let failed: Vec<String> = gaps
            .iter()
            .filter(|(_, status)| matches!(status, DeferredLoadStatus::Failed(_)))
            .map(|(name, _)| name.clone())
            .collect();
        if !failed.is_empty() {
            return Some(Reply::Incomplete { missing: failed });
        }
        if !gaps.is_empty()
            || !self.deferred_full_layout_action_ready(DeferredFullLayoutAction::ExportEntlayout)
            || self.vial_hid_task_active()
        {
            return None;
        }
        Some(match self.entlayout_export_json() {
            Some(Ok(entlayout)) => Reply::Exported { entlayout },
            Some(Err(error)) => Reply::Failed {
                message: format!("{error:#}"),
            },
            None => Reply::Failed {
                message: "No keyboard layout to export".to_owned(),
            },
        })
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
            r#"{"command":"export-layout","version":2,"device":null}"#,
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
