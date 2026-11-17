// vim: ts=4 et

//! The replication wire protocol: a framed, binary protocol spoken
//! over a single persistent connection (ssh, pipe, or local -- see
//! transport.rs). See cryo.md, "Wire protocol".
//!
//! Every frame is a fixed 18-byte header, an optional extension
//! block, then the payload. `CONTROL` frame payloads are TLV-encoded
//! so new fields can be added later without breaking old parsers: an
//! unrecognized tag can still be skipped correctly, since its length
//! is self-declared.

use std::io::{self, Read, Write};

const MAGIC: [u8; 2] = [0x43, 0x59]; // "CY"
const VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// A synchronous request or response; TLV-encoded payload.
    Control,
    /// A chunk of `zfs send` stream payload.
    Data,
    /// Ends the current streaming operation without closing the
    /// connection.
    EndOfStream,
    /// Emitted periodically during a long DATA run so a stalled or
    /// dead peer is detectable without relying on ssh/TCP timeouts.
    Heartbeat,
    /// Either side cancelling an in-progress operation.
    Abort,
}

impl FrameType {
    fn to_u16(self) -> u16 {
        match self {
            FrameType::Control => 1,
            FrameType::Data => 2,
            FrameType::EndOfStream => 3,
            FrameType::Heartbeat => 4,
            FrameType::Abort => 5,
        }
    }

    fn from_u16(v: u16) -> io::Result<FrameType> {
        match v {
            1 => Ok(FrameType::Control),
            2 => Ok(FrameType::Data),
            3 => Ok(FrameType::EndOfStream),
            4 => Ok(FrameType::Heartbeat),
            5 => Ok(FrameType::Abort),
            other => Err(bad_data(format!("unknown frame type {other}"))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub frame_type: FrameType,
    pub extension: Vec<u8>,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(frame_type: FrameType, payload: Vec<u8>) -> Frame {
        Frame { frame_type, extension: Vec::new(), payload }
    }

    /// Writes the frame and flushes -- `io::Stdout` (and anything
    /// else that might sit underneath a generic `Write`) buffers
    /// internally, and a frame's bytes won't reliably contain a
    /// newline to trigger a line-buffered auto-flush. Without this,
    /// a reply can sit in the writer's buffer indefinitely while both
    /// ends wait on each other -- a real deadlock this hit once a
    /// session started doing more than one round trip before exiting
    /// (process exit used to flush it for free).
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&MAGIC)?;
        w.write_all(&[VERSION])?;
        w.write_all(&self.frame_type.to_u16().to_be_bytes())?;
        w.write_all(&0u16.to_be_bytes())?; // flags: reserved, unused
        w.write_all(&[0u8])?; // reserved
        w.write_all(&(self.extension.len() as u16).to_be_bytes())?;
        w.write_all(&(self.payload.len() as u64).to_be_bytes())?;
        w.write_all(&self.extension)?;
        w.write_all(&self.payload)?;
        w.flush()?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Frame> {
        let mut magic = [0u8; 2];
        r.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(bad_data("bad magic: stream is desynchronized or not cryo's protocol"));
        }

        let mut version = [0u8; 1];
        r.read_exact(&mut version)?;
        if version[0] != VERSION {
            return Err(bad_data(format!("unsupported protocol version {}", version[0])));
        }

        let mut type_buf = [0u8; 2];
        r.read_exact(&mut type_buf)?;
        let frame_type = FrameType::from_u16(u16::from_be_bytes(type_buf))?;

        let mut flags = [0u8; 2];
        r.read_exact(&mut flags)?; // reserved, ignored

        let mut reserved = [0u8; 1];
        r.read_exact(&mut reserved)?; // reserved, ignored

        let mut header_len_buf = [0u8; 2];
        r.read_exact(&mut header_len_buf)?;
        let header_len = u16::from_be_bytes(header_len_buf) as usize;

        let mut payload_len_buf = [0u8; 8];
        r.read_exact(&mut payload_len_buf)?;
        let payload_len = u64::from_be_bytes(payload_len_buf) as usize;

        let mut extension = vec![0u8; header_len];
        r.read_exact(&mut extension)?;

        let mut payload = vec![0u8; payload_len];
        r.read_exact(&mut payload)?;

        Ok(Frame { frame_type, extension, payload })
    }
}

fn bad_data(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// One field of a `CONTROL` frame's TLV-encoded payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub tag: u16,
    pub value: Vec<u8>,
}

pub mod tag {
    pub const KIND: u16 = 1;
    pub const HOSTNAME: u16 = 2;
    pub const POOL_GUID: u16 = 3;
    pub const DATASET_GUID: u16 = 4;
    pub const DATASET_PATH: u16 = 5;
    pub const DESTINATION_PATH: u16 = 6;
    pub const SNAPSHOT_NAME: u16 = 7;
    pub const RESUME_TOKEN: u16 = 8;
    pub const ERROR_MESSAGE: u16 = 9;
}

impl Field {
    pub fn new(tag: u16, value: impl Into<Vec<u8>>) -> Field {
        Field { tag, value: value.into() }
    }

    pub fn text(tag: u16, value: &str) -> Field {
        Field::new(tag, value.as_bytes().to_vec())
    }

    pub fn u64(tag: u16, value: u64) -> Field {
        Field::new(tag, value.to_be_bytes().to_vec())
    }

    pub fn as_text(&self) -> Option<&str> {
        std::str::from_utf8(&self.value).ok()
    }

    pub fn as_u64(&self) -> Option<u64> {
        <[u8; 8]>::try_from(self.value.as_slice()).ok().map(u64::from_be_bytes)
    }
}

pub fn encode_fields(fields: &[Field]) -> Vec<u8> {
    let mut out = Vec::new();

    for field in fields {
        out.extend_from_slice(&field.tag.to_be_bytes());
        out.extend_from_slice(&(field.value.len() as u32).to_be_bytes());
        out.extend_from_slice(&field.value);
    }

    out
}

pub fn decode_fields(bytes: &[u8]) -> io::Result<Vec<Field>> {
    let mut fields = Vec::new();
    let mut rest = bytes;

    while !rest.is_empty() {
        if rest.len() < 6 {
            return Err(bad_data("truncated TLV field header"));
        }

        let tag = u16::from_be_bytes([rest[0], rest[1]]);
        let len = u32::from_be_bytes([rest[2], rest[3], rest[4], rest[5]]) as usize;
        rest = &rest[6..];

        if rest.len() < len {
            return Err(bad_data("truncated TLV field value"));
        }

        fields.push(Field::new(tag, rest[..len].to_vec()));
        rest = &rest[len..];
    }

    Ok(fields)
}

/// Looks up the first field with `tag` in a decoded TLV payload.
pub fn find(fields: &[Field], tag: u16) -> Option<&Field> {
    fields.iter().find(|f| f.tag == tag)
}

/// Every field with `tag` in a decoded TLV payload, in order --
/// `cryo:target`-style repeated values (e.g. a destination's existing
/// snapshot names) aren't a single field, they're the same tag reused.
pub fn find_all(fields: &[Field], tag: u16) -> impl Iterator<Item = &Field> {
    fields.iter().filter(move |f| f.tag == tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_frame() {
        let frame = Frame::new(FrameType::Data, b"hello".to_vec());
        let mut buf = Vec::new();
        frame.write_to(&mut buf).unwrap();

        let mut cursor = io::Cursor::new(buf);
        let parsed = Frame::read_from(&mut cursor).unwrap();
        assert_eq!(parsed.frame_type, FrameType::Data);
        assert_eq!(parsed.payload, b"hello");
        assert!(parsed.extension.is_empty());
    }

    #[test]
    fn round_trips_multiple_frames_on_one_stream() {
        let frames = [
            Frame::new(FrameType::Control, b"hi".to_vec()),
            Frame::new(FrameType::Data, vec![0u8; 1000]),
            Frame::new(FrameType::EndOfStream, Vec::new()),
        ];

        let mut buf = Vec::new();
        for frame in &frames {
            frame.write_to(&mut buf).unwrap();
        }

        let mut cursor = io::Cursor::new(buf);
        for expected in &frames {
            let parsed = Frame::read_from(&mut cursor).unwrap();
            assert_eq!(parsed.frame_type, expected.frame_type);
            assert_eq!(parsed.payload, expected.payload);
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut cursor = io::Cursor::new(vec![0xff, 0xff, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(Frame::read_from(&mut cursor).is_err());
    }

    #[test]
    fn round_trips_tlv_fields() {
        let fields = vec![
            Field::text(tag::HOSTNAME, "bagofholding"),
            Field::u64(tag::POOL_GUID, 123456789),
        ];

        let encoded = encode_fields(&fields);
        let decoded = decode_fields(&encoded).unwrap();

        assert_eq!(decoded, fields);
        assert_eq!(find(&decoded, tag::HOSTNAME).unwrap().as_text(), Some("bagofholding"));
        assert_eq!(find(&decoded, tag::POOL_GUID).unwrap().as_u64(), Some(123456789));
        assert!(find(&decoded, tag::DATASET_GUID).is_none());
    }

    #[test]
    fn find_all_returns_every_matching_field_in_order() {
        let fields = vec![
            Field::text(tag::SNAPSHOT_NAME, "a"),
            Field::text(tag::HOSTNAME, "x"),
            Field::text(tag::SNAPSHOT_NAME, "b"),
        ];
        let decoded = decode_fields(&encode_fields(&fields)).unwrap();
        let names: Vec<&str> = find_all(&decoded, tag::SNAPSHOT_NAME).filter_map(Field::as_text).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn decode_skips_unknown_tags_gracefully() {
        // An old parser that doesn't know tag 999 can still find a
        // known tag after it, because the unknown field's length is
        // self-declared.
        let fields = vec![Field::new(999, vec![1, 2, 3]), Field::text(tag::HOSTNAME, "x")];
        let decoded = decode_fields(&encode_fields(&fields)).unwrap();
        assert_eq!(find(&decoded, tag::HOSTNAME).unwrap().as_text(), Some("x"));
    }
}
