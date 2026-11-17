// vim: ts=4 et

//! The `CONTROL` exchanges that bracket a replication session's data
//! stream -- cryo.md, "Wire protocol". Two kinds of request/response
//! round trip here:
//!
//! - hello/ack: opens the session. The client identifies the source
//!   dataset (hostname, pool GUID, dataset GUID -- the same fields
//!   `cryo:origin` records) and names the destination path; the
//!   server either rejects it (e.g. an origin mismatch against what's
//!   already recorded) or acks with its own hostname and whatever
//!   cryo-managed snapshots the destination already has, which the
//!   client uses to pick an incremental baseline.
//! - check: after streaming the `zfs send` data and an
//!   `END-OF-STREAM` frame, the client asks whether the receive on
//!   the other end actually succeeded -- cryo.md's wire protocol
//!   section notes the client doesn't expect a response tied to the
//!   stream itself, but a session can carry further operations, and
//!   this is exactly the explicit follow-up it describes rather than
//!   a response bolted onto the stream's end.
//!
//! Between those two, the client always sends an explicit
//! proceed/skip signal (`send_proceed`) before any data flows. The
//! client -- not the server -- is the one who knows whether the
//! destination's reported snapshots mean there's nothing new to send;
//! without an explicit signal, the server would have no way to tell
//! "nothing's coming" apart from "a stream's about to start", and
//! would spawn a `zfs receive` that then sits waiting on a stream
//! that never arrives.

use std::io::{Read, Write};

use crate::discover;
use crate::error::{CryoError, Result};
use crate::protocol::{self, Field, Frame, FrameType, tag};
use crate::sysinfo;
use crate::transport::Session;
use crate::zfs;

const KIND_CHECK: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub hostname: String,
    pub pool_guid: u64,
    pub dataset_guid: u64,
    pub dataset_path: String,
    pub destination_path: String,
}

impl Hello {
    pub fn new(dataset: &str, destination_path: &str) -> Result<Hello> {
        Ok(Hello {
            hostname: sysinfo::hostname()?,
            pool_guid: zfs::pool_guid(discover::pool_of(dataset))?,
            dataset_guid: zfs::dataset_guid(dataset)?,
            dataset_path: dataset.to_string(),
            destination_path: destination_path.to_string(),
        })
    }

    fn to_fields(&self) -> Vec<Field> {
        vec![
            Field::text(tag::HOSTNAME, &self.hostname),
            Field::u64(tag::POOL_GUID, self.pool_guid),
            Field::u64(tag::DATASET_GUID, self.dataset_guid),
            Field::text(tag::DATASET_PATH, &self.dataset_path),
            Field::text(tag::DESTINATION_PATH, &self.destination_path),
        ]
    }

    fn from_fields(fields: &[Field]) -> Result<Hello> {
        Ok(Hello {
            hostname: get_text(fields, tag::HOSTNAME, "hostname")?,
            pool_guid: get_u64(fields, tag::POOL_GUID, "pool guid")?,
            dataset_guid: get_u64(fields, tag::DATASET_GUID, "dataset guid")?,
            dataset_path: get_text(fields, tag::DATASET_PATH, "dataset path")?,
            destination_path: get_text(fields, tag::DESTINATION_PATH, "destination path")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloAck {
    pub hostname: String,
    /// Names of cryo-managed snapshots already on the destination
    /// (empty if it doesn't exist yet, or has none) -- the baseline
    /// candidates for an incremental send.
    pub existing_snapshots: Vec<String>,
    /// Set if an earlier transfer to this destination was interrupted
    /// after saving resumable state (cryo.md (k)) -- if so, resuming
    /// it takes priority over any ordinary incremental send, since
    /// zfs won't accept a fresh receive onto this dataset until the
    /// pending resume is either continued or explicitly aborted.
    pub resume_token: Option<String>,
}

impl HelloAck {
    fn to_fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::text(tag::HOSTNAME, &self.hostname)];
        fields.extend(self.existing_snapshots.iter().map(|name| Field::text(tag::SNAPSHOT_NAME, name)));
        if let Some(token) = &self.resume_token {
            fields.push(Field::text(tag::RESUME_TOKEN, token));
        }
        fields
    }

    fn from_fields(fields: &[Field]) -> Result<HelloAck> {
        Ok(HelloAck {
            hostname: get_text(fields, tag::HOSTNAME, "hostname")?,
            existing_snapshots: protocol::find_all(fields, tag::SNAPSHOT_NAME)
                .filter_map(Field::as_text)
                .map(str::to_string)
                .collect(),
            resume_token: protocol::find(fields, tag::RESUME_TOKEN).and_then(Field::as_text).map(str::to_string),
        })
    }
}

fn get_text(fields: &[Field], t: u16, what: &str) -> Result<String> {
    protocol::find(fields, t)
        .and_then(Field::as_text)
        .map(str::to_string)
        .ok_or_else(|| CryoError::Command(format!("message is missing its {what} field")))
}

fn get_u64(fields: &[Field], t: u16, what: &str) -> Result<u64> {
    protocol::find(fields, t)
        .and_then(Field::as_u64)
        .ok_or_else(|| CryoError::Command(format!("message is missing its {what} field")))
}

fn check_error(fields: &[Field]) -> Result<()> {
    match protocol::find(fields, tag::ERROR_MESSAGE).and_then(Field::as_text) {
        Some(message) => Err(CryoError::Command(message.to_string())),
        None => Ok(()),
    }
}

/// Client side of the opening exchange: send our hello, return the
/// server's ack (or its rejection, as `Err`).
pub fn send_hello(session: &mut Session, dataset: &str, destination_path: &str) -> Result<HelloAck> {
    let hello = Hello::new(dataset, destination_path)?;
    let frame = Frame::new(FrameType::Control, protocol::encode_fields(&hello.to_fields()));
    frame.write_to(&mut session.writer)?;

    let reply = Frame::read_from(&mut session.reader)?;
    let fields = protocol::decode_fields(&reply.payload)?;
    check_error(&fields)?;
    HelloAck::from_fields(&fields)
}

/// Server side: reads the client's hello. The caller decides what to
/// do with it (`ack` or `reject`) once it's resolved the destination
/// dataset and checked any recorded `cryo:origin`.
pub fn read_hello<R: Read>(reader: &mut R) -> Result<Hello> {
    let request = Frame::read_from(reader)?;
    let fields = protocol::decode_fields(&request.payload)?;
    Hello::from_fields(&fields)
}

pub fn ack<W: Write>(writer: &mut W, reply: &HelloAck) -> Result<()> {
    let frame = Frame::new(FrameType::Control, protocol::encode_fields(&reply.to_fields()));
    frame.write_to(writer)?;
    Ok(())
}

pub fn reject<W: Write>(writer: &mut W, message: &str) -> Result<()> {
    respond_error(writer, message)
}

/// Client side: tells the server whether a data stream is coming at
/// all. No reply is expected or sent for `proceed = false` -- there's
/// nothing to report either way, and the session just ends there.
pub fn send_proceed(session: &mut Session, proceed: bool) -> Result<()> {
    let fields = [Field::u64(tag::KIND, u64::from(proceed))];
    Frame::new(FrameType::Control, protocol::encode_fields(&fields)).write_to(&mut session.writer)?;
    Ok(())
}

/// Server side: reads the proceed/skip signal.
pub fn read_proceed<R: Read>(reader: &mut R) -> Result<bool> {
    let frame = Frame::read_from(reader)?;
    let fields = protocol::decode_fields(&frame.payload)?;
    Ok(get_u64(&fields, tag::KIND, "kind")? != 0)
}

/// Client side of the post-stream check: did the receive actually
/// succeed?
pub fn check_result(session: &mut Session) -> Result<()> {
    let fields = [Field::u64(tag::KIND, KIND_CHECK)];
    let frame = Frame::new(FrameType::Control, protocol::encode_fields(&fields));
    frame.write_to(&mut session.writer)?;

    let reply = Frame::read_from(&mut session.reader)?;
    let fields = protocol::decode_fields(&reply.payload)?;
    check_error(&fields)
}

/// Server side of the post-stream check: just drains the request --
/// its only content is which kind of request it is, already implied
/// by its position in the session -- so the caller can reply with
/// `respond_ok`/`respond_error` once it knows whether the receive
/// actually succeeded.
pub fn read_check_request<R: Read>(reader: &mut R) -> Result<()> {
    Frame::read_from(reader)?;
    Ok(())
}

pub fn respond_ok<W: Write>(writer: &mut W) -> Result<()> {
    Frame::new(FrameType::Control, Vec::new()).write_to(writer)?;
    Ok(())
}

pub fn respond_error<W: Write>(writer: &mut W, message: &str) -> Result<()> {
    let fields = [Field::text(tag::ERROR_MESSAGE, message)];
    Frame::new(FrameType::Control, protocol::encode_fields(&fields)).write_to(writer)?;
    Ok(())
}
