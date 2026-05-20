use std::fs::File;
use std::io::{self, BufReader, BufWriter, IsTerminal};
use std::path::Path;

use anyhow::{Result, bail};
use tarzan::format::toc::TocMember;
use tracing::info;

pub fn run(
    input: Option<&Path>,
    output: Option<&Path>,
    chunk_size: usize,
    level: i32,
    verbose: bool,
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
            let input_file = File::open(input_path)?;
            let output_file = File::create(output_path)?;
            tarzan::wrap_with(
                BufReader::new(input_file),
                BufWriter::new(output_file),
                opts,
                on_member,
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
            let output_file = File::create(output_path)?;
            tarzan::wrap_with(input_lock, BufWriter::new(output_file), opts, on_member)?;
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
