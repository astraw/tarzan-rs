mod cmd_list;
mod cmd_wrap;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "tarzan")]
#[command(version)]
#[command(about = "Tar archive with random-access zstd and index")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Wrap {
        #[arg(short = 'i', long = "input")]
        input: Option<PathBuf>,

        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,

        #[arg(long = "chunk-size", default_value = "4M", value_parser = parse_size)]
        chunk_size: usize,

        #[arg(long = "level", default_value_t = 3)]
        level: i32,
    },
    List {
        archive: PathBuf,

        #[arg(short = 'l', long = "long")]
        long_format: bool,
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
            output,
            chunk_size,
            level,
        } => cmd_wrap::run(input.as_deref(), output.as_deref(), chunk_size, level),
        Commands::List {
            archive,
            long_format,
        } => cmd_list::run(&archive, long_format),
    }
}
