use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

pub fn remux_to_mp4(input: &Path, output: &Path) -> Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output)
        .status()
        .with_context(|| "failed to start ffmpeg; install ffmpeg or use TSBOX-packed files")?;

    if !status.success() {
        bail!("ffmpeg remux failed with status {status}");
    }
    let len = std::fs::metadata(output)
        .with_context(|| format!("ffmpeg did not create output {}", output.display()))?
        .len();
    if len == 0 {
        bail!("ffmpeg produced an empty output: {}", output.display());
    }
    Ok(())
}
