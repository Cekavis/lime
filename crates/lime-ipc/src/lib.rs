//! Shared framing primitives for Lime's local IPC transports.

use std::io::{self, Read, Write};

pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

pub fn read_json<R: Read, T: serde::de::DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let mut len = [0; 4];
    reader.read_exact(&mut len)?;
    let length = u32::from_le_bytes(len) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame too large",
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn write_json<W: Write, T: serde::Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "IPC frame too large"))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame too large",
        ));
    }
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_uses_little_endian_length_prefix() {
        let mut bytes = Vec::new();
        write_json(&mut bytes, &serde_json::json!({ "ok": true })).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize,
            bytes.len() - 4
        );
        let decoded: serde_json::Value = read_json(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded["ok"], true);
    }
}
