//! Newline-delimited JSON over a Unix socket. One request, one response.
//!
//! The client is blocking on purpose: the TUI drives it from its own event
//! loop and a round trip is well under a millisecond.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::model::{Entry, Snapshot, SpawnSpec};
use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Snapshot,
    /// Transcript entries from index `since` on.
    Transcript { name: String, since: usize },
    Diff { name: String },
    Log { lines: usize },
    Spawn { spec: SpawnSpec },
    /// The same task to `count` agents, each in its own worktree.
    Fanout { spec: SpawnSpec, count: usize },
    /// Continue an agent's conversation. Queued if it's mid-turn.
    Send { name: String, message: String },
    /// Send to every worker that isn't merged.
    Broadcast { message: String },
    /// Agent-to-agent (or person-to-agent) message. `to` may be `parent`
    /// or `user`; `from` is blank for a person.
    Message { from: String, to: String, text: String },
    /// A note on the shared board.
    Post { from: String, text: String },
    /// The latest `limit` board posts.
    Board { limit: usize },
    /// An agent's final message from its latest turn.
    Result { name: String },
    Stop { name: String },
    StopAll,
    Review { name: String },
    /// Hand a review's findings back to the agent that was reviewed.
    SendFindings { id: u32 },
    Merge { name: String },
    Remove { name: String },
    SetMaxConcurrent { n: usize },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { message: String },
    Error { message: String },
    Snapshot { snapshot: Box<Snapshot> },
    Transcript { entries: Vec<Entry>, total: usize },
    Text { text: String },
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    broken: bool,
}

impl Client {
    pub fn connect() -> Result<Self> {
        let path = paths::socket_path();
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("connecting to {}", path.display()))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            broken: false,
        })
    }

    /// Is a daemon answering right now?
    pub fn probe() -> bool {
        Client::connect()
            .and_then(|mut c| c.send(&Request::Ping))
            .is_ok()
    }

    pub fn is_broken(&self) -> bool {
        self.broken
    }

    pub fn send(&mut self, request: &Request) -> Result<Response> {
        let result = self.round_trip(request);
        if result.is_err() {
            self.broken = true;
        }
        result
    }

    fn round_trip(&mut self, request: &Request) -> Result<Response> {
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        let mut reply = String::new();
        if self.reader.read_line(&mut reply)? == 0 {
            bail!("daemon closed the connection");
        }
        Ok(serde_json::from_str(&reply)?)
    }

    pub fn snapshot(&mut self) -> Result<Snapshot> {
        match self.send(&Request::Snapshot)? {
            Response::Snapshot { snapshot } => Ok(*snapshot),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    /// For requests answered with a message: `Ok` becomes the text, `Error`
    /// an error carrying it.
    pub fn command(&mut self, request: &Request) -> Result<String> {
        match self.send(request)? {
            Response::Ok { message } | Response::Text { text: message } => Ok(message),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }

    pub fn transcript(&mut self, name: &str, since: usize) -> Result<(Vec<Entry>, usize)> {
        match self.send(&Request::Transcript {
            name: name.to_string(),
            since,
        })? {
            Response::Transcript { entries, total } => Ok((entries, total)),
            Response::Error { message } => bail!(message),
            other => bail!("unexpected reply: {other:?}"),
        }
    }
}
