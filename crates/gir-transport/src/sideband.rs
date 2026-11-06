use std::io::{Read, Write};

use gir_core::GirError;

use crate::pktline::{PktLine, PktLineReader, PktLineWriter, MAX_PKT_PAYLOAD_LEN};

/// Multiplexed `side-band-64k` channel identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SidebandChannel {
    /// Band 1: Packfile binary stream or multiplexed protocol data.
    PackData = 1,
    /// Band 2: Human-readable progress messages from the remote server (`stderr`).
    Progress = 2,
    /// Band 3: Fatal error message from the remote server.
    FatalError = 3,
}

impl SidebandChannel {
    pub fn from_byte(b: u8) -> Result<Self, GirError> {
        match b {
            1 => Ok(Self::PackData),
            2 => Ok(Self::Progress),
            3 => Ok(Self::FatalError),
            other => Err(GirError::InvalidArgument(format!(
                "invalid sideband channel byte: 0x{other:02x}"
            ))),
        }
    }
}

/// Writes a payload chunk on `channel` into `writer`, splitting into `<= 65515` byte sideband frames.
pub fn write_sideband_frames<W: Write>(
    writer: &mut PktLineWriter<W>,
    channel: SidebandChannel,
    data: &[u8],
) -> Result<(), GirError> {
    let max_chunk = MAX_PKT_PAYLOAD_LEN - 1;
    if data.is_empty() {
        let frame = [channel as u8];
        return writer.write_data(&frame);
    }

    for chunk in data.chunks(max_chunk) {
        let mut frame = Vec::with_capacity(chunk.len() + 1);
        frame.push(channel as u8);
        frame.extend_from_slice(chunk);
        writer.write_data(&frame)?;
    }
    Ok(())
}

/// Demultiplexes a `side-band` / `side-band-64k` `pkt-line` stream until `PktLine::Flush` (or EOF),
/// invoking `on_progress` for Band 2 messages and returning the concatenated Band 1 bytes.
pub fn demux_sideband_stream<R: Read, F: FnMut(&str)>(
    reader: &mut PktLineReader<R>,
    mut on_progress: F,
) -> Result<Vec<u8>, GirError> {
    let mut band1_data = Vec::new();

    while let Some(pkt) = reader.read_pkt()? {
        match pkt {
            PktLine::Flush | PktLine::ResponseEnd => break,
            PktLine::Delimiter => continue,
            PktLine::Data(bytes) => {
                if bytes.is_empty() {
                    continue;
                }
                match SidebandChannel::from_byte(bytes[0])? {
                    SidebandChannel::PackData => {
                        band1_data.extend_from_slice(&bytes[1..]);
                    }
                    SidebandChannel::Progress => {
                        let msg = String::from_utf8_lossy(&bytes[1..]);
                        on_progress(&msg);
                    }
                    SidebandChannel::FatalError => {
                        let err_msg = String::from_utf8_lossy(&bytes[1..]).trim().to_string();
                        return Err(GirError::InvalidArgument(format!(
                            "remote error: {err_msg}"
                        )));
                    }
                }
            }
        }
    }

    Ok(band1_data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn sideband_mux_and_demux_roundtrip() {
        let mut wire = Vec::new();
        {
            let mut w = PktLineWriter::new(&mut wire);
            write_sideband_frames(&mut w, SidebandChannel::Progress, b"Counting objects: 5, done.\n")
                .unwrap();
            write_sideband_frames(&mut w, SidebandChannel::PackData, b"PACK\x00\x00\x00\x02")
                .unwrap();
            write_sideband_frames(&mut w, SidebandChannel::PackData, b"\x00\x00\x00\x00")
                .unwrap();
            w.write_flush().unwrap();
        }

        let mut progress_log = String::new();
        let mut r = PktLineReader::new(Cursor::new(wire));
        let pack = demux_sideband_stream(&mut r, |msg| progress_log.push_str(msg)).unwrap();

        assert_eq!(progress_log, "Counting objects: 5, done.\n");
        assert_eq!(pack, b"PACK\x00\x00\x00\x02\x00\x00\x00\x00");
    }
}
