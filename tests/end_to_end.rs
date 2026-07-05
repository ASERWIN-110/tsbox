use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Output};

fn tsbox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tsbox"))
}

fn assert_ok(output: Output) {
    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_fail(output: Output) {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_roundtrips_single_file_with_basename() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("report.json");
    let packed_dir = temp.path().join("packed");
    let extracted_dir = temp.path().join("extracted");
    fs::write(&source, br#"{"ok":true}"#).unwrap();

    assert_ok(
        tsbox()
            .args(["pack"])
            .arg(&source)
            .args(["-o"])
            .arg(&packed_dir)
            .output()
            .unwrap(),
    );
    let packed = packed_dir.join("report.ts");
    assert!(packed.exists());

    assert_ok(
        tsbox()
            .args(["extract"])
            .arg(&packed)
            .args(["-o"])
            .arg(&extracted_dir)
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(extracted_dir.join("report.json")).unwrap(),
        br#"{"ok":true}"#
    );
}

#[test]
fn renamed_ts_uses_current_basename_and_packed_extension() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("archive.tar.gz");
    let packed_dir = temp.path().join("packed");
    let extracted_dir = temp.path().join("out");
    fs::write(&source, b"compressed bytes").unwrap();

    assert_ok(
        tsbox()
            .args(["p"])
            .arg(&source)
            .args(["-o"])
            .arg(&packed_dir)
            .output()
            .unwrap(),
    );
    let renamed = packed_dir.join("renamed.ts");
    fs::rename(packed_dir.join("archive.tar.ts"), &renamed).unwrap();

    assert_ok(
        tsbox()
            .args(["x"])
            .arg(&renamed)
            .args(["-o"])
            .arg(&extracted_dir)
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(extracted_dir.join("renamed.gz")).unwrap(),
        b"compressed bytes"
    );
}

#[test]
fn batch_pack_and_extract_delete_each_successful_source() {
    let temp = tempfile::tempdir().unwrap();
    let input_dir = temp.path().join("input");
    let packed_dir = temp.path().join("packed");
    let extracted_dir = temp.path().join("extracted");
    fs::create_dir(&input_dir).unwrap();
    fs::write(input_dir.join("a.txt"), b"aaa").unwrap();
    fs::write(input_dir.join("b"), b"bbb").unwrap();

    assert_ok(
        tsbox()
            .args(["pack", "-d", "--jobs", "2"])
            .arg(&input_dir)
            .args(["-o"])
            .arg(&packed_dir)
            .output()
            .unwrap(),
    );
    assert!(!input_dir.join("a.txt").exists());
    assert!(!input_dir.join("b").exists());
    assert!(packed_dir.join("a.ts").exists());
    assert!(packed_dir.join("b.ts").exists());

    assert_ok(
        tsbox()
            .args(["extract", "-d", "--jobs", "2"])
            .arg(&packed_dir)
            .args(["-o"])
            .arg(&extracted_dir)
            .output()
            .unwrap(),
    );
    assert!(!packed_dir.join("a.ts").exists());
    assert!(!packed_dir.join("b.ts").exists());
    assert_eq!(fs::read(extracted_dir.join("a.txt")).unwrap(), b"aaa");
    assert_eq!(fs::read(extracted_dir.join("b")).unwrap(), b"bbb");
}

#[test]
fn empty_file_roundtrip_works() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("empty.bin");
    let packed_dir = temp.path().join("packed");
    let out_dir = temp.path().join("out");
    fs::write(&source, []).unwrap();

    assert_ok(
        tsbox()
            .args(["pack"])
            .arg(&source)
            .args(["-o"])
            .arg(&packed_dir)
            .output()
            .unwrap(),
    );
    assert_ok(
        tsbox()
            .args(["extract"])
            .arg(packed_dir.join("empty.ts"))
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );
    assert_eq!(fs::metadata(out_dir.join("empty.bin")).unwrap().len(), 0);
}

#[test]
fn output_collision_fails_without_deleting_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("a.bin");
    let existing = temp.path().join("a.ts");
    fs::write(&source, b"new").unwrap();
    fs::write(&existing, b"old").unwrap();

    assert_fail(tsbox().args(["pack", "-d"]).arg(&source).output().unwrap());
    assert_eq!(fs::read(&source).unwrap(), b"new");
    assert_eq!(fs::read(&existing).unwrap(), b"old");
}

#[test]
fn recursive_pack_detects_basename_collision_before_deleting_sources() {
    let temp = tempfile::tempdir().unwrap();
    let input_dir = temp.path().join("input");
    let nested = input_dir.join("nested");
    let out_dir = temp.path().join("out");
    fs::create_dir_all(&nested).unwrap();
    fs::write(input_dir.join("same.bin"), b"top").unwrap();
    fs::write(nested.join("same.bin"), b"nested").unwrap();

    let output = tsbox()
        .args(["pack", "-r", "-d"])
        .arg(&input_dir)
        .args(["-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_fail(output);
    assert_eq!(fs::read(input_dir.join("same.bin")).unwrap(), b"top");
    assert_eq!(fs::read(nested.join("same.bin")).unwrap(), b"nested");
    assert!(!out_dir.join("same.ts").exists());
}

#[test]
fn corrupted_tsbox_fails_without_deleting_source_or_final_output() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("data.bin");
    let packed_dir = temp.path().join("packed");
    let out_dir = temp.path().join("out");
    fs::write(&source, b"abcdef").unwrap();

    assert_ok(
        tsbox()
            .args(["pack"])
            .arg(&source)
            .args(["-o"])
            .arg(&packed_dir)
            .output()
            .unwrap(),
    );
    let packed = packed_dir.join("data.ts");
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&packed)
        .unwrap();
    file.seek(SeekFrom::Start(188 * 3 + 182)).unwrap();
    file.write_all(&[b'X']).unwrap();
    drop(file);

    assert_fail(
        tsbox()
            .args(["extract", "-d"])
            .arg(&packed)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );
    assert!(packed.exists());
    assert!(!out_dir.join("data.bin").exists());
}

#[test]
fn batch_extract_continues_after_failure_and_preserves_failed_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("ok.bin");
    let packed_dir = temp.path().join("packed");
    let out_dir = temp.path().join("out");
    fs::write(&source, b"ok").unwrap();

    assert_ok(
        tsbox()
            .args(["pack"])
            .arg(&source)
            .args(["-o"])
            .arg(&packed_dir)
            .output()
            .unwrap(),
    );
    let bad = packed_dir.join("bad.ts");
    fs::write(&bad, b"not a ts file").unwrap();

    let output = tsbox()
        .args(["extract", "-d"])
        .arg(&packed_dir)
        .args(["-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_fail(output);

    assert_eq!(fs::read(out_dir.join("ok.bin")).unwrap(), b"ok");
    assert!(!packed_dir.join("ok.ts").exists());
    assert!(bad.exists());
}

#[test]
fn stress_roundtrip_16_mib_file() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("large.dat");
    let packed_dir = temp.path().join("packed");
    let out_dir = temp.path().join("out");
    write_pattern_file(&source, 16 * 1024 * 1024);

    assert_ok(
        tsbox()
            .args(["pack"])
            .arg(&source)
            .args(["-o"])
            .arg(&packed_dir)
            .output()
            .unwrap(),
    );
    assert_ok(
        tsbox()
            .args(["extract"])
            .arg(packed_dir.join("large.ts"))
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );

    assert_eq!(
        fs::read(&source).unwrap(),
        fs::read(out_dir.join("large.dat")).unwrap()
    );
}

#[test]
fn raw_extracts_multi_program_streams_and_resyncs_after_junk() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("sample.ts");
    let out_dir = temp.path().join("raw");
    write_synthetic_multi_program_ts(&input_ts, true);

    assert_ok(
        tsbox()
            .args(["extract", "--raw"])
            .arg(&input_ts)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );

    assert_eq!(
        fs::read(out_dir.join("sample_p1_pid0100.h264")).unwrap(),
        b"\x00\x00\x01\x09h264-data"
    );
    assert_eq!(
        fs::read(out_dir.join("sample_p2_pid0101.aac")).unwrap(),
        b"\xFF\xF1aac-data"
    );
}

#[test]
fn batch_raw_extract_detects_output_collision_before_deleting_sources() {
    let temp = tempfile::tempdir().unwrap();
    let input_dir = temp.path().join("input");
    let nested = input_dir.join("nested");
    let out_dir = temp.path().join("raw");
    fs::create_dir_all(&nested).unwrap();
    write_synthetic_single_stream_ts(&input_dir.join("same.ts"));
    write_synthetic_single_stream_ts(&nested.join("same.ts"));

    let output = tsbox()
        .args(["extract", "--raw", "-r", "-d"])
        .arg(&input_dir)
        .args(["-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_fail(output);
    assert!(input_dir.join("same.ts").exists());
    assert!(nested.join("same.ts").exists());
    assert!(!out_dir.join("same.h264").exists());
}

#[test]
fn raw_extracts_single_h264_stream_with_basename() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("video.ts");
    let out_dir = temp.path().join("raw");
    write_synthetic_single_stream_ts(&input_ts);

    assert_ok(
        tsbox()
            .args(["extract", "--raw", "--quiet"])
            .arg(&input_ts)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );

    assert_eq!(
        fs::read(out_dir.join("video.h264")).unwrap(),
        b"\x00\x00\x01\x09single-h264"
    );
}

#[test]
fn fallback_raw_extracts_when_mp4_remux_fails() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("fallback.ts");
    let out_dir = temp.path().join("out");
    write_synthetic_single_stream_ts(&input_ts);

    assert_ok(
        tsbox()
            .args(["extract", "--fallback", "raw"])
            .arg(&input_ts)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );

    assert!(!out_dir.join("fallback.mp4").exists());
    assert_eq!(
        fs::read(out_dir.join("fallback.h264")).unwrap(),
        b"\x00\x00\x01\x09single-h264"
    );
}

#[test]
fn raw_extracts_m2ts_192_byte_packets() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("clip.m2ts");
    let out_dir = temp.path().join("raw");
    let mut ts = Vec::new();
    write_synthetic_single_stream_ts_bytes(&mut ts);
    fs::write(&input_ts, to_m2ts(&ts)).unwrap();

    assert_ok(
        tsbox()
            .args(["extract", "--raw"])
            .arg(&input_ts)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(out_dir.join("clip.h264")).unwrap(),
        b"\x00\x00\x01\x09single-h264"
    );
}

#[test]
fn raw_extracts_single_h265_stream_with_basename() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("hevc.ts");
    let out_dir = temp.path().join("raw");
    write_synthetic_one_stream_ts(&input_ts, 0x24, 0xE0, b"\x00\x00\x01\x26h265-data");

    assert_ok(
        tsbox()
            .args(["extract", "--raw", "--quiet"])
            .arg(&input_ts)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );

    assert_eq!(
        fs::read(out_dir.join("hevc.h265")).unwrap(),
        b"\x00\x00\x01\x26h265-data"
    );
}

#[test]
fn raw_extracts_additional_codecs() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("codecs.ts");
    let out_dir = temp.path().join("raw");
    write_synthetic_additional_codecs_ts(&input_ts);

    assert_ok(
        tsbox()
            .args(["extract", "--raw"])
            .arg(&input_ts)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );

    assert_eq!(
        fs::read(out_dir.join("codecs_p1_pid0100.m2v")).unwrap(),
        b"mpeg2"
    );
    assert_eq!(
        fs::read(out_dir.join("codecs_p1_pid0101.eac3")).unwrap(),
        b"eac3"
    );
    assert_eq!(
        fs::read(out_dir.join("codecs_p1_pid0102.dvbsub")).unwrap(),
        b"dvbsub"
    );
}

#[test]
fn raw_extract_rejects_continuity_counter_gap() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("gap.ts");
    let out_dir = temp.path().join("raw");
    write_synthetic_continuity_gap_ts(&input_ts);

    let output = tsbox()
        .args(["extract", "--raw", "-d"])
        .arg(&input_ts)
        .args(["-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_fail(output);
    assert!(input_ts.exists());
    assert!(!out_dir.join("gap.h264").exists());
}

#[test]
fn raw_extract_rejects_transport_error_indicator() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("tei.ts");
    let out_dir = temp.path().join("raw");
    write_synthetic_transport_error_ts(&input_ts);

    let output = tsbox()
        .args(["extract", "--raw", "-d"])
        .arg(&input_ts)
        .args(["-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_fail(output);
    assert!(input_ts.exists());
    assert!(!out_dir.join("tei.h264").exists());
}

#[test]
fn raw_extract_fails_when_stream_has_no_payload() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("empty_media.ts");
    let out_dir = temp.path().join("raw");
    write_synthetic_no_payload_ts(&input_ts);

    let output = tsbox()
        .args(["extract", "--raw", "-d"])
        .arg(&input_ts)
        .args(["-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_fail(output);
    assert!(input_ts.exists());
    assert!(!out_dir.join("empty_media.h264").exists());
}

#[test]
fn raw_extract_existing_output_collision_preserves_source() {
    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("video.ts");
    let out_dir = temp.path().join("raw");
    fs::create_dir(&out_dir).unwrap();
    fs::write(out_dir.join("video.h264"), b"existing").unwrap();
    write_synthetic_single_stream_ts(&input_ts);

    let output = tsbox()
        .args(["extract", "--raw", "-d"])
        .arg(&input_ts)
        .args(["-o"])
        .arg(&out_dir)
        .output()
        .unwrap();
    assert_fail(output);
    assert!(input_ts.exists());
    assert_eq!(fs::read(out_dir.join("video.h264")).unwrap(), b"existing");
}

#[test]
fn media_ts_remuxes_to_mp4_when_ffmpeg_can_generate_fixture() {
    if !ffmpeg_available() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let input_ts = temp.path().join("sample.ts");
    let out_dir = temp.path().join("out");
    let fixture = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc2=duration=0.2:size=64x64:rate=10")
        .arg("-c:v")
        .arg("libx264")
        .arg("-f")
        .arg("mpegts")
        .arg(&input_ts)
        .output()
        .unwrap();

    if !fixture.status.success() {
        return;
    }

    assert_ok(
        tsbox()
            .args(["extract"])
            .arg(&input_ts)
            .args(["-o"])
            .arg(&out_dir)
            .output()
            .unwrap(),
    );
    assert!(out_dir.join("sample.mp4").exists());
    assert!(fs::metadata(out_dir.join("sample.mp4")).unwrap().len() > 0);
}

fn write_pattern_file(path: &Path, len: usize) {
    let mut file = fs::File::create(path).unwrap();
    let mut written = 0usize;
    let mut block = vec![0u8; 64 * 1024];
    while written < len {
        for (idx, byte) in block.iter_mut().enumerate() {
            *byte = ((written + idx) % 251) as u8;
        }
        let take = block.len().min(len - written);
        file.write_all(&block[..take]).unwrap();
        written += take;
    }
}

fn write_synthetic_single_stream_ts(path: &Path) {
    let mut data = Vec::new();
    write_synthetic_single_stream_ts_bytes(&mut data);
    fs::write(path, data).unwrap();
}

fn write_synthetic_single_stream_ts_bytes(data: &mut Vec<u8>) {
    write_synthetic_one_stream_ts_bytes(data, 0x1B, 0xE0, b"\x00\x00\x01\x09single-h264");
}

fn write_synthetic_one_stream_ts(path: &Path, stream_type: u8, stream_id: u8, payload: &[u8]) {
    let mut data = Vec::new();
    write_synthetic_one_stream_ts_bytes(&mut data, stream_type, stream_id, payload);
    fs::write(path, data).unwrap();
}

fn write_synthetic_one_stream_ts_bytes(
    data: &mut Vec<u8>,
    stream_type: u8,
    stream_id: u8,
    payload: &[u8],
) {
    write_section_packet(data, 0, &pat_section(&[(1, 0x1000)]));
    write_section_packet(
        data,
        0x1000,
        &pmt_section(1, 0x0100, &[(stream_type, 0x0100)]),
    );
    write_pes_packet(data, 0x0100, stream_id, payload);
}

fn write_synthetic_multi_program_ts(path: &Path, insert_junk: bool) {
    let mut data = Vec::new();
    write_section_packet(&mut data, 0, &pat_section(&[(1, 0x1000), (2, 0x1001)]));
    write_section_packet(
        &mut data,
        0x1000,
        &pmt_section(1, 0x0100, &[(0x1B, 0x0100)]),
    );
    write_section_packet(
        &mut data,
        0x1001,
        &pmt_section(2, 0x0101, &[(0x0F, 0x0101)]),
    );
    if insert_junk {
        data.extend_from_slice(b"broken-sync");
    }
    write_pes_packet(&mut data, 0x0100, 0xE0, b"\x00\x00\x01\x09h264-data");
    write_pes_packet(&mut data, 0x0101, 0xC0, b"\xFF\xF1aac-data");
    fs::write(path, data).unwrap();
}

fn write_synthetic_no_payload_ts(path: &Path) {
    let mut data = Vec::new();
    write_section_packet(&mut data, 0, &pat_section(&[(1, 0x1000)]));
    write_section_packet(
        &mut data,
        0x1000,
        &pmt_section(1, 0x0100, &[(0x1B, 0x0100)]),
    );
    fs::write(path, data).unwrap();
}

fn write_synthetic_additional_codecs_ts(path: &Path) {
    let mut data = Vec::new();
    write_section_packet(&mut data, 0, &pat_section(&[(1, 0x1000)]));
    let streams = vec![
        StreamSpec {
            stream_type: 0x02,
            pid: 0x0100,
            descriptors: vec![],
        },
        StreamSpec {
            stream_type: 0x06,
            pid: 0x0101,
            descriptors: vec![0x7A, 0x00],
        },
        StreamSpec {
            stream_type: 0x06,
            pid: 0x0102,
            descriptors: vec![0x59, 0x00],
        },
    ];
    write_section_packet(
        &mut data,
        0x1000,
        &pmt_section_with_descriptors(1, 0x0100, &streams),
    );
    write_pes_packet(&mut data, 0x0100, 0xE0, b"mpeg2");
    write_pes_packet(&mut data, 0x0101, 0xBD, b"eac3");
    write_pes_packet(&mut data, 0x0102, 0xBD, b"dvbsub");
    fs::write(path, data).unwrap();
}

fn write_synthetic_continuity_gap_ts(path: &Path) {
    let mut data = Vec::new();
    write_section_packet(&mut data, 0, &pat_section(&[(1, 0x1000)]));
    write_section_packet(
        &mut data,
        0x1000,
        &pmt_section(1, 0x0100, &[(0x1B, 0x0100)]),
    );
    write_pes_packet_with_flags(&mut data, 0x0100, 0xE0, b"first", 0, false);
    write_pes_packet_with_flags(&mut data, 0x0100, 0xE0, b"second", 2, false);
    fs::write(path, data).unwrap();
}

fn write_synthetic_transport_error_ts(path: &Path) {
    let mut data = Vec::new();
    write_section_packet(&mut data, 0, &pat_section(&[(1, 0x1000)]));
    write_section_packet(
        &mut data,
        0x1000,
        &pmt_section(1, 0x0100, &[(0x1B, 0x0100)]),
    );
    write_pes_packet_with_flags(&mut data, 0x0100, 0xE0, b"bad", 0, true);
    fs::write(path, data).unwrap();
}

fn to_m2ts(ts: &[u8]) -> Vec<u8> {
    assert_eq!(ts.len() % 188, 0);
    let mut out = Vec::with_capacity(ts.len() / 188 * 192);
    for packet in ts.chunks_exact(188) {
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(packet);
    }
    out
}

fn write_section_packet(out: &mut Vec<u8>, pid: u16, section: &[u8]) {
    let mut packet = [0xFFu8; 188];
    packet[0] = 0x47;
    packet[1] = 0x40 | ((pid >> 8) as u8 & 0x1F);
    packet[2] = pid as u8;
    packet[3] = 0x10;
    packet[4] = 0;
    packet[5..5 + section.len()].copy_from_slice(section);
    out.extend_from_slice(&packet);
}

fn write_pes_packet(out: &mut Vec<u8>, pid: u16, stream_id: u8, payload: &[u8]) {
    write_pes_packet_with_flags(out, pid, stream_id, payload, 0, false);
}

fn write_pes_packet_with_flags(
    out: &mut Vec<u8>,
    pid: u16,
    stream_id: u8,
    payload: &[u8],
    continuity_counter: u8,
    transport_error: bool,
) {
    let mut pes = Vec::new();
    pes.extend_from_slice(&[0x00, 0x00, 0x01, stream_id, 0x00, 0x00, 0x80, 0x00, 0x00]);
    pes.extend_from_slice(payload);

    let mut packet = [0xFFu8; 188];
    packet[0] = 0x47;
    packet[1] = 0x40 | ((pid >> 8) as u8 & 0x1F);
    if transport_error {
        packet[1] |= 0x80;
    }
    packet[2] = pid as u8;
    packet[3] = 0x30 | (continuity_counter & 0x0F);
    let adaptation_len = 183 - pes.len();
    packet[4] = adaptation_len as u8;
    if adaptation_len > 0 {
        packet[5] = 0;
    }
    let start = 5 + adaptation_len;
    packet[start..start + pes.len()].copy_from_slice(&pes);
    out.extend_from_slice(&packet);
}

struct StreamSpec {
    stream_type: u8,
    pid: u16,
    descriptors: Vec<u8>,
}

fn pat_section(programs: &[(u16, u16)]) -> Vec<u8> {
    let section_len = 5 + programs.len() * 4 + 4;
    let mut section = vec![
        0x00,
        0xB0 | ((section_len >> 8) as u8 & 0x0F),
        section_len as u8,
        0x00,
        0x01,
        0xC1,
        0x00,
        0x00,
    ];
    for (program_number, pmt_pid) in programs {
        section.extend_from_slice(&program_number.to_be_bytes());
        section.push(0xE0 | ((pmt_pid >> 8) as u8 & 0x1F));
        section.push(*pmt_pid as u8);
    }
    section.extend_from_slice(&[0, 0, 0, 0]);
    section
}

fn pmt_section(program_number: u16, pcr_pid: u16, streams: &[(u8, u16)]) -> Vec<u8> {
    let specs = streams
        .iter()
        .map(|(stream_type, pid)| StreamSpec {
            stream_type: *stream_type,
            pid: *pid,
            descriptors: vec![],
        })
        .collect::<Vec<_>>();
    pmt_section_with_descriptors(program_number, pcr_pid, &specs)
}

fn pmt_section_with_descriptors(
    program_number: u16,
    pcr_pid: u16,
    streams: &[StreamSpec],
) -> Vec<u8> {
    let es_len = streams
        .iter()
        .map(|stream| 5 + stream.descriptors.len())
        .sum::<usize>();
    let section_len = 9 + es_len + 4;
    let mut section = vec![
        0x02,
        0xB0 | ((section_len >> 8) as u8 & 0x0F),
        section_len as u8,
    ];
    section.extend_from_slice(&program_number.to_be_bytes());
    section.extend_from_slice(&[
        0xC1,
        0x00,
        0x00,
        0xE0 | ((pcr_pid >> 8) as u8 & 0x1F),
        pcr_pid as u8,
        0xF0,
        0x00,
    ]);
    for stream in streams {
        section.push(stream.stream_type);
        section.push(0xE0 | ((stream.pid >> 8) as u8 & 0x1F));
        section.push(stream.pid as u8);
        section.push(0xF0 | ((stream.descriptors.len() >> 8) as u8 & 0x0F));
        section.push(stream.descriptors.len() as u8);
        section.extend_from_slice(&stream.descriptors);
    }
    section.extend_from_slice(&[0, 0, 0, 0]);
    section
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}
