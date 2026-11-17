// vim: ts=4 et

//! Forwarding a `zfs send`'s stdout as `DATA` frames, and the
//! reverse, into a `zfs receive`'s stdin -- cryo.md, "Wire protocol".
//! Chunked at 256KB, well within the 128KB-1MB range the design calls
//! for, so neither side ever has to buffer a whole stream in memory.
//!
//! Both directions read their blocking source (a child process's
//! pipe, or the connection itself) on a dedicated thread that feeds a
//! channel, so the main loop can `recv_timeout` against it instead of
//! blocking directly. That's what makes `HEARTBEAT` emission honest:
//! a `recv_timeout` firing is a real wall-clock deadline, independent
//! of how fast -- or slow -- the thing actually producing bytes
//! happens to be. A plain blocking read has no way to notice "it's
//! been 10 seconds" while it's sitting inside that read, which would
//! mean a slow-but-fine local disk could never be told apart from a
//! genuinely dead connection.

use std::io::{self, Read, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use crate::error::{CryoError, Result};
use crate::protocol::{Frame, FrameType};

const CHUNK_SIZE: usize = 256 * 1024;

/// How long to go without a real chunk before emitting a `HEARTBEAT`
/// in its place.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// How long the receiving side waits for *anything* -- `DATA` or
/// `HEARTBEAT` -- before concluding the peer is gone. Comfortably
/// wider than `HEARTBEAT_INTERVAL` so ordinary scheduling jitter on
/// either end never reads as a stall.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Forwards all of `source` as `DATA` frames to `sink`, emitting a
/// `HEARTBEAT` in place of any gap longer than `heartbeat_interval`
/// (production call sites pass `HEARTBEAT_INTERVAL`; tests pass
/// something short enough to run quickly while still proving the
/// timing logic). Does not itself send `END-OF-STREAM` or `ABORT` --
/// the caller knows whether the process that produced `source`
/// actually succeeded (this loop only sees bytes, not that process's
/// exit status), so it decides which terminator frame is honest.
pub fn send_stream<R, W>(source: R, sink: &mut W, heartbeat_interval: Duration) -> Result<u64>
where
    R: Read + Send + 'static,
    W: Write,
{
    let (tx, rx) = mpsc::channel::<io::Result<Vec<u8>>>();

    thread::spawn(move || {
        let mut source = source;
        let mut buf = vec![0u8; CHUNK_SIZE];

        loop {
            match source.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }
    });

    let mut total = 0u64;

    loop {
        match rx.recv_timeout(heartbeat_interval) {
            Ok(Ok(chunk)) => {
                let len = chunk.len() as u64;
                Frame::new(FrameType::Data, chunk).write_to(sink)?;
                total += len;
            }
            Ok(Err(e)) => return Err(CryoError::from(e)),
            Err(RecvTimeoutError::Timeout) => {
                Frame::new(FrameType::Heartbeat, Vec::new()).write_to(sink)?;
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(total),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamOutcome {
    Completed,
    Aborted,
}

/// Reads frames from `source`, writing each `DATA` frame's payload to
/// `sink`, until `END-OF-STREAM` or `ABORT` -- or until `stall_timeout`
/// passes with nothing arriving at all, which is reported as an error
/// rather than left to hang forever. A `CONTROL` frame here would
/// mean the sender is out of sync with where we are in the session --
/// not something to recover from.
///
/// On a structural success (`Completed`/`Aborted`), `source` is
/// handed back so the caller can keep using it (e.g. for the
/// post-stream check in handshake.rs) -- the reader thread only ever
/// stops after delivering a terminal frame, so reclaiming it here
/// never blocks. On a hard failure there's nothing to reclaim: the
/// reader thread may still be stuck in a blocked read with no way to
/// cancel it (that's what "stalled" means), so it's simply abandoned,
/// safe because the caller is a short-lived `cryo server` process
/// about to exit anyway.
pub fn receive_stream<R, W>(source: R, sink: &mut W, stall_timeout: Duration) -> Result<(R, StreamOutcome)>
where
    R: Read + Send + 'static,
    W: Write,
{
    let (tx, rx) = mpsc::channel::<io::Result<Frame>>();

    let handle = thread::spawn(move || {
        let mut source = source;

        loop {
            match Frame::read_from(&mut source) {
                Ok(frame) => {
                    let terminal = matches!(frame.frame_type, FrameType::EndOfStream | FrameType::Abort);
                    let sent = tx.send(Ok(frame)).is_ok();
                    if terminal || !sent {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    break;
                }
            }
        }

        source
    });

    loop {
        match rx.recv_timeout(stall_timeout) {
            Ok(Ok(frame)) => match frame.frame_type {
                FrameType::Data => sink.write_all(&frame.payload)?,
                FrameType::Heartbeat => {}
                FrameType::EndOfStream => return Ok((reclaim(handle)?, StreamOutcome::Completed)),
                FrameType::Abort => return Ok((reclaim(handle)?, StreamOutcome::Aborted)),
                FrameType::Control => {
                    return Err(CryoError::Command(
                        "unexpected CONTROL frame during the data stream".to_string(),
                    ));
                }
            },
            Ok(Err(e)) => return Err(CryoError::from(e)),
            Err(RecvTimeoutError::Timeout) => {
                return Err(CryoError::Command(format!(
                    "no data received for over {}s -- the connection appears to have stalled",
                    stall_timeout.as_secs()
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(CryoError::Command(
                    "connection closed unexpectedly during the data stream".to_string(),
                ));
            }
        }
    }
}

fn reclaim<R>(handle: thread::JoinHandle<R>) -> Result<R> {
    handle.join().map_err(|_| CryoError::Command("the stream reader thread panicked".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Produces `chunks` one at a time, sleeping `delay` before every
    /// one after the first -- simulating a source that's merely slow,
    /// not dead, so a correct implementation keeps the connection
    /// alive with heartbeats rather than mistaking it for a stall.
    struct SlowReader {
        chunks: std::vec::IntoIter<Vec<u8>>,
        delay: Duration,
        first: bool,
    }

    impl Read for SlowReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.first {
                thread::sleep(self.delay);
            }
            self.first = false;

            match self.chunks.next() {
                Some(chunk) => {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                None => Ok(0),
            }
        }
    }

    /// A source that never produces anything and never errors --
    /// standing in for a genuinely stalled connection.
    #[derive(Debug)]
    struct BlockForever;

    impl Read for BlockForever {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            loop {
                thread::sleep(Duration::from_secs(3600));
            }
        }
    }

    fn read_all_frames(bytes: Vec<u8>) -> Vec<Frame> {
        let mut cursor = io::Cursor::new(bytes);
        let mut frames = Vec::new();
        while let Ok(frame) = Frame::read_from(&mut cursor) {
            frames.push(frame);
        }
        frames
    }

    #[test]
    fn emits_heartbeats_during_a_slow_source() {
        let chunks = vec![b"first".to_vec(), b"second".to_vec()];
        let reader = SlowReader { chunks: chunks.into_iter(), delay: Duration::from_millis(60), first: true };

        let mut sink = Vec::new();
        let total = send_stream(reader, &mut sink, Duration::from_millis(20)).unwrap();
        assert_eq!(total, 11); // b"first".len() + b"second".len()

        let frames = read_all_frames(sink);
        let payloads: Vec<&[u8]> = frames
            .iter()
            .filter(|f| f.frame_type == FrameType::Data)
            .map(|f| f.payload.as_slice())
            .collect();
        assert_eq!(payloads, vec![b"first".as_slice(), b"second".as_slice()]);

        let heartbeats = frames.iter().filter(|f| f.frame_type == FrameType::Heartbeat).count();
        assert!(heartbeats > 0, "expected at least one heartbeat frame, got {frames:?}");
    }

    #[test]
    fn receive_stream_completes_normally_when_not_stalled() {
        let mut encoded = Vec::new();
        Frame::new(FrameType::Data, b"hello".to_vec()).write_to(&mut encoded).unwrap();
        Frame::new(FrameType::EndOfStream, Vec::new()).write_to(&mut encoded).unwrap();

        let mut sink = Vec::new();
        let (_, outcome) =
            receive_stream(io::Cursor::new(encoded), &mut sink, Duration::from_millis(200)).unwrap();
        assert_eq!(outcome, StreamOutcome::Completed);
        assert_eq!(sink, b"hello");
    }

    #[test]
    fn detects_a_stalled_source() {
        let mut sink = Vec::new();
        let result = receive_stream(BlockForever, &mut sink, Duration::from_millis(50));
        assert!(result.is_err(), "expected a stall timeout error, got {result:?}");
    }
}
