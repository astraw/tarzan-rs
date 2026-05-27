use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tarzan::format::toc::TocMember;
use tracing::info;

pub fn run(
    input: Option<&Path>,
    output: Option<&Path>,
    chunk_size: usize,
    level: i32,
    verbose: bool,
    sync: bool,
) -> Result<()> {
    if output.is_none() && io::stdout().is_terminal() {
        bail!("refusing to write binary archive to terminal; use `-f FILE` or redirect stdout");
    }

    let opts = tarzan::WrapOptions::default()
        .chunk_size(chunk_size)
        .level(level);

    // tar's -v lists each member as it is processed. wrap streams the input,
    // so members are reported as soon as they finish compressing.
    let on_member = |member: &TocMember| {
        if verbose {
            eprintln!("{}", member.path);
        }
    };

    match (input, output) {
        (Some(input_path), Some(output_path)) => {
            info!(input = %input_path.display(), output = %output_path.display(), "wrapping tar file");
            if paths_refer_to_same_file(input_path, output_path)? {
                bail!(
                    "refusing to use the same path for input and output: {}",
                    output_path.display()
                );
            }
            let input_file = File::open(input_path)?;
            wrap_to_output_file(
                BufReader::new(input_file),
                output_path,
                opts,
                on_member,
                sync,
            )?;
        }
        (Some(input_path), None) => {
            info!(input = %input_path.display(), "wrapping tar stream to stdout");
            let input_file = File::open(input_path)?;
            let stdout = io::stdout();
            let lock = stdout.lock();
            tarzan::wrap_with(BufReader::new(input_file), lock, opts, on_member)?;
        }
        (None, Some(output_path)) => {
            info!(output = %output_path.display(), "wrapping stdin tar stream to file");
            let stdin = io::stdin();
            let input_lock = stdin.lock();
            wrap_to_output_file(input_lock, output_path, opts, on_member, sync)?;
        }
        (None, None) => {
            info!("wrapping stdin tar stream to stdout");
            let stdin = io::stdin();
            let stdout = io::stdout();
            tarzan::wrap_with(stdin.lock(), stdout.lock(), opts, on_member)?;
        }
    }

    Ok(())
}

fn paths_refer_to_same_file(input_path: &Path, output_path: &Path) -> Result<bool> {
    let input = fs::canonicalize(input_path)
        .with_context(|| format!("resolving input path {}", input_path.display()))?;
    let output = match fs::canonicalize(output_path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("resolving output path {}", output_path.display()));
        }
    };
    Ok(input == output)
}

fn wrap_to_output_file<R, F>(
    input: R,
    output_path: &Path,
    opts: tarzan::WrapOptions,
    on_member: F,
    sync: bool,
) -> Result<()>
where
    R: Read,
    F: FnMut(&TocMember),
{
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = output_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("output path has no file name: {}", output_path.display()))?
        .to_string_lossy();
    let (temp_path, output_file) = create_temp_output_file(parent, &file_name)?;
    let result = (|| {
        let mut output = BufWriter::new(output_file);
        tarzan::wrap_with(input, &mut output, opts, on_member)?;
        output
            .flush()
            .with_context(|| format!("flushing temporary output {}", temp_path.display()))?;
        if sync {
            output
                .get_ref()
                .sync_all()
                .with_context(|| format!("syncing temporary output {}", temp_path.display()))?;
        }
        fs::rename(&temp_path, output_path).with_context(|| {
            format!(
                "renaming temporary output {} to {}",
                temp_path.display(),
                output_path.display()
            )
        })?;
        if sync {
            sync_directory(parent)?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn create_temp_output_file(parent: &Path, file_name: &str) -> Result<(PathBuf, File)> {
    for attempt in 0..100u32 {
        let candidate = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            attempt
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating temporary output in {}", parent.display()));
            }
        }
    }
    bail!(
        "could not allocate a temporary output path in {}",
        parent.display()
    )
}

fn sync_directory(path: &Path) -> Result<()> {
    // Windows does not support opening a directory as a File for fsync;
    // directory entry durability is handled by the OS on rename.
    #[cfg(not(windows))]
    {
        File::open(path)
            .with_context(|| format!("opening directory {} for sync", path.display()))?
            .sync_all()
            .with_context(|| format!("syncing directory {}", path.display()))?;
    }
    #[cfg(windows)]
    let _ = path;
    Ok(())
}
