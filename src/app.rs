use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::custom_ts;
use crate::demux;
use crate::media;
use crate::paths;

#[derive(Debug, Parser)]
#[command(name = "tsbox")]
#[command(about = "Pack arbitrary files into MPEG-TS files and extract TSBOX/media TS files")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(alias = "p", about = "Pack file(s) into TSBOX .ts files")]
    Pack(PackArgs),
    #[command(
        alias = "x",
        about = "Extract TSBOX .ts files or remux media TS to MP4"
    )]
    Extract(ExtractArgs),
}

#[derive(Debug, Args)]
struct PackArgs {
    /// File or directory to pack.
    input: PathBuf,

    /// Output file for a single input, or output directory for batch input.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Delete each source file only after its .ts output is successfully committed.
    #[arg(short = 'd', long)]
    delete_source: bool,

    /// Recurse into input directories.
    #[arg(short, long)]
    recursive: bool,

    /// Number of files to process concurrently.
    #[arg(short = 'j', long, default_value_t = 1)]
    jobs: usize,

    /// Suppress per-file progress output.
    #[arg(short, long)]
    quiet: bool,
}

#[derive(Debug, Args)]
struct ExtractArgs {
    /// .ts file or directory of .ts files to extract.
    input: PathBuf,

    /// Output file for a single input, or output directory for batch input.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Delete each source .ts file only after its output is successfully committed.
    #[arg(short = 'd', long)]
    delete_source: bool,

    /// Recurse into input directories.
    #[arg(short, long)]
    recursive: bool,

    /// Export raw H.264/H.265/AAC/MP3/AC3 streams with the built-in Rust demuxer.
    #[arg(long)]
    raw: bool,

    /// Fallback behavior when normal MP4 remux fails.
    #[arg(long, value_enum, default_value_t = FallbackMode::None)]
    fallback: FallbackMode,

    /// Number of files to process concurrently.
    #[arg(short = 'j', long, default_value_t = 1)]
    jobs: usize,

    /// Suppress per-file progress output.
    #[arg(short, long)]
    quiet: bool,
}

pub fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Pack(args) => pack(args),
        Command::Extract(args) => extract(args),
    }
}

fn pack(args: PackArgs) -> Result<()> {
    let inputs = discover_inputs(&args.input, args.recursive, InputKind::Pack)?;
    let batch = inputs.len() > 1 || args.input.is_dir();
    if inputs.is_empty() {
        bail!("no files found to pack");
    }
    preflight_pack_outputs(&inputs, args.output.as_deref(), batch)?;

    let output = Arc::new(args.output);
    run_tasks(inputs, args.jobs, args.quiet, "packed", move |input| {
        pack_one(&input, output.as_deref(), batch, args.delete_source).map(|output| vec![output])
    })
}

fn extract(args: ExtractArgs) -> Result<()> {
    let inputs = discover_inputs(&args.input, args.recursive, InputKind::Extract)?;
    let batch = inputs.len() > 1 || args.input.is_dir();
    if inputs.is_empty() {
        bail!("no .ts files found to extract");
    }

    let mode = if args.raw {
        ExtractMode::Raw
    } else {
        ExtractMode::Mp4 {
            fallback_raw: args.fallback == FallbackMode::Raw,
        }
    };
    preflight_extract_outputs(&inputs, args.output.as_deref(), batch, mode)?;
    let output = Arc::new(args.output);
    run_tasks(inputs, args.jobs, args.quiet, "extracted", move |input| {
        extract_one(&input, output.as_deref(), batch, args.delete_source, mode)
    })
}

fn pack_one(
    input: &Path,
    output: Option<&Path>,
    batch: bool,
    delete_source: bool,
) -> Result<PathBuf> {
    let final_path = paths::pack_output_path(input, output, batch)?;
    paths::ensure_output_available(input, &final_path)?;
    let temp_path = paths::temp_path_for(&final_path)?;
    paths::ensure_temp_available(&temp_path)?;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    let result = custom_ts::pack_file(input, &temp_path)
        .and_then(|_| paths::commit_temp(&temp_path, &final_path));

    match result {
        Ok(()) => {
            if delete_source {
                fs::remove_file(input)
                    .with_context(|| format!("failed to delete source {}", input.display()))?;
            }
            Ok(final_path)
        }
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            Err(err)
        }
    }
}

fn extract_one(
    input: &Path,
    output: Option<&Path>,
    batch: bool,
    delete_source: bool,
    mode: ExtractMode,
) -> Result<Vec<PathBuf>> {
    let tsbox_probe = match custom_ts::probe(input) {
        Ok(probe) => probe,
        Err(err) if matches!(mode, ExtractMode::Raw) => {
            let _ = err;
            None
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to probe TSBOX payload in {}", input.display()));
        }
    };

    match tsbox_probe {
        Some(meta) => {
            let final_path =
                paths::extract_output_path(input, output, batch, meta.original_extension())?;
            paths::ensure_output_available(input, &final_path)?;
            let temp_path = paths::temp_path_for(&final_path)?;
            paths::ensure_temp_available(&temp_path)?;
            if let Some(parent) = final_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create output directory {}", parent.display())
                })?;
            }

            let result = custom_ts::extract_file(input, &temp_path)
                .and_then(|_| paths::commit_temp(&temp_path, &final_path));

            match result {
                Ok(()) => {
                    if delete_source {
                        fs::remove_file(input).with_context(|| {
                            format!("failed to delete source {}", input.display())
                        })?;
                    }
                    Ok(vec![final_path])
                }
                Err(err) => {
                    let _ = fs::remove_file(&temp_path);
                    Err(err)
                }
            }
        }
        None => {
            let outputs = match mode {
                ExtractMode::Mp4 { fallback_raw } => {
                    let final_path = paths::extract_output_path(input, output, batch, Some("mp4"))?;
                    paths::ensure_output_available(input, &final_path)?;
                    let temp_path = paths::temp_path_for(&final_path)?;
                    paths::ensure_temp_available(&temp_path)?;
                    if let Some(parent) = final_path.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("failed to create output directory {}", parent.display())
                        })?;
                    }

                    let result = media::remux_to_mp4(input, &temp_path)
                        .and_then(|_| paths::commit_temp(&temp_path, &final_path));

                    match result {
                        Ok(()) => vec![final_path],
                        Err(err) => {
                            let _ = fs::remove_file(&temp_path);
                            if !fallback_raw {
                                return Err(err);
                            }
                            let output_dir = fallback_raw_output_dir(input, output, batch);
                            fs::create_dir_all(&output_dir).with_context(|| {
                                format!(
                                    "failed to create fallback output directory {}",
                                    output_dir.display()
                                )
                            })?;
                            demux::demux_raw(input, &output_dir).with_context(|| {
                                format!("mp4 remux failed ({err:#}); raw fallback also failed")
                            })?
                        }
                    }
                }
                ExtractMode::Raw => {
                    let output_dir = paths::raw_output_dir(input, output)?;
                    fs::create_dir_all(&output_dir).with_context(|| {
                        format!("failed to create output directory {}", output_dir.display())
                    })?;
                    demux::demux_raw(input, &output_dir)?
                }
            };

            if delete_source {
                fs::remove_file(input)
                    .with_context(|| format!("failed to delete source {}", input.display()))?;
            }
            Ok(outputs)
        }
    }
}

#[derive(Copy, Clone)]
enum ExtractMode {
    Mp4 { fallback_raw: bool },
    Raw,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum)]
enum FallbackMode {
    None,
    Raw,
}

fn run_tasks<F>(
    inputs: Vec<PathBuf>,
    jobs: usize,
    quiet: bool,
    verb: &'static str,
    f: F,
) -> Result<()>
where
    F: Fn(PathBuf) -> Result<Vec<PathBuf>> + Send + Sync + 'static,
{
    let total = inputs.len();
    let jobs = jobs.max(1).min(total.max(1));
    let queue = Arc::new(Mutex::new(VecDeque::from(inputs)));
    let f = Arc::new(f);
    let (tx, rx) = mpsc::channel();

    let mut handles = Vec::with_capacity(jobs);
    for _ in 0..jobs {
        let queue = Arc::clone(&queue);
        let f = Arc::clone(&f);
        let tx = tx.clone();
        handles.push(thread::spawn(move || {
            loop {
                let input = {
                    let mut queue = queue.lock().expect("work queue lock poisoned");
                    queue.pop_front()
                };
                let Some(input) = input else {
                    break;
                };

                let result = f(input.clone()).map_err(|err| format!("{err:#}"));
                if tx.send((input, result)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);

    let mut done = 0usize;
    let mut failures = 0usize;
    for (input, result) in rx {
        done += 1;
        match result {
            Ok(outputs) => {
                if !quiet {
                    eprintln!(
                        "[{done}/{total}] {verb} {} -> {}",
                        input.display(),
                        format_outputs(&outputs)
                    );
                }
            }
            Err(err) => {
                failures += 1;
                eprintln!(
                    "[{done}/{total}] failed to {verb} {}: {err}",
                    input.display()
                );
            }
        }
    }

    for handle in handles {
        if handle.join().is_err() {
            failures += 1;
            eprintln!("worker thread panicked");
        }
    }

    if failures > 0 {
        bail!("{failures} file(s) failed");
    }
    Ok(())
}

fn preflight_pack_outputs(inputs: &[PathBuf], output: Option<&Path>, batch: bool) -> Result<()> {
    let mut planned = Vec::with_capacity(inputs.len());
    for input in inputs {
        planned.push((
            input.clone(),
            paths::pack_output_path(input, output, batch)?,
        ));
    }
    paths::ensure_no_output_collisions(&planned)?;
    for (input, output) in planned {
        paths::ensure_output_available(&input, &output)?;
        paths::ensure_temp_available(&paths::temp_path_for(&output)?)?;
    }
    Ok(())
}

fn preflight_extract_outputs(
    inputs: &[PathBuf],
    output: Option<&Path>,
    batch: bool,
    mode: ExtractMode,
) -> Result<()> {
    let mut planned = Vec::new();
    for input in inputs {
        match planned_extract_outputs(input, output, batch, mode) {
            Ok(outputs) => {
                for output in outputs {
                    planned.push((input.clone(), output));
                }
            }
            Err(_) => {
                // Per-file extraction still reports parse/remux errors. This preflight is only
                // for collisions we can prove before starting concurrent workers.
            }
        }
    }
    paths::ensure_no_output_collisions(&planned)?;
    Ok(())
}

fn planned_extract_outputs(
    input: &Path,
    output: Option<&Path>,
    batch: bool,
    mode: ExtractMode,
) -> Result<Vec<PathBuf>> {
    if let Some(meta) = custom_ts::probe(input)? {
        return Ok(vec![paths::extract_output_path(
            input,
            output,
            batch,
            meta.original_extension(),
        )?]);
    }

    match mode {
        ExtractMode::Raw => {
            let output_dir = paths::raw_output_dir(input, output)?;
            demux::plan_raw_outputs(input, &output_dir)
        }
        ExtractMode::Mp4 { fallback_raw } => {
            let mut outputs = vec![paths::extract_output_path(
                input,
                output,
                batch,
                Some("mp4"),
            )?];
            if fallback_raw {
                let output_dir = fallback_raw_output_dir(input, output, batch);
                if let Ok(raw_outputs) = demux::plan_raw_outputs(input, &output_dir) {
                    outputs.extend(raw_outputs);
                }
            }
            Ok(outputs)
        }
    }
}

fn fallback_raw_output_dir(input: &Path, output: Option<&Path>, batch: bool) -> PathBuf {
    if let Some(output) = output {
        if !batch && output.extension().is_some() && !output.is_dir() {
            return output
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
        }
        return output.to_path_buf();
    }
    input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn format_outputs(outputs: &[PathBuf]) -> String {
    outputs
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Copy, Clone)]
enum InputKind {
    Pack,
    Extract,
}

fn discover_inputs(root: &Path, recursive: bool, kind: InputKind) -> Result<Vec<PathBuf>> {
    let metadata = fs::metadata(root)
        .with_context(|| format!("failed to read input metadata {}", root.display()))?;

    if metadata.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !metadata.is_dir() {
        bail!(
            "input is neither a file nor a directory: {}",
            root.display()
        );
    }

    let mut out = Vec::new();
    collect_files(root, recursive, kind, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_files(
    dir: &Path,
    recursive: bool,
    kind: InputKind,
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type {}", path.display()))?;
        if file_type.is_dir() {
            if recursive {
                collect_files(&path, recursive, kind, out)?;
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        match kind {
            InputKind::Pack => out.push(path),
            InputKind::Extract => {
                if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
                {
                    out.push(path);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_extract_filters_non_ts_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.ts"), b"not a real ts").unwrap();
        fs::write(temp.path().join("b.txt"), b"ignore").unwrap();

        let inputs = discover_inputs(temp.path(), false, InputKind::Extract).unwrap();
        assert_eq!(inputs, vec![temp.path().join("a.ts")]);
    }

    #[test]
    fn missing_input_is_an_error() {
        let err = discover_inputs(Path::new("missing"), false, InputKind::Pack).unwrap_err();
        assert!(format!("{err:#}").contains("failed to read input metadata"));
    }

    #[test]
    fn exact_output_file_is_allowed_for_single_pack() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("a.bin");
        fs::write(&input, b"x").unwrap();
        let output = temp.path().join("custom.ts");
        let actual = paths::pack_output_path(&input, Some(&output), false).unwrap();
        assert_eq!(actual, output);
    }

    #[test]
    fn batch_output_is_always_a_directory() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("a.bin");
        let out_dir = temp.path().join("out.ts");
        let actual = paths::pack_output_path(&input, Some(&out_dir), true).unwrap();
        assert_eq!(actual, out_dir.join("a.ts"));
    }

    #[test]
    fn output_collision_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("a.bin");
        let output = temp.path().join("a.ts");
        fs::write(&input, b"x").unwrap();
        fs::write(&output, b"old").unwrap();

        let err = paths::ensure_output_available(&input, &output).unwrap_err();
        assert!(format!("{err:#}").contains("output already exists"));
    }
}
