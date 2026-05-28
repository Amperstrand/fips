//! Length-Prefix Framing for RFCOMM Serial Transport
//!
//! RFCOMM is a byte stream (like TCP). This module provides simple
//! length-prefix framing: 2-byte big-endian length + payload.
//! Each FIPS packet is encoded as [len:2 BE][payload:len] on the wire.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum frame size (matches BLE MTU default).
pub const MAX_FRAME_SIZE: usize = 2048;

/// Size of the length prefix in bytes.
const LENGTH_PREFIX_SIZE: usize = 2;

/// Encode a packet with a 2-byte big-endian length prefix.
///
/// Returns `Vec<u8>` containing [len:2 BE][payload].
/// Returns an error if the payload exceeds `MAX_FRAME_SIZE`.
pub fn encode_packet(data: &[u8]) -> Result<Vec<u8>, FramingError> {
    if data.len() > MAX_FRAME_SIZE {
        return Err(FramingError::PayloadTooLarge {
            len: data.len(),
            max: MAX_FRAME_SIZE,
        });
    }
    let len = data.len() as u16;
    let mut buf = Vec::with_capacity(LENGTH_PREFIX_SIZE + data.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(data);
    Ok(buf)
}

/// Read one complete framed packet from an async reader.
///
/// Reads the 2-byte big-endian length prefix, then reads exactly
/// that many bytes of payload. Returns the payload (without prefix).
pub async fn read_framed_packet<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, FramingError> {
    // Read length prefix
    let mut prefix = [0u8; LENGTH_PREFIX_SIZE];
    reader.read_exact(&mut prefix).await?;
    let len = u16::from_be_bytes(prefix) as usize;

    if len > MAX_FRAME_SIZE {
        return Err(FramingError::PayloadTooLarge {
            len,
            max: MAX_FRAME_SIZE,
        });
    }

    if len == 0 {
        return Ok(Vec::new());
    }

    // Read payload
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Write one complete framed packet to an async writer.
///
/// Writes the 2-byte big-endian length prefix followed by the payload.
pub async fn write_framed_packet<W: AsyncWrite + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> Result<(), FramingError> {
    let framed = encode_packet(data)?;
    writer.write_all(&framed).await?;
    Ok(())
}

/// Errors from the framing layer.
#[derive(Debug)]
pub enum FramingError {
    /// Payload exceeds maximum frame size.
    PayloadTooLarge { len: usize, max: usize },
    /// I/O error (including EOF).
    Io(std::io::Error),
}

impl std::fmt::Display for FramingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FramingError::PayloadTooLarge { len, max } => {
                write!(f, "payload too large: {} > max {}", len, max)
            }
            FramingError::Io(e) => write!(f, "io: {}", e),
        }
    }
}

impl std::error::Error for FramingError {}

impl From<std::io::Error> for FramingError {
    fn from(e: std::io::Error) -> Self {
        FramingError::Io(e)
    }
}
