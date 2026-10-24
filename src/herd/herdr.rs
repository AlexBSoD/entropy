//! herdr socket API: an event subscription plus `agent.list` snapshots, and
//! the thread that follows them for the agent counts.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// `pane.agent_status_changed` needs a pane_id, which would mean tracking
/// subscriptions per pane as they come and go. `pane.updated` is global and
/// already carries agent_status, which is all the counts need.
const SUBSCRIPTIONS: [&str; 5] = [
    "pane.updated",
    "pane.created",
    "pane.closed",
    "pane.exited",
    "pane.agent_detected",
];

/// herdr answers these from memory, so anything slower is a hang, not load.
/// A unix socket has no timeout of its own: a peer that accepts without ever
/// replying would park the feed for good.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Agent states flap between tool calls; a burst of events is read once.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Counts are read at least this often even without events: the firmware
/// expires them after 30 s.
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(10);
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// A subscription that lasted this long resets the reconnect backoff.
const HEALTHY_SESSION: Duration = Duration::from_secs(60);

const AGENT_LIST_ID: &str = "entropy-herd:agent.list";
const SUBSCRIBE_ID: &str = "entropy-herd:subscribe";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentCounts {
    pub(crate) working: u8,
    pub(crate) idle: u8,
    pub(crate) blocked: u8,
    pub(crate) done: u8,
    pub(crate) unknown: u8,
}

impl std::fmt::Display for AgentCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} working, {} idle, {} blocked, {} done, {} unknown",
            self.working, self.idle, self.blocked, self.done, self.unknown
        )
    }
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    id: Option<String>,
    result: Option<AgentList>,
    error: Option<serde_json::Value>,
}

/// A reply checked only for failure: `events.subscribe` answers with a result
/// of its own shape, which must not be held to `agent.list`'s.
#[derive(Deserialize)]
struct Ack {
    #[serde(default)]
    id: Option<String>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct AgentList {
    /// Deliberately not `#[serde(default)]`: a reply whose shape is no longer
    /// understood must be an error, not a confident report of zero agents.
    agents: Vec<Agent>,
}

#[derive(Deserialize)]
struct Agent {
    agent_status: Option<String>,
}

pub(crate) struct Client {
    socket_path: PathBuf,
}

impl Client {
    pub(crate) fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// `$HERDR_SOCKET_PATH`, else herdr's own default in the config directory.
    pub(crate) fn default_socket_path() -> PathBuf {
        if let Some(path) = std::env::var_os("HERDR_SOCKET_PATH") {
            return PathBuf::from(path);
        }
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("herdr")
            .join("herdr.sock")
    }

    /// Connects and sends one request line, returning the reader for the reply.
    fn send(&self, request: &serde_json::Value, what: &str) -> Result<BufReader<UnixStream>> {
        let stream = UnixStream::connect(&self.socket_path)
            .with_context(|| format!("connecting to {}", self.socket_path.display()))?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .with_context(|| what.to_owned())?;
        Ok(reader)
    }

    /// One short-lived request/response call.
    pub(crate) fn agent_counts(&self) -> Result<AgentCounts> {
        let request = serde_json::json!({
            "id": AGENT_LIST_ID,
            "method": "agent.list",
            "params": {},
        });
        let mut reader = self.send(&request, "sending agent.list")?;
        let line = read_reply(&mut reader, "reading agent.list")?;
        let response: Response = serde_json::from_str(&line).context("parsing agent.list")?;
        if let Some(error) = error_of(response.error) {
            bail!("agent.list failed: {error}");
        }
        // An unsolicited line arriving first would otherwise be read as an
        // authoritative agent list.
        if let Some(id) = response.id.as_deref() {
            if id != AGENT_LIST_ID {
                bail!("agent.list answered with a foreign id: {id}");
            }
        }
        let agents = response
            .result
            .context("agent.list returned neither a result nor an error")?
            .agents;
        Ok(tally(agents))
    }

    /// Subscribes to the pane events that hint an agent may have moved.
    pub(crate) fn subscribe(&self) -> Result<Events> {
        let subscriptions: Vec<_> = SUBSCRIPTIONS
            .iter()
            .map(|name| serde_json::json!({ "type": name }))
            .collect();
        let request = serde_json::json!({
            "id": SUBSCRIBE_ID,
            "method": "events.subscribe",
            "params": { "subscriptions": subscriptions },
        });
        let mut reader = self.send(&request, "sending events.subscribe")?;
        let line = read_reply(&mut reader, "reading the subscribe reply")?;
        let reply: Ack = serde_json::from_str(&line).context("parsing the subscribe reply")?;
        if let Some(error) = error_of(reply.error) {
            bail!("subscribe rejected: {error}");
        }
        if let Some(id) = reply.id.as_deref() {
            if id != SUBSCRIBE_ID {
                bail!("subscribe answered with a foreign id: {id}");
            }
        }
        // A quiet herdr is normal once subscribed, unlike a quiet request.
        reader.get_ref().set_read_timeout(None)?;
        log::info!("subscribed to herdr at {}", self.socket_path.display());
        Ok(Events { reader })
    }
}

/// A JSON-RPC reply carries `"error": null` on success, which is a present key
/// holding null, not an absent one.
fn error_of(error: Option<serde_json::Value>) -> Option<serde_json::Value> {
    error.filter(|error| !error.is_null())
}

fn tally(agents: Vec<Agent>) -> AgentCounts {
    let mut counts = AgentCounts::default();
    for agent in agents {
        let slot = match agent.agent_status.as_deref() {
            Some("working") => &mut counts.working,
            Some("idle") => &mut counts.idle,
            Some("blocked") => &mut counts.blocked,
            Some("done") => &mut counts.done,
            _ => &mut counts.unknown,
        };
        *slot = slot.saturating_add(1);
    }
    counts
}

fn read_reply(reader: &mut BufReader<UnixStream>, what: &str) -> Result<String> {
    let mut line = String::new();
    if reader
        .read_line(&mut line)
        .with_context(|| what.to_owned())?
        == 0
    {
        bail!("herdr closed the socket while {what}");
    }
    Ok(line)
}

pub(crate) struct Events {
    reader: BufReader<UnixStream>,
}

impl Events {
    /// Waits for the next event. The payload is deliberately ignored: it is
    /// only a hint that something moved, and `agent.list` is the truth.
    pub(crate) fn next(&mut self) -> Result<()> {
        let mut line = String::new();
        if self
            .reader
            .read_line(&mut line)
            .context("reading the event stream")?
            == 0
        {
            bail!("herdr closed the event stream");
        }
        Ok(())
    }
}

/// Follows herdr on a thread of its own and hands every `agent.list` result
/// to `report`: after events, debounced, and at least every [`HEARTBEAT`].
/// While herdr is away it reports `None` and reconnects with backoff. The feed
/// ends once `report` returns false.
pub(crate) fn follow(
    client: Client,
    report: impl FnMut(Option<AgentCounts>) -> bool + Send + 'static,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("herdr-feed".to_owned())
        .spawn(move || run(&client, report))
}

fn run(client: &Client, mut report: impl FnMut(Option<AgentCounts>) -> bool) {
    let mut backoff = RECONNECT_MIN;
    loop {
        let connected = Instant::now();
        let Err(error) = session(client, &mut report) else {
            return;
        };
        if connected.elapsed() >= HEALTHY_SESSION {
            backoff = RECONNECT_MIN;
        }
        log::warn!("herdr: {error:#}; retrying in {}s", backoff.as_secs());
        if !report(None) {
            return;
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// One subscription: Ok once `report` asks to stop, Err when herdr was lost.
fn session(client: &Client, report: &mut impl FnMut(Option<AgentCounts>) -> bool) -> Result<()> {
    let mut events = client.subscribe()?;
    let (sender, received) = mpsc::channel();
    // The event stream blocks without end, so it gets a reader of its own;
    // it ends at the next event or close after this session gives up on it.
    thread::Builder::new()
        .name("herdr-events".to_owned())
        .spawn(move || loop {
            let event = events.next();
            let lost = event.is_err();
            if sender.send(event).is_err() || lost {
                return;
            }
        })?;
    let mut due = Instant::now();
    loop {
        match received.recv_timeout(due.saturating_duration_since(Instant::now())) {
            // Pull the read in, never push it out: a burst is one read, and a
            // chatty herdr cannot postpone it for as long as it keeps talking.
            Ok(Ok(())) => due = due.min(Instant::now() + DEBOUNCE),
            Ok(Err(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("the herdr event reader ended"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !report(Some(client.agent_counts()?)) {
                    return Ok(());
                }
                due = Instant::now() + HEARTBEAT;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    /// Captured from a live herdr: the subscribe result has a shape of its own,
    /// so holding it to `agent.list`'s would reject every subscription.
    const REAL_SUBSCRIBE_ACK: &str =
        r#"{"id":"entropy-herd:subscribe","result":{"type":"subscription_started"}}"#;
    /// Also captured live, trimmed to the fields the counts read.
    const REAL_AGENT_LIST: &str = r#"{"id":"entropy-herd:agent.list","result":{"type":"agent_list","agents":[{"terminal_id":"t1","agent":"pi","agent_status":"idle"},{"terminal_id":"t2","agent_status":"working"},{"terminal_id":"t3"}]}}"#;

    #[test]
    fn accepts_the_subscribe_ack_herdr_actually_sends() {
        let reply: Ack = serde_json::from_str(REAL_SUBSCRIBE_ACK).expect("parses");
        assert!(error_of(reply.error).is_none());
        assert_eq!(reply.id.as_deref(), Some(SUBSCRIBE_ID));
    }

    #[test]
    fn reads_the_agent_list_herdr_actually_sends() {
        let response: Response = serde_json::from_str(REAL_AGENT_LIST).expect("parses");
        let counts = tally(response.result.expect("has a result").agents);
        assert_eq!(
            counts,
            AgentCounts {
                working: 1,
                idle: 1,
                unknown: 1,
                ..AgentCounts::default()
            }
        );
    }

    #[test]
    fn treats_an_explicit_null_error_as_success() {
        let reply: Ack = serde_json::from_str(r#"{"id":"entropy-herd:subscribe","error":null}"#)
            .expect("parses");
        assert!(error_of(reply.error).is_none());
    }

    #[test]
    fn keeps_a_real_error_object() {
        let reply: Ack =
            serde_json::from_str(r#"{"id":"x","error":{"code":-32601}}"#).expect("parses");
        assert!(error_of(reply.error).is_some());
    }

    #[test]
    fn refuses_an_agent_list_without_an_agents_field() {
        // Counting this as zero agents would blank the screen while agents
        // work, and keep feeding the firmware so its own expiry never shows.
        let response: Result<Response, _> =
            serde_json::from_str(r#"{"id":"x","result":{"type":"agent_list"}}"#);
        assert!(response.is_err());
    }

    /// A herdr that answers `agent.list` with `working` agents and lets the
    /// test push events to, or drop, its subscribers.
    struct FakeHerdr {
        path: PathBuf,
        working: Arc<Mutex<u8>>,
        lists: Arc<Mutex<usize>>,
        subscribers: Arc<Mutex<Vec<UnixStream>>>,
    }

    impl FakeHerdr {
        fn start(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("entropy-herd-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("herdr.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let herdr = Self {
                path,
                working: Arc::default(),
                lists: Arc::default(),
                subscribers: Arc::default(),
            };
            let (working, lists, subscribers) = (
                herdr.working.clone(),
                herdr.lists.clone(),
                herdr.subscribers.clone(),
            );
            thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    let mut request = String::new();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    if reader.read_line(&mut request).is_err() {
                        continue;
                    }
                    if request.contains("events.subscribe") {
                        let _ = writeln!(stream, "{REAL_SUBSCRIBE_ACK}");
                        subscribers.lock().unwrap().push(stream);
                    } else {
                        *lists.lock().unwrap() += 1;
                        let agents = vec![
                            r#"{"agent_status":"working"}"#;
                            *working.lock().unwrap() as usize
                        ];
                        let _ = writeln!(
                            stream,
                            r#"{{"id":"{AGENT_LIST_ID}","result":{{"agents":[{}]}}}}"#,
                            agents.join(",")
                        );
                    }
                }
            });
            herdr
        }

        fn client(&self) -> Client {
            Client::new(self.path.clone())
        }

        fn event(&self) {
            for subscriber in self.subscribers.lock().unwrap().iter_mut() {
                let _ = writeln!(subscriber, r#"{{"type":"pane.updated"}}"#);
            }
        }

        fn drop_subscribers(&self) {
            for subscriber in self.subscribers.lock().unwrap().drain(..) {
                let _ = subscriber.shutdown(std::net::Shutdown::Both);
            }
        }

        fn lists(&self) -> usize {
            *self.lists.lock().unwrap()
        }
    }

    impl Drop for FakeHerdr {
        fn drop(&mut self) {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn reports() -> (
        impl FnMut(Option<AgentCounts>) -> bool + Send + 'static,
        mpsc::Receiver<Option<AgentCounts>>,
    ) {
        let (sender, received) = mpsc::channel();
        (move |counts| sender.send(counts).is_ok(), received)
    }

    fn working(count: u8) -> Option<AgentCounts> {
        Some(AgentCounts {
            working: count,
            ..AgentCounts::default()
        })
    }

    #[test]
    fn the_feed_reads_counts_at_once_and_again_after_events() {
        let herdr = FakeHerdr::start("events");
        *herdr.working.lock().unwrap() = 1;
        let (report, received) = reports();
        follow(herdr.client(), report).unwrap();
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)),
            Ok(working(1))
        );

        *herdr.working.lock().unwrap() = 2;
        herdr.event();
        // The heartbeat is ten seconds off: within five, only the event can
        // have caused this read.
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)),
            Ok(working(2))
        );
    }

    #[test]
    fn a_burst_of_events_is_read_once() {
        let herdr = FakeHerdr::start("burst");
        let (report, received) = reports();
        follow(herdr.client(), report).unwrap();
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        let before = herdr.lists();
        for _ in 0..5 {
            herdr.event();
        }
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        // A burst slower than the debounce may take a second read, never five.
        assert!(
            herdr.lists() - before <= 2,
            "{} reads",
            herdr.lists() - before
        );
    }

    #[test]
    fn the_feed_reports_none_while_herdr_is_away_and_comes_back() {
        let herdr = FakeHerdr::start("away");
        *herdr.working.lock().unwrap() = 3;
        let (report, received) = reports();
        follow(herdr.client(), report).unwrap();
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)),
            Ok(working(3))
        );

        herdr.drop_subscribers();
        assert_eq!(received.recv_timeout(Duration::from_secs(5)), Ok(None));
        // The first reconnect comes a second later.
        assert_eq!(
            received.recv_timeout(Duration::from_secs(5)),
            Ok(working(3))
        );
    }

    #[test]
    fn the_feed_ends_when_its_report_does() {
        let herdr = FakeHerdr::start("end");
        let feed = follow(herdr.client(), |_| false).unwrap();
        let (sender, ended) = mpsc::channel();
        thread::spawn(move || sender.send(feed.join().is_ok()).unwrap());
        assert_eq!(ended.recv_timeout(Duration::from_secs(5)), Ok(true));
    }
}
