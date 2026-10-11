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
    ///
    /// Plugins are named by the ids `plugins list` prints (`vst3:<class id>`); their parameters are
    /// set by name or id, normalized 0..1 (a stepped parameter takes its step number). `--rack`
    /// applies a saved rack file instead of one effect.
    Apply {
        #[arg(long, required_unless_present = "rack")]
        effect: Option<String>,
        /// A rack saved from the app (`.json`), instead of `--effect`.
        #[arg(long, conflicts_with = "effect")]
        rack: Option<PathBuf>,
        /// A plugin's saved state (as stored in a rack file), loaded before `--param`s apply.
        #[arg(long, requires = "effect")]
        state: Option<PathBuf>,
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
    Scan {
        /// An extra folder to search, for this run (repeatable).
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        /// Also search the standard system plugin folders (all of your installed plugins). Off
        /// unless this is given or the app's Plugin Manager has it ticked.
        #[arg(long)]
        standard: bool,
        /// Accepted for compatibility: searching only `--path` folders is now the default.
        #[arg(long, hide = true)]
        only_given_paths: bool,
    },
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
    let mut formats: Vec<Box<dyn boothcraft_plugin::PluginFormat>> =
        vec![Box::new(boothcraft_plugin_vst3::Vst3Format::new())];
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
        PluginsCommand::Scan {
            paths,
            standard,
            only_given_paths: _,
        } => {
            manager.settings.extra_paths.extend(paths);
            let mut cfg = ScanConfig::new(WorkerCommand::current_exe()?);
            cfg.standard_locations = standard || manager.settings.scan_standard_locations;
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

/// Apply plugins (a single `--effect vst3:…`, or every slot of a `--rack` file) to the whole
/// document, through new offline instances with latency compensated.
#[cfg(feature = "plugins")]
fn apply_with_plugins(
    effect: Option<&str>,
    rack: Option<&std::path::Path>,
    state: Option<&std::path::Path>,
    params: &[(String, f32)],
    doc: &mut boothcraft_core::Document,
) -> Result<()> {
    use boothcraft_dsp::rack::{render_offline_with, OFFLINE_BLOCK};
    use boothcraft_dsp::{PluginState, RackConfig, RackDesc};
    use boothcraft_plugin::{MainThread, PluginManager};
    use std::sync::atomic::{AtomicBool, AtomicU32};

    let mut manager = PluginManager::new(
        MainThread::init(),
        plugin_formats(),
        boothcraft_plugin::settings::default_dir(),
        std::sync::Arc::new(|| {}),
    );
    let mut desc = match rack {
        Some(path) => boothcraft_dsp::preset::load_rack(path)
            .with_context(|| format!("reading {}", path.display()))?,
        None => RackDesc::default(),
    };
    if let Some(id) = effect {
        let d = manager
            .catalog()
            .get(id)
            .cloned()
            .with_context(|| format!("no plugin {id:?}; run `boothcraft-cli plugins scan`"))?;
        let mut slot = manager.new_slot(&d);
        if let Some(path) = state {
            slot.state = Some(PluginState(
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
            ));
        }
        manager
            .ensure_loaded(&slot)
            .map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
        let infos = manager
            .params(slot.id)
            .context("plugin has no parameters")?;
        for (key, value) in params {
            let p = infos
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(key) || p.id.to_string() == *key)
                .with_context(|| {
                    let names: Vec<_> = infos.iter().map(|p| p.name.as_str()).collect();
                    format!("{id} has no parameter {key:?}; it has {names:?}")
                })?;
            manager.edit_param(slot.id, p.id, *value);
        }
        desc.slots[0] = Some(slot);
    } else {
        anyhow::ensure!(params.is_empty(), "--param needs --effect");
    }
    manager.snapshot(&mut desc);

    let n = doc.frames();
    let mut chans: Vec<Vec<f32>> = doc.channels.iter().map(|c| c.to_vec(0..n)).collect();
    let cfg = RackConfig {
        sample_rate: doc.sample_rate as f32,
        channels: chans.len(),
        max_block: OFFLINE_BLOCK,
    };
    let (mut built, errors) = manager
        .prepare_offline(&desc, &cfg)
        .poll()
        .context("a plugin is still loading")?
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !errors.is_empty() {
        let why: Vec<String> = errors
            .iter()
            .map(|(i, e)| format!("slot {}: {e}", i + 1))
            .collect();
        manager.finish_offline(built);
        manager.shutdown(Vec::new());
        bail!("cannot apply: {}", why.join("; "));
    }
    let done = render_offline_with(
        &mut built,
        &mut chans,
        &AtomicU32::new(0),
        &AtomicBool::new(false),
    );
    manager.finish_offline(built);
    manager.shutdown(Vec::new());
    anyhow::ensure!(done, "rendering did not complete");
    let slices: Vec<&[f32]> = chans.iter().map(Vec::as_slice).collect();
    doc.overwrite(0, &slices);
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
            rack,
            state,
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
            let label = match (&effect, &rack) {
                (Some(e), _) => e.clone(),
                (None, Some(r)) => r.display().to_string(),
                (None, None) => unreachable!("clap requires one of --effect and --rack"),
            };
            let is_plugin = effect
                .as_deref()
                .is_some_and(boothcraft_dsp::registry::is_plugin_id);
            if is_plugin || rack.is_some() {
                #[cfg(feature = "plugins")]
                apply_with_plugins(
                    effect.as_deref(),
                    rack.as_deref(),
                    state.as_deref(),
                    &params,
                    &mut doc,
                )?;
                #[cfg(not(feature = "plugins"))]
                bail!("this build has no plugin support");
            } else {
                let _ = state;
                apply_effect(effect.as_deref().unwrap_or_default(), &params, &mut doc)?;
            }
            boothcraft_io::write(
                &doc,
                &output,
                &boothcraft_io::WriteOptions::new(format, to_depth(depth)),
            )?;
            println!("applied {label} -> {}", output.display());
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
