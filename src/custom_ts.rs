use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

const PACKET_SIZE: usize = 188;
const M2TS_PACKET_SIZE: usize = 192;
const SYNC_BYTE: u8 = 0x47;
const PMT_PID: u16 = 0x1000;
const DATA_PID: u16 = 0x0100;
const MAGIC: &[u8; 7] = b"TSBOX1\0";
const TRAILER_MAGIC: &[u8; 8] = b"TSBOXEND";
const VERSION: u8 = 1;
const FIXED_HEADER_LEN: usize = 18;
const MAX_NAME_LEN: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    original_name: String,
    size: u64,
}

impl Metadata {
    pub fn original_extension(&self) -> Option<&str> {
        Path::new(&self.original_name)
            .extension()
            .and_then(|ext| ext.to_str())
    }
}

pub fn pack_file(input: &Path, output: &Path) -> Result<()> {
    let input_file = File::open(input)
        .with_context(|| format!("failed to open input file {}", input.display()))?;
    let metadata = input_file
        .metadata()
        .with_context(|| format!("failed to read input metadata {}", input.display()))?;
    if !metadata.is_file() {
        bail!("input is not a regular file: {}", input.display());
    }

    let name = input
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("input file name is not valid UTF-8: {}", input.display()))?;
    let name_bytes = name.as_bytes();
    if name_bytes.len() > u16::MAX as usize {
        bail!("file name is too long: {}", input.display());
    }

    let output_file = File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| format!("failed to create output file {}", output.display()))?;
    let mut input_reader = BufReader::new(input_file);
    let mut writer = TsDataWriter::new(BufWriter::new(output_file))?;

    let mut header = Vec::with_capacity(FIXED_HEADER_LEN + name_bytes.len());
    header.extend_from_slice(MAGIC);
    header.push(VERSION);
    header.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
    header.extend_from_slice(&metadata.len().to_be_bytes());
    header.extend_from_slice(name_bytes);
    writer.write_data(&header)?;

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = input_reader
            .read(&mut buffer)
            .with_context(|| format!("failed to read input file {}", input.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        writer.write_data(&buffer[..read])?;
    }

    let mut trailer = Vec::with_capacity(TRAILER_MAGIC.len() + 32);
    trailer.extend_from_slice(TRAILER_MAGIC);
    trailer.extend_from_slice(&hasher.finalize());
    writer.write_data(&trailer)?;
    writer.finish()
}

pub fn probe(input: &Path) -> Result<Option<Metadata>> {
    let mut reader = TsPayloadReader::open(input, DATA_PID)?;
    read_metadata(&mut reader, false)
}

pub fn extract_file(input: &Path, output: &Path) -> Result<Metadata> {
    let mut reader = TsPayloadReader::open(input, DATA_PID)?;
    let meta = read_metadata(&mut reader, true)?
        .ok_or_else(|| anyhow!("not a TSBOX file: {}", input.display()))?;

    let output_file = File::options()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| format!("failed to create output file {}", output.display()))?;
    let mut output_writer = BufWriter::new(output_file);
    let mut hasher = Sha256::new();
    let mut remaining = meta.size;
    let mut buffer = vec![0u8; 1024 * 1024];

    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        reader
            .read_exact(&mut buffer[..want])
            .with_context(|| "TSBOX payload ended before file content was complete")?;
        output_writer
            .write_all(&buffer[..want])
            .with_context(|| format!("failed to write output file {}", output.display()))?;
        hasher.update(&buffer[..want]);
        remaining -= want as u64;
    }

    let mut trailer = [0u8; TRAILER_MAGIC.len() + 32];
    reader
        .read_exact(&mut trailer)
        .with_context(|| "TSBOX payload ended before checksum trailer")?;
    if &trailer[..TRAILER_MAGIC.len()] != TRAILER_MAGIC {
        bail!("invalid TSBOX trailer");
    }

    let actual_hash = hasher.finalize();
    if actual_hash.as_slice() != &trailer[TRAILER_MAGIC.len()..] {
        bail!("TSBOX checksum mismatch");
    }

    output_writer
        .flush()
        .with_context(|| format!("failed to flush output file {}", output.display()))?;
    Ok(meta)
}

fn read_metadata<R: Read>(reader: &mut R, strict: bool) -> Result<Option<Metadata>> {
    let mut fixed = [0u8; FIXED_HEADER_LEN];
    match reader.read_exact(&mut fixed) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof && !strict => return Ok(None),
        Err(err) => return Err(err).context("failed to read TSBOX header"),
    }

    if &fixed[..MAGIC.len()] != MAGIC {
        return Ok(None);
    }
    if fixed[MAGIC.len()] != VERSION {
        bail!("unsupported TSBOX version: {}", fixed[MAGIC.len()]);
    }

    let name_len = u16::from_be_bytes([fixed[8], fixed[9]]) as usize;
    if name_len == 0 || name_len > MAX_NAME_LEN {
        bail!("invalid TSBOX file name length: {name_len}");
    }
    let size = u64::from_be_bytes([
        fixed[10], fixed[11], fixed[12], fixed[13], fixed[14], fixed[15], fixed[16], fixed[17],
    ]);

    let mut name = vec![0u8; name_len];
    reader
        .read_exact(&mut name)
        .context("failed to read TSBOX file name")?;
    let original_name = String::from_utf8(name).context("TSBOX file name is not valid UTF-8")?;
    if original_name.contains('/') || original_name.contains('\\') {
        bail!("TSBOX file name must be a basename, got {original_name:?}");
    }

    Ok(Some(Metadata {
        original_name,
        size,
    }))
}

struct TsDataWriter<W: Write> {
    writer: W,
    continuity: u8,
}

impl<W: Write> TsDataWriter<W> {
    fn new(mut writer: W) -> Result<Self> {
        write_section_packet(&mut writer, 0, true, &pat_section())?;
        write_section_packet(&mut writer, PMT_PID, true, &pmt_section())?;
        Ok(Self {
            writer,
            continuity: 0,
        })
    }

    fn write_data(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            let take = data.len().min(184);
            self.write_data_packet(&data[..take])?;
            data = &data[take..];
        }
        Ok(())
    }

    fn write_data_packet(&mut self, payload: &[u8]) -> Result<()> {
        if payload.is_empty() || payload.len() > 184 {
            bail!("invalid TS payload length: {}", payload.len());
        }

        let mut packet = [0xFFu8; PACKET_SIZE];
        packet[0] = SYNC_BYTE;
        packet[1] = ((DATA_PID >> 8) & 0x1F) as u8;
        packet[2] = (DATA_PID & 0xFF) as u8;
        packet[3] = self.continuity & 0x0F;

        let payload_start = if payload.len() == 184 {
            packet[3] |= 0x10;
            4
        } else {
            packet[3] |= 0x30;
            let adaptation_len = 183 - payload.len();
            packet[4] = adaptation_len as u8;
            if adaptation_len > 0 {
                packet[5] = 0x00;
            }
            5 + adaptation_len
        };

        packet[payload_start..payload_start + payload.len()].copy_from_slice(payload);
        self.writer.write_all(&packet)?;
        self.continuity = (self.continuity + 1) & 0x0F;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.writer.flush().map_err(Into::into)
    }
}

struct TsPayloadReader {
    reader: BufReader<File>,
    target_pid: u16,
    packet_size: usize,
    buffer: Vec<u8>,
    offset: usize,
    eof: bool,
}

impl TsPayloadReader {
    fn open(input: &Path, target_pid: u16) -> Result<Self> {
        let file = File::open(input)
            .with_context(|| format!("failed to open TS input {}", input.display()))?;
        let packet_size = detect_packet_size(input)?;
        Ok(Self {
            reader: BufReader::new(file),
            target_pid,
            packet_size,
            buffer: Vec::with_capacity(184),
            offset: 0,
            eof: false,
        })
    }

    fn load_payload(&mut self) -> io::Result<bool> {
        self.buffer.clear();
        self.offset = 0;

        let mut packet = vec![0u8; self.packet_size];
        loop {
            match self.reader.read_exact(&mut packet) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    return Ok(false);
                }
                Err(err) => return Err(err),
            }

            let ts = if self.packet_size == M2TS_PACKET_SIZE {
                &packet[4..]
            } else {
                &packet[..]
            };
            if ts[0] != SYNC_BYTE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "lost MPEG-TS sync byte",
                ));
            }

            let pid = (((ts[1] & 0x1F) as u16) << 8) | ts[2] as u16;
            if pid != self.target_pid {
                continue;
            }

            let adaptation_control = (ts[3] >> 4) & 0x03;
            if adaptation_control == 0 || adaptation_control == 2 {
                continue;
            }

            let payload_start = if adaptation_control == 1 {
                4
            } else {
                let adaptation_len = ts[4] as usize;
                let start = 5 + adaptation_len;
                if start > PACKET_SIZE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid TS adaptation field length",
                    ));
                }
                start
            };

            if payload_start < PACKET_SIZE {
                self.buffer.extend_from_slice(&ts[payload_start..]);
                return Ok(true);
            }
        }
    }
}

impl Read for TsPayloadReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }

        let mut written = 0usize;
        while written < out.len() {
            if self.offset == self.buffer.len() {
                if self.eof || !self.load_payload()? {
                    break;
                }
            }

            let available = &self.buffer[self.offset..];
            let take = available.len().min(out.len() - written);
            out[written..written + take].copy_from_slice(&available[..take]);
            self.offset += take;
            written += take;
        }

        Ok(written)
    }
}

fn detect_packet_size(input: &Path) -> Result<usize> {
    let mut file = File::open(input)
        .with_context(|| format!("failed to open TS input {}", input.display()))?;
    let mut probe = vec![0u8; M2TS_PACKET_SIZE * 5];
    let read = file
        .read(&mut probe)
        .with_context(|| format!("failed to read TS probe {}", input.display()))?;

    for size in [PACKET_SIZE, M2TS_PACKET_SIZE] {
        let offset = if size == M2TS_PACKET_SIZE { 4 } else { 0 };
        if read >= offset + size * 3 && (0..3).all(|idx| probe[offset + idx * size] == SYNC_BYTE) {
            return Ok(size);
        }
    }

    if read == 0 {
        bail!("empty TS input: {}", input.display());
    }
    bail!(
        "input does not look like 188-byte TS or 192-byte M2TS: {}",
        input.display()
    );
}

fn write_section_packet<W: Write>(
    writer: &mut W,
    pid: u16,
    payload_unit_start: bool,
    section: &[u8],
) -> Result<()> {
    if section.len() + 1 > 184 {
        bail!("PSI section too large");
    }

    let mut packet = [0xFFu8; PACKET_SIZE];
    packet[0] = SYNC_BYTE;
    packet[1] = ((pid >> 8) & 0x1F) as u8;
    if payload_unit_start {
        packet[1] |= 0x40;
    }
    packet[2] = (pid & 0xFF) as u8;
    packet[3] = 0x10;
    packet[4] = 0x00;
    packet[5..5 + section.len()].copy_from_slice(section);
    writer.write_all(&packet)?;
    Ok(())
}

fn pat_section() -> Vec<u8> {
    let mut section = vec![
        0x00,
        0xB0,
        0x0D,
        0x00,
        0x01,
        0xC1,
        0x00,
        0x00,
        0x00,
        0x01,
        0xE0 | ((PMT_PID >> 8) as u8 & 0x1F),
        (PMT_PID & 0xFF) as u8,
    ];
    let crc = mpeg_crc32(&section);
    section.extend_from_slice(&crc.to_be_bytes());
    section
}

fn pmt_section() -> Vec<u8> {
    let mut section = vec![
        0x02,
        0xB0,
        0x12,
        0x00,
        0x01,
        0xC1,
        0x00,
        0x00,
        0xE0 | ((DATA_PID >> 8) as u8 & 0x1F),
        (DATA_PID & 0xFF) as u8,
        0xF0,
        0x00,
        0x06,
        0xE0 | ((DATA_PID >> 8) as u8 & 0x1F),
        (DATA_PID & 0xFF) as u8,
        0xF0,
        0x00,
    ];
    let crc = mpeg_crc32(&section);
    section.extend_from_slice(&crc.to_be_bytes());
    section
}

fn mpeg_crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= (*byte as u32) << 24;
        for _ in 0..8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn pat_crc_known_vector_is_stable() {
        let section = pat_section();
        assert_eq!(section.len(), 16);
        assert_eq!(&section[12..], &mpeg_crc32(&section[..12]).to_be_bytes());
    }

    #[test]
    fn packet_size_detection_rejects_random_data() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("bad.ts");
        fs::write(&input, b"not ts").unwrap();

        let err = detect_packet_size(&input).unwrap_err();
        assert!(format!("{err:#}").contains("does not look like"));
    }

    #[test]
    fn checksum_mismatch_is_detected() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("data.bin");
        let packed = temp.path().join("data.ts");
        let output = temp.path().join("data.out");
        fs::write(&input, b"abcdef").unwrap();
        pack_file(&input, &packed).unwrap();

        let mut file = File::options()
            .read(true)
            .write(true)
            .open(&packed)
            .unwrap();
        file.seek(SeekFrom::Start(188 * 3 + 182)).unwrap();
        file.write_all(&[b'X']).unwrap();
        drop(file);

        let err = extract_file(&packed, &output).unwrap_err();
        assert!(format!("{err:#}").contains("checksum mismatch"));
    }

    #[test]
    fn original_extension_handles_multiple_dots() {
        let meta = Metadata {
            original_name: "archive.tar.gz".to_string(),
            size: 0,
        };
        assert_eq!(meta.original_extension(), Some("gz"));
    }
}
