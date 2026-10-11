//! Loads real VST3 plugins (built by `scripts/build_vst3_fixtures.sh`) and checks the host.
//!
//! `harness = false` with libtest-mimic and one test thread: VST3 setup must happen on the main
//! thread, which the stock harness never uses.

use boothcraft_dsp::rack::render_offline_with;
use boothcraft_dsp::{Rack, RackConfig, RackDesc, SlotDesc};
use boothcraft_plugin::{
    Candidate, HostContext, MainThread, PluginDescriptor, PluginFormat, PluginManager,
    ProcessConfig,
};
use boothcraft_plugin_vst3::Vst3Format;
use libtest_mimic::{Arguments, Failed, Trial};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;

type Res = Result<(), Failed>;
type TestFn = fn() -> Res;

fn fixture_dir() -> PathBuf {
    std::env::var_os("BOOTHCRAFT_VST3_TEST_DIR")
        .map(PathBuf::from)
        .expect("set BOOTHCRAFT_VST3_TEST_DIR (see scripts/build_vst3_fixtures.sh)")
}

fn bundle(name: &str) -> PathBuf {
    let p = fixture_dir().join(name);
    assert!(p.exists(), "missing fixture {}", p.display());
    p
}

fn probe(name: &str) -> Vec<PluginDescriptor> {
    Vst3Format::new()
        .probe(&Candidate {
            path: bundle(name),
            known: None,
        })
        .unwrap_or_else(|e| panic!("probing {name}: {e}"))
}

fn manager(descs: &[PluginDescriptor]) -> PluginManager {
    let mut m = PluginManager::new(
        MainThread::init(),
        vec![Box::new(Vst3Format::new())],
        None,
        Arc::new(|| {}),
    );
    for d in descs {
        m.register(d.clone());
    }
    m
}

fn cfg(channels: usize) -> RackConfig {
    RackConfig {
        sample_rate: 48_000.0,
        channels,
        max_block: 4096,
    }
}

fn rack_of(d: &PluginDescriptor) -> (RackDesc, SlotDesc) {
    let slot = SlotDesc::for_plugin(d.plugin_ref(), Vec::new(), None);
    let mut desc = RackDesc::default();
    desc.slots[0] = Some(slot.clone());
    (desc, slot)
}

fn signal(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (i as f32 * 0.05).sin() * 0.5 + (i as f32 * 0.31).cos() * 0.2)
        .collect()
}

/// Apply `desc` offline to `input` (one channel per vec), through the host, then tear down.
fn apply(
    m: &mut PluginManager,
    desc: &RackDesc,
    input: &[Vec<f32>],
) -> Result<Vec<Vec<f32>>, Failed> {
    let (mut rack, errors) = m
        .prepare_offline(desc, &cfg(input.len()))
        .poll()
        .ok_or("not ready")?
        .map_err(|e| e.to_string())?;
    if !errors.is_empty() {
        return Err(format!("slot errors: {errors:?}").into());
    }
    let mut chans = input.to_vec();
    let done = render_offline_with(
        &mut rack,
        &mut chans,
        &AtomicU32::new(0),
        &AtomicBool::new(false),
    );
    m.finish_offline(rack);
    if !done {
        return Err("render did not finish".into());
    }
    Ok(chans)
}

fn assert_close(a: &[f32], b: &[f32], tol: f32, what: &str) -> Res {
    if a.len() != b.len() {
        return Err(format!("{what}: lengths {} vs {}", a.len(), b.len()).into());
    }
    let worst = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max);
    if worst > tol {
        return Err(format!("{what}: differ by {worst} (> {tol})").into());
    }
    Ok(())
}

fn find_param(m: &PluginManager, slot: boothcraft_dsp::SlotId, name: &str) -> (u32, usize) {
    let p = m.params(slot).expect("loaded");
    let i = p
        .iter()
        .position(|p| p.name.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| {
            panic!(
                "no parameter {name:?} in {:?}",
                p.iter().map(|p| &p.name).collect::<Vec<_>>()
            )
        });
    (p[i].id, i)
}

fn t_probe_finds_effects() -> Res {
    let g = probe("again-sample-accurate.vst3");
    let d = probe("adelay.vst3");
    if g.is_empty() || d.len() != 1 {
        return Err(format!("expected effects, got {} and {}", g.len(), d.len()).into());
    }
    for x in g.iter().chain(&d) {
        if x.uid.len() != 32 || x.name.is_empty() || x.format != "vst3" {
            return Err(format!("bad descriptor {x:?}").into());
        }
    }
    Ok(())
}

fn t_gain_is_correct() -> Res {
    let d = probe("again-sample-accurate.vst3").remove(0);
    let mut m = manager(std::slice::from_ref(&d));
    let (mut desc, slot) = rack_of(&d);
    m.ensure_loaded(&slot).map_err(|e| e.to_string())?;
    let (gain_id, _) = find_param(&m, slot.id, "Gain");
    m.edit_param(slot.id, gain_id, 0.5);
    m.snapshot(&mut desc);
    let input = vec![signal(20_000), signal(20_000)];
    let out = apply(&mut m, &desc, &input)?;
    for c in 0..2 {
        let expect: Vec<f32> = input[c].iter().map(|x| x * 0.5).collect();
        assert_close(&out[c], &expect, 1e-5, "gain 0.5")?;
    }
    Ok(())
}

fn t_edit_snapshot_reload_is_identical() -> Res {
    let d = probe("again-sample-accurate.vst3").remove(0);
    let (mut desc, slot) = rack_of(&d);
    let input = vec![signal(10_000), signal(10_000)];
    let first = {
        let mut m = manager(std::slice::from_ref(&d));
        m.ensure_loaded(&slot).map_err(|e| e.to_string())?;
        let (id, _) = find_param(&m, slot.id, "Gain");
        m.edit_param(slot.id, id, 0.25);
        m.snapshot(&mut desc);
        let out = apply(&mut m, &desc, &input)?;
        m.shutdown(Vec::new());
        out
    };
    // A fresh host (a new session) loading the saved rack must sound the same.
    let json = serde_json_round_trip(&desc);
    let mut m2 = manager(std::slice::from_ref(&d));
    let second = apply(&mut m2, &json, &input)?;
    for c in 0..2 {
        assert_close(&first[c], &second[c], 1e-6, "reloaded rack")?;
    }
    let expect: Vec<f32> = input[0].iter().map(|x| x * 0.25).collect();
    assert_close(&second[0], &expect, 1e-5, "edit survived the round trip")
}

/// The rack as it would be saved to disk and loaded in another session.
fn serde_json_round_trip(d: &RackDesc) -> RackDesc {
    let path = std::env::temp_dir().join(format!("bc-vst3-rack-{}.json", std::process::id()));
    boothcraft_dsp::preset::save_rack(&path, d).expect("save");
    let loaded = boothcraft_dsp::preset::load_rack(&path).expect("load");
    std::fs::remove_file(path).ok();
    loaded
}

fn t_mono_into_stereo_only_plugin() -> Res {
    let d = probe("again-sample-accurate.vst3").remove(0);
    let mut m = manager(std::slice::from_ref(&d));
    let (mut desc, slot) = rack_of(&d);
    m.ensure_loaded(&slot).map_err(|e| e.to_string())?;
    let (id, _) = find_param(&m, slot.id, "Gain");
    m.edit_param(slot.id, id, 0.5);
    m.snapshot(&mut desc);
    let input = vec![signal(8_000)];
    let out = apply(&mut m, &desc, &input)?;
    let expect: Vec<f32> = input[0].iter().map(|x| x * 0.5).collect();
    assert_close(&out[0], &expect, 1e-5, "mono through a stereo plugin")
}

fn t_block_size_does_not_change_the_sound() -> Res {
    let d = probe("adelay.vst3").remove(0);
    let (desc, slot) = rack_of(&d);
    let input = vec![signal(30_000), signal(30_000)];
    let whole = {
        let mut m = manager(std::slice::from_ref(&d));
        m.ensure_loaded(&slot).map_err(|e| e.to_string())?;
        apply(&mut m, &desc, &input)?
    };
    // The same rack in preview, fed random block sizes.
    let mut m = manager(std::slice::from_ref(&d));
    let mut b = m.build_preview(&desc, &cfg(2));
    if !b.errors.is_empty() {
        return Err(format!("{:?}", b.errors).into());
    }
    let mut out = input.clone();
    let mut at = 0;
    let mut seed = 12345u32;
    while at < 30_000 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let len = (1 + (seed >> 16) as usize % 700).min(30_000 - at);
        let [l, r] = &mut out[..] else { unreachable!() };
        b.rack
            .process(&mut [&mut l[at..at + len], &mut r[at..at + len]]);
        at += len;
    }
    let latency = b.rack.latency();
    m.retire(b.rack);
    if latency != 0 {
        return Err(format!("adelay reports latency {latency}").into());
    }
    for c in 0..2 {
        assert_close(&out[c], &whole[c], 1e-5, "random block sizes")?;
    }
    Ok(())
}

fn t_activate_deactivate_cycles() -> Res {
    let d = probe("adelay.vst3").remove(0);
    let mt = MainThread::init();
    let (tx, _rx) = std::sync::mpsc::channel();
    let ctx = HostContext::new(boothcraft_dsp::SlotId::fresh(), tx, Arc::new(|| {}));
    let mut inst = Vst3Format::new()
        .instantiate(&d, ctx, mt)
        .poll()
        .ok_or("pending")?
        .map_err(|e| e.to_string())?;
    for i in 0..100 {
        let pcfg = ProcessConfig {
            sample_rate: if i % 2 == 0 { 44_100.0 } else { 48_000.0 },
            max_block: 512,
            channels: 2,
            offline: i % 3 == 0,
        };
        let (proc, a) = inst
            .activate(mt, &pcfg)
            .map_err(|e| format!("cycle {i}: {e}"))?;
        if a.in_channels == 0 || a.out_channels == 0 {
            return Err(format!("cycle {i}: no channels").into());
        }
        inst.deactivate(mt, proc);
    }
    Ok(())
}

fn t_state_survives_a_new_instance() -> Res {
    let d = probe("adelay.vst3").remove(0);
    let mt = MainThread::init();
    let make = || {
        let (tx, _rx) = std::sync::mpsc::channel();
        let ctx = HostContext::new(boothcraft_dsp::SlotId::fresh(), tx, Arc::new(|| {}));
        Vst3Format::new()
            .instantiate(&d, ctx, mt)
            .poll()
            .expect("sync")
            .map_err(|e| e.to_string())
    };
    let mut a = make()?;
    let p = a.params();
    let delay = p
        .iter()
        .find(|p| p.name.to_lowercase().contains("delay"))
        .ok_or("no delay parameter")?;
    a.set_param(mt, delay.id, 0.7);
    let state = a
        .save_state(mt, boothcraft_plugin::StateKind::Document)
        .map_err(|e| e.to_string())?;
    let mut b = make()?;
    b.load_state(mt, &state).map_err(|e| e.to_string())?;
    let got = b
        .params()
        .iter()
        .find(|q| q.id == delay.id)
        .map(|q| q.current)
        .ok_or("param gone")?;
    if (got - 0.7).abs() > 1e-4 {
        return Err(format!("delay was {got} after reload, expected 0.7").into());
    }
    Ok(())
}

// ---- the latency test plugin (built from this workspace) ---------------------------------------

/// Build the test plugin and wrap it in a bundle the platform loader accepts.
fn latency_bundle() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let release = !cfg!(debug_assertions);
    let mut cmd =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.args(["build", "-p", "boothcraft-vst3-test-plugin"]);
    if release {
        cmd.arg("--release");
    }
    assert!(
        cmd.status().expect("run cargo").success(),
        "building the test plugin"
    );
    let (prefix, ext) = if cfg!(windows) {
        ("", "dll")
    } else if cfg!(target_os = "macos") {
        ("lib", "dylib")
    } else {
        ("lib", "so")
    };
    let lib = target
        .join(if release { "release" } else { "debug" })
        .join(format!("{prefix}boothcraft_vst3_test_plugin.{ext}"));
    assert!(lib.exists(), "missing {}", lib.display());
    let bundle = target.join("vst3-fixtures/latency/LatencyTest.vst3");
    let _ = std::fs::remove_dir_all(&bundle);
    let contents = bundle.join("Contents");
    if cfg!(target_os = "macos") {
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::copy(&lib, contents.join("MacOS/LatencyTest")).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>LatencyTest</string>
<key>CFBundleIdentifier</key><string>invalid.boothcraft.latencytest</string>
<key>CFBundleName</key><string>LatencyTest</string>
<key>CFBundlePackageType</key><string>BNDL</string>
<key>CFBundleVersion</key><string>1.0</string>
</dict></plist>"#,
        )
        .unwrap();
    } else if cfg!(windows) {
        let d = contents.join("x86_64-win");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::copy(&lib, d.join("LatencyTest.vst3")).unwrap();
    } else {
        let arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        let d = contents.join(format!("{arch}-linux"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::copy(&lib, d.join("LatencyTest.so")).unwrap();
    }
    bundle
}

fn latency_descriptor() -> PluginDescriptor {
    Vst3Format::new()
        .probe(&Candidate {
            path: latency_bundle(),
            known: None,
        })
        .expect("probe")
        .remove(0)
}

fn impulse_input() -> Vec<Vec<f32>> {
    let mut x = vec![0.0f32; 10_000];
    x[1000] = 1.0;
    x[5000] = -0.5;
    vec![x.clone(), x]
}

fn t_latency_is_compensated_in_apply() -> Res {
    let d = latency_descriptor();
    let mut m = manager(std::slice::from_ref(&d));
    let (desc, _) = rack_of(&d);
    let (rack, errors) = m
        .prepare_offline(&desc, &cfg(2))
        .poll()
        .ok_or("pending")?
        .map_err(|e| e.to_string())?;
    if !errors.is_empty() || rack.latency() != 64 {
        return Err(format!("errors {errors:?}, latency {}", rack.latency()).into());
    }
    m.finish_offline(rack);
    let input = impulse_input();
    let out = apply(&mut m, &desc, &input)?;
    for c in 0..2 {
        assert_close(&out[c], &input[c], 1e-6, "impulses land where they went in")?;
    }
    Ok(())
}

fn t_latency_change_reactivates() -> Res {
    use boothcraft_dsp::SlotError;
    let d = latency_descriptor();
    let mut m = manager(std::slice::from_ref(&d));
    let (mut desc, slot) = rack_of(&d);
    let b1 = m.build_preview(&desc, &cfg(2));
    if !b1.errors.is_empty() || b1.rack.latency() != 64 {
        return Err(format!(
            "first build: {:?}, latency {}",
            b1.errors,
            b1.rack.latency()
        )
        .into());
    }
    if m.pump().rebuild {
        return Err("nothing should have asked for a rebuild yet".into());
    }

    // The plugin asks to be restarted from inside the host's own call (re-entrantly).
    let (id, _) = find_param(&m, slot.id, "Latency");
    m.edit_param(slot.id, id, 0.5);
    if !m.pump().rebuild {
        return Err("kLatencyChanged did not request a rebuild".into());
    }
    let b2 = m.build_preview(&desc, &cfg(2));
    if b2.errors != vec![(0, SlotError::NotReady)] {
        return Err(format!(
            "expected the slot to wait for its processor: {:?}",
            b2.errors
        )
        .into());
    }
    m.retire(b1.rack);
    if !m.pump().rebuild {
        return Err("no rebuild once the processor was back".into());
    }
    let b3 = m.build_preview(&desc, &cfg(2));
    if !b3.errors.is_empty() || b3.rack.latency() != 128 {
        return Err(format!(
            "after restart: {:?}, latency {}",
            b3.errors,
            b3.rack.latency()
        )
        .into());
    }
    m.retire(b3.rack);

    // Apply picks up the same edit and still lines up.
    m.snapshot(&mut desc);
    let input = impulse_input();
    let out = apply(&mut m, &desc, &input)?;
    assert_close(&out[0], &input[0], 1e-6, "aligned at the new latency")
}

fn main() {
    let mut args = Arguments::from_args();
    // Everything on the main thread.
    args.test_threads = Some(1);
    let _ = Rack::new; // keep the dsp import honest if tests are trimmed
    let tests: Vec<(&str, TestFn)> = vec![
        ("probe_finds_effects", t_probe_finds_effects),
        ("gain_is_correct", t_gain_is_correct),
        (
            "edit_snapshot_reload_is_identical",
            t_edit_snapshot_reload_is_identical,
        ),
        (
            "mono_into_stereo_only_plugin",
            t_mono_into_stereo_only_plugin,
        ),
        (
            "block_size_does_not_change_the_sound",
            t_block_size_does_not_change_the_sound,
        ),
        (
            "activate_deactivate_100_cycles",
            t_activate_deactivate_cycles,
        ),
        (
            "state_survives_a_new_instance",
            t_state_survives_a_new_instance,
        ),
        (
            "latency_is_compensated_in_apply",
            t_latency_is_compensated_in_apply,
        ),
        ("latency_change_reactivates", t_latency_change_reactivates),
    ];
    let trials = tests
        .into_iter()
        .map(|(name, f)| Trial::test(name, f))
        .collect();
    libtest_mimic::run(&args, trials).exit();
}
