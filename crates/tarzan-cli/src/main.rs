mod cmd_cat;
mod cmd_extract;
mod cmd_list;
mod cmd_verify;
mod cmd_wrap;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Tar archives with random-access zstd and an embedded index.
///
/// tarzan reads and writes `.tar.zst` archives augmented with a table of
/// contents stored as a zstd skippable frame. Standard zstd tools can
/// decompress a tarzan archive normally; tarzan-aware tools can also list
/// contents and extract single files without a full decompression pass.
#[derive(Debug, Parser)]
#[command(name = "tarzan", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Wrap an existing tar stream into a tarzan `.tar.zst` archive.
    ///
    /// Reads a raw tar stream and writes a tarzan-formatted archive,
    /// splitting the body into independently decodable zstd frames and
    /// appending a TOC frame. Designed for pipelines such as
    /// `tar -cf - ./dir | tarzan wrap -f out.tar.zst`.
    Wrap {
        /// Input tar stream. `-` or omitted reads from stdin.
        #[arg(value_name = "TAR")]
        input: Option<PathBuf>,

        /// Output archive path. `-` or omitted writes to stdout.
        #[arg(short = 'f', long = "file", value_name = "ARCHIVE")]
        file: Option<PathBuf>,

        /// Chunk boundary size. Accepts plain bytes or K/M/G suffixes.
        /// Smaller chunks improve random-access granularity at some cost
        /// to compression ratio; larger chunks compress better.
        #[arg(long = "chunk-size", default_value = "4M", value_parser = parse_size)]
        chunk_size: usize,

        /// Zstd compression level (1 = fastest, 22 = best).
        #[arg(long = "level", default_value_t = 3)]
        level: i32,

        /// List each member to stderr after wrapping (tar's `-cvf`). Only
        /// effective when output is a file; for stdout/pipes the listing
        /// is suppressed since the archive can't be re-read.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// List archive contents using only the embedded TOC.
    ///
    /// Reads the TOC skippable frame at the tail of the archive without
    /// decompressing any chunk data, so it runs in roughly constant time
    /// regardless of archive size.
    #[command(visible_aliases = ["t", "ls"])]
    List {
        /// Archive to list.
        #[arg(short = 'f', long = "file", value_name = "ARCHIVE")]
        file: PathBuf,

        /// Show permissions, size, and mtime in addition to the path,
        /// like `tar -tvf`.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },

    /// Stream a single member from the archive to stdout.
    ///
    /// Uses the TOC to seek directly to the member's chunks; only those
    /// chunks are decompressed.
    Cat {
        /// Archive to read from.
        #[arg(short = 'f', long = "file", value_name = "ARCHIVE")]
        file: PathBuf,

        /// Path of the member within the archive.
        #[arg(value_name = "PATH")]
        path: String,
    },

    /// Extract archive members onto the filesystem.
    ///
    /// Decompresses only the chunks needed for the requested members
    /// (or all of them, if no positional paths are given). Refuses to
    /// extract absolute paths or paths containing `..` so extraction
    /// stays inside the destination directory.
    #[command(visible_alias = "x")]
    Extract {
        /// Archive to extract from.
        #[arg(short = 'f', long = "file", value_name = "ARCHIVE")]
        file: PathBuf,

        /// Destination directory (created if missing). Defaults to the
        /// current working directory.
        #[arg(
            short = 'C',
            long = "directory",
            value_name = "DIR",
            default_value = "."
        )]
        directory: PathBuf,

        /// Drop N leading path components from each archive entry,
        /// like `tar --strip-components`.
        #[arg(long = "strip-components", value_name = "N", default_value_t = 0)]
        strip_components: usize,

        /// Skip members matching this shell-glob pattern. Repeatable.
        #[arg(long = "exclude", value_name = "PATTERN")]
        exclude: Vec<String>,

        /// Print each member to stderr as it is extracted.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,

        /// Restrict extraction to these paths or directory prefixes;
        /// omit to extract everything. Matching is by exact path,
        /// directory-prefix, or shell glob.
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },

    /// Verify SHA-256 checksums recorded in the TOC.
    ///
    /// Decompresses each chunk and compares its SHA-256 against the value
    /// recorded at archive creation time. Exits non-zero if any chunk
    /// fails to verify.
    Verify {
        /// Archive to verify.
        #[arg(short = 'f', long = "file", value_name = "ARCHIVE")]
        file: PathBuf,

        /// Restrict verification to a single member path; omit to verify
        /// every member.
        #[arg(value_name = "PATH")]
        path: Option<String>,

        /// Print an `OK` line for every successfully-verified member.
        /// Without this flag, verify is silent on success and only
        /// reports mismatches.
        #[arg(short = 'v', long = "verbose")]
        verbose: bool,
    },
}

fn parse_size(value: &str) -> Result<usize, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("chunk size cannot be empty".to_owned());
    }

    let split_idx = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, suffix) = value.split_at(split_idx);
    if digits.is_empty() {
        return Err("chunk size must start with digits".to_owned());
    }

    let base = digits
        .parse::<usize>()
        .map_err(|error| format!("invalid chunk size number: {error}"))?;
    let scale = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1usize,
        "k" | "kb" => 1024usize,
        "m" | "mb" => 1024usize * 1024,
        "g" | "gb" => 1024usize * 1024 * 1024,
        _ => return Err(format!("invalid chunk size suffix: {suffix}")),
    };

    base.checked_mul(scale)
        .ok_or_else(|| "chunk size is too large".to_owned())
}

/// Treat `-` (or absence) as the stdin/stdout sentinel.
fn resolve_stream(path: Option<PathBuf>) -> Option<PathBuf> {
    path.filter(|p| p.as_os_str() != "-")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Wrap {
            input,
            file,
            chunk_size,
            level,
            verbose,
        } => {
            let input = resolve_stream(input);
            let output = resolve_stream(file);
            cmd_wrap::run(
                input.as_deref(),
                output.as_deref(),
                chunk_size,
                level,
                verbose,
            )
        }
        Commands::List { file, verbose } => cmd_list::run(&file, verbose),
        Commands::Cat { file, path } => cmd_cat::run(&file, &path),
        Commands::Extract {
            file,
            directory,
            strip_components,
            exclude,
            verbose,
            paths,
        } => cmd_extract::run(
            &file,
            &directory,
            strip_components,
            exclude,
            paths,
            verbose,
        ),
        Commands::Verify {
            file,
            path,
            verbose,
        } => cmd_verify::run(&file, path.as_deref(), verbose),
    }
}
