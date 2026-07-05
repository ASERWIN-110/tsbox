use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};

pub fn pack_output_path(input: &Path, output: Option<&Path>, batch: bool) -> Result<PathBuf> {
    let stem = file_stem(input)?;
    if let Some(output) = output {
        if !batch && looks_like_file_output(output, "ts") {
            return Ok(output.to_path_buf());
        }
        return Ok(output.join(with_extension(stem, Some("ts"))));
    }

    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent.join(with_extension(stem, Some("ts"))))
}

pub fn extract_output_path(
    input: &Path,
    output: Option<&Path>,
    batch: bool,
    extension: Option<&str>,
) -> Result<PathBuf> {
    let stem = file_stem(input)?;
    if let Some(output) = output {
        if !batch && looks_like_exact_file_output(output) {
            return Ok(output.to_path_buf());
        }
        return Ok(output.join(with_extension(stem, extension)));
    }

    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent.join(with_extension(stem, extension)))
}

pub fn raw_output_dir(input: &Path, output: Option<&Path>) -> Result<PathBuf> {
    if let Some(output) = output {
        return Ok(output.to_path_buf());
    }
    Ok(input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf())
}

pub fn ensure_output_available(input: &Path, output: &Path) -> Result<()> {
    if paths_equal(input, output)? {
        bail!("output path would overwrite input: {}", output.display());
    }
    if output.exists() {
        bail!("output already exists: {}", output.display());
    }
    Ok(())
}

pub fn ensure_temp_available(temp_path: &Path) -> Result<()> {
    if temp_path.exists() {
        bail!("temporary output already exists: {}", temp_path.display());
    }
    Ok(())
}

pub fn ensure_no_output_collisions(planned: &[(PathBuf, PathBuf)]) -> Result<()> {
    let mut seen = HashMap::<PathBuf, PathBuf>::new();
    for (input, output) in planned {
        let key = output_key(output)?;
        if let Some(previous) = seen.insert(key, input.clone()) {
            bail!(
                "output filename collision: {} and {} both map to {}",
                previous.display(),
                input.display(),
                output.display()
            );
        }
    }
    Ok(())
}

pub fn temp_path_for(final_path: &Path) -> Result<PathBuf> {
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    let name = final_path
        .file_name()
        .ok_or_else(|| anyhow!("output path has no file name: {}", final_path.display()))?;

    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(".part");

    if let Some(ext) = final_path.extension() {
        temp_name.push(".");
        temp_name.push(ext);
    }

    Ok(parent.join(temp_name))
}

pub fn commit_temp(temp_path: &Path, final_path: &Path) -> Result<()> {
    if final_path.exists() {
        bail!("output already exists: {}", final_path.display());
    }
    fs::rename(temp_path, final_path).map_err(Into::into)
}

fn looks_like_file_output(path: &Path, expected_ext: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(expected_ext))
}

fn looks_like_exact_file_output(path: &Path) -> bool {
    path.extension().is_some() && !path.is_dir()
}

fn file_stem(path: &Path) -> Result<&OsStr> {
    path.file_stem()
        .filter(|stem| !stem.is_empty())
        .or_else(|| path.file_name())
        .ok_or_else(|| anyhow!("input path has no file name: {}", path.display()))
}

fn with_extension(stem: &OsStr, extension: Option<&str>) -> OsString {
    let mut name = OsString::from(stem);
    if let Some(extension) = extension.filter(|extension| !extension.is_empty()) {
        name.push(".");
        name.push(extension);
    }
    name
}

fn paths_equal(left: &Path, right: &Path) -> Result<bool> {
    let left_abs = absolutize(left)?;
    let right_abs = absolutize(right)?;
    Ok(left_abs == right_abs)
}

fn output_key(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.exists() {
        fs::canonicalize(parent)?
    } else {
        absolutize(parent)?
    };
    Ok(parent.join(
        path.file_name()
            .ok_or_else(|| anyhow!("path has no file name: {}", path.display()))?,
    ))
}

fn absolutize(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path).map_err(Into::into);
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.exists() {
        fs::canonicalize(parent)?
    } else {
        let base = std::env::current_dir()?;
        base.join(parent)
    };

    Ok(parent.join(
        path.file_name()
            .ok_or_else(|| anyhow!("path has no file name: {}", path.display()))?,
    ))
}
