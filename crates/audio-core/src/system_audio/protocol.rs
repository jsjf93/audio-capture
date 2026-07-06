//! Rust-side decoder for the wire protocol documented in
//! `docs/audio-tap-protocol.md`. This is one of two independent
//! implementations of that spec — the other is the Swift encoder in
//! `swift-helper/Sources/AudioTapHelper/Framing.swift`.

use serde::Deserialize;
use std::io::{self, Read};

const MAGIC: [u8; 4] = *b"ATAP";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 12;

#[derive(Debug, Clone)]
pub enum TapMessage {
    Audio(AudioMessage),
    StatusEvent(StatusEvent),
    Heartbeat,
}

/// Interleaved f32 PCM samples captured by the helper, plus the metadata
/// needed to interpret them. Every field here is self-describing on
/// purpose (see the protocol doc) so a consumer never needs an
/// out-of-band channel to know how to read `samples`.
#[derive(Debug, Clone)]
pub struct AudioMessage {
    pub timestamp_ns: u64,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_count: u32,
    pub samples: Vec<f32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatusEvent {
    pub level: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("bad magic bytes at start of message header")]
    BadMagic,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),
    #[error("unknown message type {0}")]
    UnknownType(u8),
    #[error("audio payload is {actual} bytes, shorter than the 18-byte fixed header")]
    TruncatedAudioHeader { actual: usize },
    #[error("audio payload declares format {0}, only 0 (interleaved f32 LE) is supported")]
    UnsupportedFormat(u8),
    #[error("audio payload sample data is {actual} bytes, expected {expected} for frame_count * channels * 4")]
    SampleDataLengthMismatch { expected: usize, actual: usize },
    #[error("status event payload was not valid UTF-8 JSON: {0}")]
    MalformedStatusEvent(String),
}

/// Reads one message from `reader`. Returns `Ok(None)` on a clean
/// end-of-stream (EOF exactly at a message boundary — the normal way this
/// ends when the helper process exits). Any other error is treated as
/// fatal to this stream, per the protocol doc's "no resynchronization"
/// policy: a corrupted header doesn't try to scan forward for the next
/// `MAGIC`, it's reported and the caller (in production, the process
/// supervisor) decides what to do — typically, restart the helper.
pub fn read_message<R: Read>(reader: &mut R) -> Result<Option<TapMessage>, ProtocolError> {
    let mut header = [0u8; HEADER_LEN];
    if !read_exact_or_clean_eof(reader, &mut header)? {
        return Ok(None);
    }

    if header[0..4] != MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    let version = header[4];
    if version != VERSION {
        return Err(ProtocolError::UnsupportedVersion(version));
    }
    let message_type = header[5];
    let payload_len = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;

    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload)?;

    match message_type {
        0 => Ok(Some(TapMessage::Audio(parse_audio_payload(&payload)?))),
        1 => Ok(Some(TapMessage::StatusEvent(parse_status_payload(&payload)?))),
        2 => Ok(Some(TapMessage::Heartbeat)),
        other => Err(ProtocolError::UnknownType(other)),
    }
}

fn parse_audio_payload(payload: &[u8]) -> Result<AudioMessage, ProtocolError> {
    const FIXED_HEADER_LEN: usize = 18;
    if payload.len() < FIXED_HEADER_LEN {
        return Err(ProtocolError::TruncatedAudioHeader { actual: payload.len() });
    }

    let timestamp_ns = u64::from_le_bytes(payload[0..8].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(payload[8..12].try_into().unwrap());
    let channels = payload[12];
    let format = payload[13];
    if format != 0 {
        return Err(ProtocolError::UnsupportedFormat(format));
    }
    let frame_count = u32::from_le_bytes(payload[14..18].try_into().unwrap());

    let sample_bytes = &payload[FIXED_HEADER_LEN..];
    let expected = frame_count as usize * channels as usize * 4;
    if sample_bytes.len() != expected {
        return Err(ProtocolError::SampleDataLengthMismatch {
            expected,
            actual: sample_bytes.len(),
        });
    }

    let samples = sample_bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();

    Ok(AudioMessage {
        timestamp_ns,
        sample_rate,
        channels,
        frame_count,
        samples,
    })
}

fn parse_status_payload(payload: &[u8]) -> Result<StatusEvent, ProtocolError> {
    serde_json::from_slice(payload).map_err(|e| ProtocolError::MalformedStatusEvent(e.to_string()))
}

/// Like `Read::read_exact`, but distinguishes "EOF before any byte of this
/// call was read" (a clean stream end — `Ok(false)`) from "EOF partway
/// through" (a truncated message — an error, since a well-formed stream
/// only ever ends between messages, never inside one).
fn read_exact_or_clean_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<bool, io::Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended in the middle of a message header",
                ))
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // Small test-only encoder, deliberately independent of Swift's
    // Framing.swift, so these tests exercise the Rust decoder against a
    // byte-for-byte reference built straight from the spec in
    // docs/audio-tap-protocol.md rather than against our own encoder logic.
    fn encode(msg_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(msg_type);
        out.extend_from_slice(&[0, 0]); // flags
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn encode_audio_payload(timestamp_ns: u64, sample_rate: u32, channels: u8, samples: &[f32]) -> Vec<u8> {
        let frame_count = (samples.len() / channels as usize) as u32;
        let mut payload = Vec::new();
        payload.extend_from_slice(&timestamp_ns.to_le_bytes());
        payload.extend_from_slice(&sample_rate.to_le_bytes());
        payload.push(channels);
        payload.push(0); // format = interleaved f32 LE
        payload.extend_from_slice(&frame_count.to_le_bytes());
        for sample in samples {
            payload.extend_from_slice(&sample.to_le_bytes());
        }
        payload
    }

    #[test]
    fn decodes_an_audio_message() {
        let samples = [0.1f32, -0.2, 0.3, -0.4]; // 4 frames, mono
        let payload = encode_audio_payload(123_456_789, 48_000, 1, &samples);
        let bytes = encode(0, &payload);

        let mut cursor = Cursor::new(bytes);
        let msg = read_message(&mut cursor).unwrap().expect("expected a message");
        match msg {
            TapMessage::Audio(audio) => {
                assert_eq!(audio.timestamp_ns, 123_456_789);
                assert_eq!(audio.sample_rate, 48_000);
                assert_eq!(audio.channels, 1);
                assert_eq!(audio.frame_count, 4);
                assert_eq!(audio.samples, samples);
            }
            other => panic!("expected Audio, got {other:?}"),
        }

        // Stream ends cleanly right after the one message.
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn decodes_a_status_event() {
        let payload = br#"{"level":"error","code":"tap_create_failed","message":"boom"}"#;
        let bytes = encode(1, payload);
        let mut cursor = Cursor::new(bytes);

        match read_message(&mut cursor).unwrap().expect("expected a message") {
            TapMessage::StatusEvent(event) => {
                assert_eq!(event.level, "error");
                assert_eq!(event.code, "tap_create_failed");
                assert_eq!(event.message, "boom");
            }
            other => panic!("expected StatusEvent, got {other:?}"),
        }
    }

    #[test]
    fn decodes_a_heartbeat() {
        let bytes = encode(2, &[]);
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(
            read_message(&mut cursor).unwrap().expect("expected a message"),
            TapMessage::Heartbeat
        ));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = encode(2, &[]);
        bytes[0] = b'X'; // corrupt the magic
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(read_message(&mut cursor), Err(ProtocolError::BadMagic)));
    }

    #[test]
    fn rejects_sample_data_length_mismatch() {
        // Claim 4 frames but only supply enough bytes for 2 — a corrupted
        // or truncated audio payload should be a decode error, not a panic
        // or a silently wrong frame count.
        let mut payload = encode_audio_payload(0, 48_000, 1, &[0.1, 0.2]);
        payload[14..18].copy_from_slice(&4u32.to_le_bytes()); // lie about frame_count
        let bytes = encode(0, &payload);
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(
            read_message(&mut cursor),
            Err(ProtocolError::SampleDataLengthMismatch { .. })
        ));
    }

    #[test]
    fn clean_eof_between_messages_returns_none() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn truncated_header_is_an_error_not_a_clean_eof() {
        let bytes = encode(2, &[]);
        let mut cursor = Cursor::new(bytes[..6].to_vec()); // cut off mid-header
        assert!(read_message(&mut cursor).is_err());
    }
}
