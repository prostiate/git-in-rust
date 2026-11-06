use std::io::{self, Read, Write};

use gir_core::GirError;

/// Maximum total length of a single `pkt-line` frame (4-byte hex header + up to 65,516 payload bytes).
pub const MAX_PKT_LINE_LEN: usize = 65520;
/// Maximum payload bytes in a single `pkt-line` frame.
pub const MAX_PKT_PAYLOAD_LEN: usize = MAX_PKT_LINE_LEN - 4;

/// A single parsed Git `pkt-line` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PktLine {
    /// `0000` Flush packet (marks the end of a section or stream).
    Flush,
    /// `0001` Delimiter packet (used in protocol v2).
    Delimiter,
    /// `0002` Response-end packet.
    ResponseEnd,
    /// `0004..=fff0` Data packet containing `0..=65516` payload bytes.
    Data(Vec<u8>),
}

/// Encodes a payload slice into a 4-byte ASCII hexadecimal length-prefixed `pkt-line`.
pub fn encode_pkt_line(payload: &[u8]) -> Result<Vec<u8>, GirError> {
    if payload.len() > MAX_PKT_PAYLOAD_LEN {
        return Err(GirError::InvalidArgument(format!(
            "pkt-line payload length {} exceeds maximum {}",
            payload.len(),
            MAX_PKT_PAYLOAD_LEN
        )));
    }
    let total_len = payload.len() + 4;
    let mut out = Vec::with_capacity(total_len);
    write!(&mut out, "{total_len:04x}")?;
    out.extend_from_slice(payload);
    Ok(out)
}

#[inline]
pub const fn encode_flush_pkt() -> &'static [u8] {
    b"0000"
}

#[inline]
pub const fn encode_delim_pkt() -> &'static [u8] {
    b"0001"
}

/// Streaming `pkt-line` reader wrapping any `std::io::Read`.
pub struct PktLineReader<R> {
    inner: R,
}

impl<R: Read> PktLineReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Reads the next `PktLine` from the underlying stream, returning `Ok(None)` at clean EOF.
    pub fn read_pkt(&mut self) -> Result<Option<PktLine>, GirError> {
        let mut hdr = [0u8; 4];
        match read_exact_or_eof(&mut self.inner, &mut hdr)? {
            false => return Ok(None),
            true => {}
        }

        let hex_str = std::str::from_utf8(&hdr).map_err(|_| {
            GirError::InvalidArgument(format!("invalid non-ASCII pkt-line header: {hdr:?}"))
        })?;
        let len = usize::from_str_radix(hex_str, 16).map_err(|_| {
            GirError::InvalidArgument(format!("invalid hex in pkt-line header: '{hex_str}'"))
        })?;

        match len {
            0 => Ok(Some(PktLine::Flush)),
            1 => Ok(Some(PktLine::Delimiter)),
            2 => Ok(Some(PktLine::ResponseEnd)),
            3 => Err(GirError::InvalidArgument(
                "invalid pkt-line length 0003".to_string(),
            )),
            n if n > MAX_PKT_LINE_LEN => Err(GirError::InvalidArgument(format!(
                "pkt-line length {n} exceeds maximum {MAX_PKT_LINE_LEN}"
            ))),
            n => {
                let payload_len = n - 4;
                let mut payload = vec![0u8; payload_len];
                self.inner.read_exact(&mut payload)?;
                Ok(Some(PktLine::Data(payload)))
            }
        }
    }
}

/// Streaming `pkt-line` writer wrapping any `std::io::Write`.
pub struct PktLineWriter<W> {
    inner: W,
}

impl<W: Write> PktLineWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }

    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    pub fn write_data(&mut self, payload: &[u8]) -> Result<(), GirError> {
        let encoded = encode_pkt_line(payload)?;
        self.inner.write_all(&encoded)?;
        Ok(())
    }

    pub fn write_str(&mut self, line: &str) -> Result<(), GirError> {
        self.write_data(line.as_bytes())
    }

    pub fn write_flush(&mut self) -> Result<(), GirError> {
        self.inner.write_all(encode_flush_pkt())?;
        Ok(())
    }

    pub fn write_delim(&mut self) -> Result<(), GirError> {
        self.inner.write_all(encode_delim_pkt())?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), GirError> {
        self.inner.flush()?;
        Ok(())
    }
}

fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut read_total = 0;
    while read_total < buf.len() {
        match reader.read(&mut buf[read_total..]) {
            Ok(0) => {
                if read_total == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated pkt-line header",
                ));
            }
            Ok(n) => read_total += n,
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

    #[test]
    fn pktline_roundtrip_data_flush_and_delim() {
        let mut buf = Vec::new();
        {
            let mut writer = PktLineWriter::new(&mut buf);
            writer
                .write_str("want 0123456789abcdef0123456789abcdef01234567\n")
                .unwrap();
            writer.write_delim().unwrap();
            writer.write_data(b"").unwrap();
            writer.write_flush().unwrap();
        }

        assert!(buf.starts_with(b"0032want 0123456789abcdef0123456789abcdef01234567\n"));

        let mut reader = PktLineReader::new(Cursor::new(buf));
        assert_eq!(
            reader.read_pkt().unwrap(),
            Some(PktLine::Data(
                b"want 0123456789abcdef0123456789abcdef01234567\n".to_vec()
            ))
        );
        assert_eq!(reader.read_pkt().unwrap(), Some(PktLine::Delimiter));
        assert_eq!(reader.read_pkt().unwrap(), Some(PktLine::Data(Vec::new())));
        assert_eq!(reader.read_pkt().unwrap(), Some(PktLine::Flush));
        assert_eq!(reader.read_pkt().unwrap(), None);
    }

    #[test]
    fn pktline_max_65516_payload_roundtrip() {
        let big = vec![b'x'; MAX_PKT_PAYLOAD_LEN];
        let encoded = encode_pkt_line(&big).unwrap();
        assert_eq!(encoded.len(), MAX_PKT_LINE_LEN);
        assert_eq!(&encoded[..4], b"fff0");

        let mut reader = PktLineReader::new(Cursor::new(encoded));
        assert_eq!(reader.read_pkt().unwrap(), Some(PktLine::Data(big)));

        let too_big = vec![b'x'; MAX_PKT_PAYLOAD_LEN + 1];
        assert!(encode_pkt_line(&too_big).is_err());
    }
}
