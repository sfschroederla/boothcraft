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
    /// List the built-in effects and their parameters.
    Effects,
    /// Apply one effect to a whole file: `apply --effect amplify --param left=6 in.wav out.wav`.
    Apply {
        #[arg(long)]
        effect: String,
        /// Parameter settings as id=value (repeatable).
        #[arg(long = "param", value_parser = parse_kv)]
        params: Vec<(String, f32)>,
        input: PathBuf,
        output: PathBuf,
        #[arg(long, value_enum, default_value = "32f")]
        depth: Depth,
    },
    /// Print format, length and level information.
    Info { file: PathBuf },
    /// Find and list third-party plugins.
    #[cfg(feature = "plugins")]
    Plugins {
        #[command(subcommand)]
        action: PluginsCommand,
    },
    /// Convert between WAV, AIFF and FLAC (chosen by output extension).
    Convert {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, value_enum, default_value = "24")]
        depth: Depth,
        /// Resample to this rate (Hz) on the way out.
        #[arg(long)]
        rate: Option<u32>,
        /// Add TPDF dither when writing 16 or 24-bit.
        #[arg(long)]
        dither: bool,
    },
}

#[cfg(feature = "plugins")]
#[derive(Subcommand)]
enum PluginsCommand {
    /// Scan for plugins (each in its own child process) and update the cache.
    Scan,
    /// List the plugins found by the last scan.
    List {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

/// The plugin formats this build can host.
#[cfg(feature = "plugins")]
fn plugin_formats() -> Vec<Box<dyn boothcraft_plugin::PluginFormat>> {
    #[allow(unused_mut)]
    let mut formats: Vec<Box<dyn boothcraft_plugin::PluginFormat>> = Vec::new();
    #[cfg(feature = "mock-plugins")]
    if std::env::var("BOOTHCRAFT_PLUGINS").is_ok_and(|v| v == "mock") {
        formats.push(Box::new(boothcraft_plugin::mock::MockFormat::new()));
    }
    formats
}

#[cfg(feature = "plugins")]
fn plugins_command(action: PluginsCommand) -> Result<()> {
    use boothcraft_plugin::{MainThread, PluginManager, ScanConfig, WorkerCommand};
    let mut manager = PluginManager::new(
        MainThread::init(),
        plugin_formats(),
        boothcraft_plugin::settings::default_dir(),
        std::sync::Arc::new(|| {}),
    );
    match action {
        PluginsCommand::Scan => {
            let cfg = ScanConfig::new(WorkerCommand::current_exe()?);
            let r = manager.scan_blocking(&cfg);
            println!(
                "probed {}, cached {}, found {} plugin(s)",
                r.probed, r.cached, r.found
            );
            for p in &r.blocked {
                println!("blocked (crashed or hung while scanning): {}", p.display());
            }
            for (p, why) in &r.invalid {
                println!("skipped {}: {why}", p.display());
            }
        }
        PluginsCommand::List { json } => {
            let all: Vec<_> = manager.catalog().all().collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&all)?);
            } else {
                for d in all {
                    println!(
                        "{} - {} ({}) [{}]",
                        d.effect_id(),
                        d.name,
                        d.vendor,
                        d.path.display()
                    );
                }
            }
        }
    }
    manager.shutdown(Vec::new());
    Ok(())
}

fn parse_kv(s: &str) -> Result<(String, f32), String> {
    let (k, v) = s.split_once('=').ok_or("expected id=value")?;
    Ok((
        k.trim().to_string(),
        v.trim().parse().map_err(|_| format!("not a number: {v}"))?,
    ))
}

fn to_depth(d: Depth) -> BitDepth {
    match d {
        Depth::Int16 => BitDepth::Int16,
        Depth::Int24 => BitDepth::Int24,
        Depth::Float32 => BitDepth::Float32,
    }
}

fn apply_effect(
    id: &str,
    params: &[(String, f32)],
    doc: &mut boothcraft_core::Document,
) -> Result<()> {
    use boothcraft_dsp::registry;
    use std::sync::atomic::{AtomicBool, AtomicU32};
    let info = registry::info(id)
        .with_context(|| format!("unknown effect {id:?}; see `boothcraft-cli effects`"))?;
    let specs = registry::specs(id).unwrap_or(&[]);
    let mut values: Vec<f32> = specs.iter().map(|s| s.default).collect();
    for (k, v) in params {
        let i = specs
            .iter()
            .position(|s| s.id == k)
            .with_context(|| format!("{id} has no parameter {k:?}"))?;
        values[i] = specs[i].clamp(*v);
    }
    let n = doc.frames();
    let mut chans: Vec<Vec<f32>> = doc.channels.iter().map(|c| c.to_vec(0..n)).collect();
    let rate = doc.sample_rate as f32;
    if info.is_realtime() {
        let mut desc = boothcraft_dsp::RackDesc::single(id).context("effect")?;
        desc.slots[0].as_mut().context("slot")?.values = values;
        anyhow::ensure!(
            boothcraft_dsp::rack::render_offline(
                &desc,
                &mut chans,
                rate,
                &AtomicU32::new(0),
                &AtomicBool::new(false),
            ),
            "rendering {id} did not complete"
        );
    } else {
        let mut fx = registry::create_offline(id).context("effect")?;
        fx.values_mut().copy_from_slice(&values);
        fx.process_offline(&mut chans, rate);
    }
    let slices: Vec<&[f32]> = chans.iter().map(Vec::as_slice).collect();
    doc.overwrite(0, &slices);
    Ok(())
}

fn main() -> Result<()> {
    // Hidden mode: `--plugin-scan-worker <format> <path>` probes one plugin file and exits.
    #[cfg(feature = "plugins")]
    boothcraft_plugin::maybe_run_scan_worker(&plugin_formats());
    // Removes the disk-backed block store's segment directory when this function returns,
    // including on an early `?` return.
    let _cleanup = boothcraft_core::install_cleanup_guard();
    match Cli::parse().command {
        #[cfg(feature = "plugins")]
        Command::Plugins { action } => plugins_command(action)?,
        Command::Effects => {
            for info in boothcraft_dsp::registry::EFFECTS {
                let specs = boothcraft_dsp::registry::specs(info.id).unwrap_or(&[]);
                let kind = if info.is_realtime() { "" } else { " (offline)" };
                println!(
                    "{} - {}{} [{}]",
                    info.id,
                    info.name,
                    kind,
                    info.category.name()
                );
                for s in specs {
                    if s.choices.is_empty() {
                        println!(
                            "    {:<14} {} .. {} (default {})",
                            s.id,
                            s.format(s.min),
                            s.format(s.max),
                            s.format(s.default)
                        );
                    } else {
                        let opts: Vec<String> = s
                            .choices
                            .iter()
                            .enumerate()
                            .map(|(i, c)| format!("{i}={c}"))
                            .collect();
                        println!(
                            "    {:<14} {} (default {})",
                            s.id,
                            opts.join(", "),
                            s.default
                        );
                    }
                }
            }
        }
        Command::Apply {
            effect,
            params,
            input,
            output,
            depth,
        } => {
            let Some(format) = boothcraft_io::format_for_path(&output) else {
                bail!("output extension must be .wav, .aiff or .flac");
            };
            let mut doc = boothcraft_io::read(&input)
                .with_context(|| format!("reading {}", input.display()))?;
            apply_effect(&effect, &params, &mut doc)?;
            boothcraft_io::write(
                &doc,
                &output,
                &boothcraft_io::WriteOptions::new(format, to_depth(depth)),
            )?;
            println!("applied {effect} -> {}", output.display());
        }
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
            rate,
            dither,
        } => {
            let depth = to_depth(depth);
            let Some(format) = boothcraft_io::format_for_path(&output) else {
                bail!("output extension must be .wav, .aiff or .flac");
            };
            let doc = boothcraft_io::read(&input)
                .with_context(|| format!("reading {}", input.display()))?;
            let opts = boothcraft_io::WriteOptions {
                sample_rate: rate,
                dither,
                ..boothcraft_io::WriteOptions::new(format, depth)
            };
            boothcraft_io::write(&doc, &output, &opts)?;
            let rate = rate.unwrap_or(doc.sample_rate);
            println!(
                "wrote {} ({} Hz, {} channels)",
                output.display(),
                rate,
                doc.channel_count()
            );
        }
    }
    Ok(())
}
