// vim: ts=4 et

//! The local control channel between a `cryo` invocation and a
//! running `cryo daemon`, over a Unix domain socket. Deliberately
//! much simpler than the remote replication wire protocol (cryo.md,
//! "Wire protocol"): every message here is small and local, never a
//! streaming payload, so there's no reason for it to solve the same
//! problems. One line out, one small response back, connection
//! closed.
//!
//! Request:  "STATUS" | "RUN" | "RESYNC" | "STOP", newline-terminated.
//! Response: first line "OK" or "ERR <message>"; on "OK", any
//! remaining lines are the body. Connection is then closed by the
//! daemon -- there's nothing streamed, so EOF carries no ambiguity
//! here the way it would for a `zfs send` payload.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub enum Request {
    Status,
    Run,
    Resync,
    Stop,
}

impl Request {
    fn line(self) -> &'static str {
        match self {
            Request::Status => "STATUS\n",
            Request::Run => "RUN\n",
            Request::Resync => "RESYNC\n",
            Request::Stop => "STOP\n",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// No daemon is reachable at all; the caller may fall back to
    /// doing the work standalone, where that makes sense.
    Unavailable,

    /// A daemon answered, but with an error.
    Failed(String),
}

pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("CRYO_SOCKET") {
        return PathBuf::from(path);
    }

    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("cryo.sock");
    }

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "shared".to_string());
    PathBuf::from(format!("/tmp/cryo-{user}.sock"))
}

pub fn request(req: Request) -> Result<String, Error> {
    let mut stream = UnixStream::connect(socket_path()).map_err(|_| Error::Unavailable)?;

    stream
        .write_all(req.line().as_bytes())
        .map_err(|e| Error::Failed(e.to_string()))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| Error::Failed(e.to_string()))?;

    let (status, rest) = response
        .split_once('\n')
        .ok_or_else(|| Error::Failed(format!("malformed daemon response: {response:?}")))?;

    if status == "OK" {
        Ok(rest.to_string())
    } else if let Some(msg) = status.strip_prefix("ERR ") {
        Err(Error::Failed(msg.to_string()))
    } else {
        Err(Error::Failed(format!("malformed daemon response: {status:?}")))
    }
}
