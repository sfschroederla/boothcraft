use anyhow::{bail, Context, Result};
use boothcraft_io::BitDepth;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "boothcraft-cli",
    version,
    about = "Headless Boothcraft audio tools"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum Depth {
    #[value(name = "16")]
    Int16,
    #[value(name = "24")]
    Int24,
    #[value(name = "32f")]
    Float32,
}

#[derive(Subcommand)]
enum Command {
    /// Print format, length and level information.
    Info { file: PathBuf },
    /// Convert between WAV, AIFF and FLAC (chosen by output extension).
    Convert {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, value_enum, default_value = "24")]
        depth: Depth,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Info { file } => {
            let doc = boothcraft_io::read(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let peak = doc
                .channels
                .iter()
                .flat_map(|c| c.peaks(0..doc.frames(), 1))
                .map(|p| p.min.abs().max(p.max.abs()))
                .fold(0.0f32, f32::max);
            println!("channels:    {}", doc.channel_count());
            println!("sample rate: {} Hz", doc.sample_rate);
            println!("frames:      {}", doc.frames());
            println!("duration:    {:.3} s", doc.duration_secs());
            println!("peak:        {:.2} dBFS", 20.0 * peak.max(1e-9).log10());
        }
        Command::Convert {
            input,
            output,
            depth,
        } => {
            let depth = match depth {
                Depth::Int16 => BitDepth::Int16,
                Depth::Int24 => BitDepth::Int24,
                Depth::Float32 => BitDepth::Float32,
            };
            if boothcraft_io::format_for_path(&output).is_none() {
                bail!("output extension must be .wav, .aiff or .flac");
            }
            let doc = boothcraft_io::convert(&input, &output, depth)?;
            println!("wrote {} ({} frames)", output.display(), doc.frames());
        }
    }
    Ok(())
}
