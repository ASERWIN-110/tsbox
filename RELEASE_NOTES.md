# tsbox v0.1.0

## Highlights

- Pack arbitrary files into valid MPEG-TS private streams.
- Extract TSBOX files back to their original extension using basename output rules.
- Remux media TS to MP4 through ffmpeg without re-encoding.
- Export raw elementary streams with the built-in Rust demuxer.
- Support H.264, H.265, AAC, MP3, AC3, E-AC3, MPEG video, DVB subtitles, teletext, and LPCM raw output.
- Handle multi-program TS, 188-byte TS, and 192-byte M2TS.
- Recover from damaged sync regions where a later valid TS sync chain exists.
- Detect output collisions, empty raw streams, transport errors, and continuity counter gaps.
- Support batch processing, recursive input, per-file deletion after successful commit, progress output, and concurrency control.

## Artifacts

The local `scripts/build_release.sh` script creates artifacts in `dist/`.

The GitHub Actions release workflow builds:

- `x86_64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
