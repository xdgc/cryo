// vim: ts=4 et

//! Opens a byte-stream session to a replication target's `cryo
//! server`, per cryo.md, "Replication behaviors" (e): ssh (spawns
//! `ssh <endpoint> cryo server`), pipe (runs `detail` as a shell
//! command that's expected to connect to a remote `cryo server`), or
//! local (runs `cryo server` as a subprocess directly). All three
//! just need to produce something to write frames to and read frames
//! from -- a child process's stdin/stdout pipes, in every case.

use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::error::{CryoError, Result};
use crate::target::{Target, Transport};

pub struct Session {
    child: Child,
    pub reader: ChildStdout,
    pub writer: ChildStdin,
}

pub fn connect(target: &Target) -> Result<Session> {
    let mut command = match &target.transport {
        Transport::Local => {
            let exe = std::env::current_exe()?;
            let mut command = Command::new(exe);
            command.arg("server");
            command
        }
        Transport::Ssh => {
            let endpoint = target.endpoint.as_deref().ok_or_else(|| {
                CryoError::Command("ssh transport requires an endpoint".to_string())
            })?;
            let mut command = Command::new("ssh");
            command.arg(endpoint).arg("cryo").arg("server");
            command
        }
        Transport::Pipe(detail) => {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(detail);
            command
        }
    };

    command.stdin(Stdio::piped()).stdout(Stdio::piped());

    let mut child = command.spawn()?;
    let writer = child.stdin.take().expect("spawned with piped stdin");
    let reader = child.stdout.take().expect("spawned with piped stdout");

    Ok(Session { child, reader, writer })
}

impl Session {
    /// Closes the write side (signaling EOF to the peer, same as
    /// closing the whole connection would from its point of view once
    /// it's finished reading) and waits for the server process to
    /// exit, reporting a nonzero exit as an error.
    pub fn finish(self) -> Result<()> {
        let Session { mut child, reader, writer } = self;
        drop(writer);
        drop(reader);

        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(CryoError::Command(format!("cryo server exited with {status}")))
        }
    }
}
