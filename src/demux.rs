use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::paths;

const TS_PACKET_SIZE: usize = 188;
const M2TS_PACKET_SIZE: usize = 192;
const SYNC_BYTE: u8 = 0x47;
const PROBE_SIZE: usize = 1024 * 1024;

pub fn demux_raw(input: &Path, output_dir: &Path) -> Result<Vec<PathBuf>> {
    let plan = analyze(input)?;
    let base = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("input file name is not valid UTF-8: {}", input.display()))?;
    let targets = plan_output_paths(base, output_dir, &plan.streams)?;

    for target in targets.values() {
        paths::ensure_output_available(input, &target.final_path)?;
    }

    let mut writers = HashMap::new();
    for (pid, target) in &targets {
        paths::ensure_temp_available(&target.temp_path)?;
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&target.temp_path)
            .with_context(|| {
                format!(
                    "failed to create temp output {}",
                    target.temp_path.display()
                )
            })?;
        writers.insert(
            *pid,
            StreamWriter {
                writer: BufWriter::new(file),
                temp_path: target.temp_path.clone(),
                final_path: target.final_path.clone(),
                header_buffer: Vec::new(),
                awaiting_pes_header: false,
                started: false,
                bytes_written: 0,
                last_continuity_counter: None,
                pes_remaining: None,
            },
        );
    }

    let result = write_raw_streams(input, &mut writers);
    if let Err(err) = result.and_then(|_| validate_raw_outputs(&writers)) {
        for writer in writers.values() {
            let _ = std::fs::remove_file(&writer.temp_path);
        }
        return Err(err);
    }

    let mut outputs = Vec::with_capacity(writers.len());
    for (_, mut writer) in writers {
        writer
            .writer
            .flush()
            .with_context(|| format!("failed to flush {}", writer.temp_path.display()))?;
        paths::commit_temp(&writer.temp_path, &writer.final_path)?;
        outputs.push(writer.final_path);
    }
    outputs.sort();
    Ok(outputs)
}

pub fn plan_raw_outputs(input: &Path, output_dir: &Path) -> Result<Vec<PathBuf>> {
    let plan = analyze(input)?;
    let base = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("input file name is not valid UTF-8: {}", input.display()))?;
    let mut outputs = plan_output_paths(base, output_dir, &plan.streams)?
        .into_values()
        .map(|target| target.final_path)
        .collect::<Vec<_>>();
    outputs.sort();
    Ok(outputs)
}

fn analyze(input: &Path) -> Result<DemuxPlan> {
    let mut reader = PacketReader::open(input)?;
    let mut pat = SectionAssembler::default();
    let mut pmt_assemblers: HashMap<u16, SectionAssembler> = HashMap::new();
    let mut pmt_to_program: HashMap<u16, u16> = HashMap::new();
    let mut programs = BTreeMap::new();

    while let Some(packet) = reader.next_packet()? {
        let Some(payload) = packet.payload() else {
            continue;
        };

        if packet.pid == 0 {
            for section in pat.push(packet.payload_unit_start, payload)? {
                for (program_number, pmt_pid) in parse_pat(&section)? {
                    pmt_to_program.insert(pmt_pid, program_number);
                    pmt_assemblers.entry(pmt_pid).or_default();
                }
            }
            continue;
        }

        if let Some(assembler) = pmt_assemblers.get_mut(&packet.pid) {
            for section in assembler.push(packet.payload_unit_start, payload)? {
                let program_number = pmt_to_program
                    .get(&packet.pid)
                    .copied()
                    .unwrap_or_else(|| pmt_program_number(&section).unwrap_or(0));
                let streams = parse_pmt(program_number, &section)?;
                programs.insert(program_number, streams);
            }
        }
    }

    let mut streams = Vec::new();
    for program_streams in programs.values() {
        for stream in program_streams {
            if stream.kind.is_some() {
                streams.push(stream.clone());
            }
        }
    }

    if streams.is_empty() {
        bail!("no supported elementary streams found");
    }

    Ok(DemuxPlan { streams })
}

fn write_raw_streams(input: &Path, writers: &mut HashMap<u16, StreamWriter>) -> Result<()> {
    let mut reader = PacketReader::open(input)?;
    while let Some(packet) = reader.next_packet()? {
        let Some(writer) = writers.get_mut(&packet.pid) else {
            continue;
        };
        let Some(payload) = packet.payload() else {
            continue;
        };
        if !writer.accept_packet(&packet)? {
            continue;
        }

        if packet.payload_unit_start {
            writer.start_pes(payload)?;
        } else {
            writer.continue_pes(payload)?;
        }
    }
    Ok(())
}

fn plan_output_paths(
    base: &str,
    output_dir: &Path,
    streams: &[StreamInfo],
) -> Result<HashMap<u16, OutputTarget>> {
    let multi = streams.len() > 1;
    let mut paths_seen = HashMap::<PathBuf, u16>::new();
    let mut targets = HashMap::new();

    for stream in streams {
        let kind = stream.kind.expect("stream kind filtered before planning");
        let name = if multi {
            format!(
                "{}_p{}_pid{:04x}.{}",
                base,
                stream.program_number,
                stream.pid,
                kind.extension()
            )
        } else {
            format!("{}.{}", base, kind.extension())
        };
        let final_path = output_dir.join(name);
        if let Some(previous_pid) = paths_seen.insert(final_path.clone(), stream.pid) {
            bail!(
                "output name collision for PIDs 0x{previous_pid:04x} and 0x{:04x}: {}",
                stream.pid,
                final_path.display()
            );
        }
        let temp_path = paths::temp_path_for(&final_path)?;
        targets.insert(
            stream.pid,
            OutputTarget {
                final_path,
                temp_path,
            },
        );
    }

    Ok(targets)
}

#[derive(Debug)]
struct DemuxPlan {
    streams: Vec<StreamInfo>,
}

#[derive(Debug, Clone)]
struct StreamInfo {
    program_number: u16,
    pid: u16,
    kind: Option<StreamKind>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum StreamKind {
    Mpeg1Video,
    Mpeg2Video,
    H264,
    H265,
    Aac,
    Mp3,
    Ac3,
    EAc3,
    DvbSubtitle,
    Teletext,
    Lpcm,
}

impl StreamKind {
    fn from_stream_type(stream_type: u8, descriptors: &[u8]) -> Option<Self> {
        match stream_type {
            0x01 => Some(Self::Mpeg1Video),
            0x02 => Some(Self::Mpeg2Video),
            0x1B => Some(Self::H264),
            0x24 => Some(Self::H265),
            0x0F => Some(Self::Aac),
            0x03 | 0x04 => Some(Self::Mp3),
            0x81 => Some(Self::Ac3),
            0x87 => Some(Self::EAc3),
            0x80 => Some(Self::Lpcm),
            0x06 => infer_private_stream_kind(descriptors),
            _ => infer_private_stream_kind(descriptors),
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Mpeg1Video | Self::Mpeg2Video => "m2v",
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::Aac => "aac",
            Self::Mp3 => "mp3",
            Self::Ac3 => "ac3",
            Self::EAc3 => "eac3",
            Self::DvbSubtitle => "dvbsub",
            Self::Teletext => "teletext",
            Self::Lpcm => "lpcm",
        }
    }
}

fn infer_private_stream_kind(descriptors: &[u8]) -> Option<StreamKind> {
    let mut offset = 0usize;
    while offset + 2 <= descriptors.len() {
        let tag = descriptors[offset];
        let len = descriptors[offset + 1] as usize;
        let start = offset + 2;
        let end = start.saturating_add(len);
        if end > descriptors.len() {
            break;
        }
        match tag {
            0x56 => return Some(StreamKind::Teletext),
            0x59 => return Some(StreamKind::DvbSubtitle),
            0x6A => return Some(StreamKind::Ac3),
            0x7A => return Some(StreamKind::EAc3),
            _ => {}
        }
        offset = end;
    }
    None
}

#[derive(Debug)]
struct OutputTarget {
    final_path: PathBuf,
    temp_path: PathBuf,
}

struct StreamWriter {
    writer: BufWriter<File>,
    temp_path: PathBuf,
    final_path: PathBuf,
    header_buffer: Vec<u8>,
    awaiting_pes_header: bool,
    started: bool,
    bytes_written: u64,
    last_continuity_counter: Option<u8>,
    pes_remaining: Option<usize>,
}

impl StreamWriter {
    fn accept_packet(&mut self, packet: &TsPacket) -> Result<bool> {
        if packet.transport_error {
            bail!("PID 0x{:04x} has transport error indicator set", packet.pid);
        }

        let cc = packet.continuity_counter;
        if let Some(last) = self.last_continuity_counter {
            let expected = (last + 1) & 0x0F;
            if cc == last {
                return Ok(false);
            }
            if cc != expected {
                bail!(
                    "PID 0x{:04x} continuity counter mismatch: expected {expected}, got {cc}",
                    packet.pid
                );
            }
        }
        self.last_continuity_counter = Some(cc);
        Ok(true)
    }

    fn start_pes(&mut self, payload: &[u8]) -> Result<()> {
        if self.pes_remaining.is_some_and(|remaining| remaining > 0) {
            bail!("new PES packet started before previous PES declared length was complete");
        }
        self.header_buffer.clear();
        self.awaiting_pes_header = true;
        self.pes_remaining = None;
        self.feed_pes_header(payload)
    }

    fn continue_pes(&mut self, payload: &[u8]) -> Result<()> {
        if self.awaiting_pes_header {
            self.feed_pes_header(payload)
        } else if self.started {
            self.write_elementary_payload(payload)
        } else {
            Ok(())
        }
    }

    fn feed_pes_header(&mut self, payload: &[u8]) -> Result<()> {
        self.header_buffer.extend_from_slice(payload);
        if self.header_buffer.len() < 9 {
            return Ok(());
        }
        if self.header_buffer[0] != 0 || self.header_buffer[1] != 0 || self.header_buffer[2] != 1 {
            bail!("invalid PES start code");
        }

        let pes_packet_length =
            u16::from_be_bytes([self.header_buffer[4], self.header_buffer[5]]) as usize;
        let header_len = 9 + self.header_buffer[8] as usize;
        if self.header_buffer.len() < header_len {
            return Ok(());
        }
        if pes_packet_length > 0 {
            let bytes_after_length = 3 + self.header_buffer[8] as usize;
            if pes_packet_length < bytes_after_length {
                bail!("invalid PES packet length");
            }
            self.pes_remaining = Some(pes_packet_length - bytes_after_length);
        }

        let payload = self.header_buffer[header_len..].to_vec();
        self.write_elementary_payload(&payload)?;
        self.header_buffer.clear();
        self.awaiting_pes_header = false;
        self.started = true;
        Ok(())
    }

    fn write_elementary_payload(&mut self, payload: &[u8]) -> Result<()> {
        let writable = match self.pes_remaining {
            Some(remaining) => remaining.min(payload.len()),
            None => payload.len(),
        };
        if writable > 0 {
            self.writer.write_all(&payload[..writable])?;
            self.bytes_written += writable as u64;
        }
        if let Some(remaining) = &mut self.pes_remaining {
            *remaining -= writable;
        }
        Ok(())
    }
}

fn validate_raw_outputs(writers: &HashMap<u16, StreamWriter>) -> Result<()> {
    for (pid, writer) in writers {
        if writer.awaiting_pes_header {
            bail!("PID 0x{pid:04x} ended before a complete PES header");
        }
        if writer.bytes_written == 0 {
            bail!(
                "PID 0x{pid:04x} produced no elementary stream payload; refusing empty output {}",
                writer.final_path.display()
            );
        }
    }
    Ok(())
}

#[derive(Default)]
struct SectionAssembler {
    buffer: Vec<u8>,
}

impl SectionAssembler {
    fn push(&mut self, payload_unit_start: bool, payload: &[u8]) -> Result<Vec<Vec<u8>>> {
        if payload.is_empty() {
            return Ok(Vec::new());
        }

        let append_from = if payload_unit_start {
            let pointer = payload[0] as usize;
            if payload.len() < 1 + pointer {
                self.buffer.clear();
                return Ok(Vec::new());
            }
            self.buffer.clear();
            1 + pointer
        } else {
            0
        };

        self.buffer.extend_from_slice(&payload[append_from..]);
        let mut sections = Vec::new();
        loop {
            if self.buffer.len() < 3 {
                break;
            }
            if self.buffer[0] == 0xFF {
                self.buffer.clear();
                break;
            }
            let section_len = (((self.buffer[1] & 0x0F) as usize) << 8) | self.buffer[2] as usize;
            let total_len = 3 + section_len;
            if section_len < 4 || total_len > 4096 {
                self.buffer.clear();
                bail!("invalid PSI section length: {section_len}");
            }
            if self.buffer.len() < total_len {
                break;
            }

            let section = self.buffer[..total_len].to_vec();
            self.buffer.drain(..total_len);
            sections.push(section);
        }
        Ok(sections)
    }
}

fn parse_pat(section: &[u8]) -> Result<Vec<(u16, u16)>> {
    if section.len() < 12 || section[0] != 0x00 {
        return Ok(Vec::new());
    }

    let section_len = (((section[1] & 0x0F) as usize) << 8) | section[2] as usize;
    let total_len = 3 + section_len;
    if total_len > section.len() || total_len < 12 {
        bail!("invalid PAT section length");
    }

    let mut out = Vec::new();
    let mut offset = 8;
    let end = total_len - 4;
    while offset + 4 <= end {
        let program_number = u16::from_be_bytes([section[offset], section[offset + 1]]);
        let pid = (((section[offset + 2] & 0x1F) as u16) << 8) | section[offset + 3] as u16;
        if program_number != 0 {
            out.push((program_number, pid));
        }
        offset += 4;
    }
    Ok(out)
}

fn pmt_program_number(section: &[u8]) -> Option<u16> {
    if section.len() >= 5 && section[0] == 0x02 {
        Some(u16::from_be_bytes([section[3], section[4]]))
    } else {
        None
    }
}

fn parse_pmt(program_number: u16, section: &[u8]) -> Result<Vec<StreamInfo>> {
    if section.len() < 16 || section[0] != 0x02 {
        return Ok(Vec::new());
    }

    let section_len = (((section[1] & 0x0F) as usize) << 8) | section[2] as usize;
    let total_len = 3 + section_len;
    if total_len > section.len() || total_len < 16 {
        bail!("invalid PMT section length");
    }

    let program_info_len = (((section[10] & 0x0F) as usize) << 8) | section[11] as usize;
    let mut offset = 12 + program_info_len;
    let end = total_len - 4;
    let mut streams = Vec::new();

    while offset + 5 <= end {
        let stream_type = section[offset];
        let pid = (((section[offset + 1] & 0x1F) as u16) << 8) | section[offset + 2] as u16;
        let es_info_len =
            (((section[offset + 3] & 0x0F) as usize) << 8) | section[offset + 4] as usize;
        if offset + 5 + es_info_len > end {
            bail!("invalid PMT ES descriptor length");
        }
        let descriptors = &section[offset + 5..offset + 5 + es_info_len];
        streams.push(StreamInfo {
            program_number,
            pid,
            kind: StreamKind::from_stream_type(stream_type, descriptors),
        });
        offset += 5 + es_info_len;
    }

    Ok(streams)
}

#[derive(Debug)]
struct TsPacket {
    pid: u16,
    payload_unit_start: bool,
    transport_error: bool,
    continuity_counter: u8,
    bytes: [u8; TS_PACKET_SIZE],
}

impl TsPacket {
    fn payload(&self) -> Option<&[u8]> {
        let adaptation_control = (self.bytes[3] >> 4) & 0x03;
        match adaptation_control {
            0 | 2 => None,
            1 => Some(&self.bytes[4..]),
            3 => {
                let adaptation_len = self.bytes[4] as usize;
                let start = 5 + adaptation_len;
                if start > TS_PACKET_SIZE {
                    None
                } else {
                    Some(&self.bytes[start..])
                }
            }
            _ => None,
        }
    }
}

struct PacketReader {
    reader: BufReader<File>,
    packet_size: usize,
    sync_offset: usize,
}

impl PacketReader {
    fn open(input: &Path) -> Result<Self> {
        let mut file = File::open(input)
            .with_context(|| format!("failed to open TS input {}", input.display()))?;
        let (packet_size, sync_offset, start) = detect_layout(&mut file)
            .with_context(|| format!("failed to detect TS packet layout in {}", input.display()))?;
        file.seek(SeekFrom::Start(start as u64))?;
        Ok(Self {
            reader: BufReader::new(file),
            packet_size,
            sync_offset,
        })
    }

    fn next_packet(&mut self) -> Result<Option<TsPacket>> {
        let mut raw = vec![0u8; self.packet_size];
        loop {
            let start_pos = self.reader.stream_position()?;
            match self.reader.read_exact(&mut raw) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => return Err(err).context("failed to read TS packet"),
            }

            if raw[self.sync_offset] == SYNC_BYTE {
                let mut bytes = [0u8; TS_PACKET_SIZE];
                if self.packet_size == M2TS_PACKET_SIZE {
                    bytes.copy_from_slice(&raw[4..]);
                } else {
                    bytes.copy_from_slice(&raw);
                }

                let pid = (((bytes[1] & 0x1F) as u16) << 8) | bytes[2] as u16;
                return Ok(Some(TsPacket {
                    pid,
                    payload_unit_start: bytes[1] & 0x40 != 0,
                    transport_error: bytes[1] & 0x80 != 0,
                    continuity_counter: bytes[3] & 0x0F,
                    bytes,
                }));
            }

            if !self.resync_from(start_pos + 1)? {
                return Ok(None);
            }
        }
    }

    fn resync_from(&mut self, from: u64) -> Result<bool> {
        self.reader.seek(SeekFrom::Start(from))?;
        let mut probe = vec![0u8; PROBE_SIZE];
        let mut absolute = from;

        loop {
            let read = self.reader.read(&mut probe)?;
            if read < self.sync_offset + 1 {
                return Ok(false);
            }

            let usable_end = read.saturating_sub(self.packet_size + self.sync_offset);
            for idx in 0..=usable_end {
                if sync_chain_at(&probe[..read], idx, self.packet_size, self.sync_offset, 2) {
                    self.reader.seek(SeekFrom::Start(absolute + idx as u64))?;
                    return Ok(true);
                }
            }

            if read < probe.len() {
                return Ok(false);
            }

            let overlap = self.packet_size + self.sync_offset;
            absolute += (read - overlap) as u64;
            self.reader.seek(SeekFrom::Start(absolute))?;
        }
    }
}

fn detect_layout(file: &mut File) -> Result<(usize, usize, usize)> {
    file.seek(SeekFrom::Start(0))?;
    let mut probe = vec![0u8; PROBE_SIZE];
    let read = file.read(&mut probe)?;
    if read == 0 {
        bail!("empty input");
    }
    probe.truncate(read);

    for (packet_size, sync_offset) in [(TS_PACKET_SIZE, 0usize), (M2TS_PACKET_SIZE, 4usize)] {
        let limit = read.saturating_sub(packet_size * 2 + sync_offset);
        for start in 0..=limit {
            if sync_chain_at(&probe, start, packet_size, sync_offset, 3) {
                return Ok((packet_size, sync_offset, start));
            }
        }
    }

    bail!("input does not look like 188-byte TS or 192-byte M2TS");
}

fn sync_chain_at(
    buf: &[u8],
    start: usize,
    packet_size: usize,
    sync_offset: usize,
    count: usize,
) -> bool {
    (0..count).all(|idx| {
        let pos = start + sync_offset + idx * packet_size;
        pos < buf.len() && buf[pos] == SYNC_BYTE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pat_with_multiple_programs() {
        let section = vec![
            0x00, 0xB0, 0x11, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00, 0x00, 0x02,
            0xE1, 0x01, 0, 0, 0, 0,
        ];
        let programs = parse_pat(&section).unwrap();
        assert_eq!(programs, vec![(1, 0x0100), (2, 0x0101)]);
    }

    #[test]
    fn parses_supported_pmt_streams() {
        let section = vec![
            0x02, 0xB0, 0x17, 0x00, 0x01, 0xC1, 0x00, 0x00, 0xE1, 0x00, 0xF0, 0x00, 0x1B, 0xE1,
            0x00, 0xF0, 0x00, 0x0F, 0xE1, 0x01, 0xF0, 0x00, 0, 0, 0, 0,
        ];
        let streams = parse_pmt(1, &section).unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].pid, 0x0100);
        assert_eq!(streams[0].kind, Some(StreamKind::H264));
        assert_eq!(streams[1].pid, 0x0101);
        assert_eq!(streams[1].kind, Some(StreamKind::Aac));
    }
}
