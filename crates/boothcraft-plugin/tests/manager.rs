//! The plugin manager against the mock format: lifecycle, rebuilds, Apply, shutdown.
#![cfg(feature = "mock")]

use boothcraft_dsp::rack::render_offline_with;
use boothcraft_dsp::{PluginState, Rack, RackConfig, RackDesc, SlotDesc, SlotError, CARRY_NONE};
use boothcraft_plugin::mock::{descriptor, MockFormat, MockStats};
use boothcraft_plugin::{MainThread, PluginManager};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;

fn setup(uids: &[&str]) -> (PluginManager, Arc<MockStats>) {
    let mt = MainThread::init();
    let format = MockFormat::new();
    let stats = format.stats();
    let mut m = PluginManager::new(mt, vec![Box::new(format)], None, Arc::new(|| {}));
    for u in uids {
        m.register(descriptor(u));
    }
    (m, stats)
}

fn slot(uid: &str) -> SlotDesc {
    SlotDesc::for_plugin(descriptor(uid).plugin_ref(), Vec::new(), None)
}

fn gain_state(db: f32) -> PluginState {
    let mut v = b"MOCK".to_vec();
    v.extend_from_slice(&db.to_le_bytes());
    PluginState(v)
}

fn rack_with(slots: Vec<SlotDesc>) -> RackDesc {
    let mut d = RackDesc::default();
    for (i, s) in slots.into_iter().enumerate() {
        d.slots[i] = Some(s);
    }
    d
}

fn cfg(channels: usize, block: usize) -> RackConfig {
    RackConfig {
        sample_rate: 48_000.0,
        channels,
        max_block: block,
    }
}

fn run(rack: &mut Rack, input: &[f32]) -> Vec<f32> {
    let mut out = input.to_vec();
    rack.process(&mut [&mut out[..]]);
    out
}

#[test]
fn apply_compensates_the_plugins_latency() {
    let (mut m, stats) = setup(&["delay:64"]);
    let desc = rack_with(vec![slot("delay:64")]);
    let (mut rack, errors) = m
        .prepare_offline(&desc, &cfg(1, 4096))
        .poll()
        .unwrap()
        .unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(rack.latency(), 64, "latency is read after priming");

    // An impulse must come out exactly where it went in.
    let mut input = vec![0.0f32; 10_000];
    input[1000] = 1.0;
    input[5000] = -0.5;
    let mut chans = vec![input.clone()];
    assert!(render_offline_with(
        &mut rack,
        &mut chans,
        &AtomicU32::new(0),
        &AtomicBool::new(false)
    ));
    assert_eq!(chans[0], input);

    m.finish_offline(rack);
    assert_eq!(
        stats.live_instances.load(Relaxed),
        0,
        "offline instance torn down"
    );
    assert_eq!(stats.deactivations.load(Relaxed), 1);
    assert_eq!(stats.off_main_drops.load(Relaxed), 0);
}

#[test]
fn rebuilds_keep_the_one_instance() {
    let (mut m, stats) = setup(&["gain"]);
    let mut s = slot("gain");
    s.state = Some(gain_state(6.0));
    let mut desc = rack_with(vec![s]);

    let b1 = m.build_preview(&desc, &cfg(1, 256));
    assert!(b1.errors.is_empty(), "{:?}", b1.errors);
    assert!(b1.carry.iter().all(|&c| c == CARRY_NONE));
    let mut playing = b1.rack;
    let loud = run(&mut playing, &[0.25; 64]);
    assert!((loud[63] - 0.25 * 1.9953).abs() < 1e-3);

    // Edit the rack (a built-in effect joins) and rebuild: the plugin must carry over.
    desc.slots[1] = SlotDesc::new("amplify");
    let mut b2 = m.build_preview(&desc, &cfg(1, 256));
    assert_eq!(b2.carry[0], 0, "slot 0 takes the playing plugin");
    b2.rack.adopt(&mut playing, &b2.carry);
    m.retire(playing);
    let still = run(&mut b2.rack, &[0.25; 64]);
    assert!(
        (still[63] - loud[63]).abs() < 1e-6,
        "same live instance, same sound"
    );
    assert_eq!(stats.instantiations.load(Relaxed), 1);
    assert_eq!(stats.activations.load(Relaxed), 1);
    assert_eq!(stats.live_instances.load(Relaxed), 1);

    // Power off and on again: the processor is parked, then reused, not re-created.
    m.retire(b2.rack);
    m.preview_cleared();
    let b3 = m.build_preview(&desc, &cfg(1, 256));
    assert!(b3.errors.is_empty());
    assert_eq!(stats.instantiations.load(Relaxed), 1);
    assert_eq!(stats.activations.load(Relaxed), 1);
    assert_eq!(stats.off_main_drops.load(Relaxed), 0);
}

#[test]
fn a_new_configuration_reactivates_after_the_old_rack_is_retired() {
    let (mut m, stats) = setup(&["gain"]);
    let desc = rack_with(vec![slot("gain")]);
    let mut playing = m.build_preview(&desc, &cfg(1, 256)).rack;

    // The document changed to stereo: the plugin is busy in the old rack, so the new one waits.
    let b = m.build_preview(&desc, &cfg(2, 256));
    assert_eq!(b.errors, vec![(0, SlotError::NotReady)]);
    assert!(!m.pump().rebuild);
    m.retire(playing);
    assert!(m.pump().rebuild, "processor is back; build again");
    let b = m.build_preview(&desc, &cfg(2, 256));
    assert!(b.errors.is_empty(), "{:?}", b.errors);
    assert_eq!(stats.activations.load(Relaxed), 2);
    assert_eq!(stats.deactivations.load(Relaxed), 1);
    playing = b.rack;
    drop(playing);
}

#[test]
fn a_restart_requested_from_inside_load_state_is_handled() {
    // `relatch` calls back into the host from inside `load_state`.
    let (mut m, stats) = setup(&["relatch"]);
    let mut s = slot("relatch");
    s.state = Some(gain_state(0.0));
    let desc = rack_with(vec![s]);

    let b1 = m.build_preview(&desc, &cfg(1, 256));
    assert!(b1.errors.is_empty(), "{:?}", b1.errors);
    assert_eq!(b1.rack.latency(), 64);
    let p = m.pump();
    assert!(p.rebuild, "the queued callback asks for a rebuild");

    // Rebuild: the plugin is playing, so it is released first, then re-activated.
    let b2 = m.build_preview(&desc, &cfg(1, 256));
    assert_eq!(b2.errors, vec![(0, SlotError::NotReady)]);
    m.retire(b1.rack);
    assert!(m.pump().rebuild);
    let b3 = m.build_preview(&desc, &cfg(1, 256));
    assert!(b3.errors.is_empty(), "{:?}", b3.errors);
    assert_eq!(b3.rack.latency(), 64);
    assert_eq!(stats.activations.load(Relaxed), 2);
    assert!(!m.pump().rebuild, "restart handled once");
}

#[test]
fn missing_plugins_pass_audio_and_are_reported() {
    let (mut m, _) = setup(&[]);
    let desc = rack_with(vec![slot("gain")]);
    let mut b = m.build_preview(&desc, &cfg(1, 64));
    assert!(matches!(b.errors.as_slice(), [(0, SlotError::Missing(id))] if id == "mock:gain"));
    assert_eq!(run(&mut b.rack, &[0.5; 8]), vec![0.5; 8]);
}

#[test]
fn bad_output_goes_dry_and_is_flagged() {
    let (mut m, _) = setup(&["nan"]);
    let s = slot("nan");
    let id = s.id;
    let mut b = m.build_preview(&rack_with(vec![s]), &cfg(1, 64));
    assert_eq!(run(&mut b.rack, &[0.5; 8]), vec![0.5; 8]);
    assert!(m.take_bad_output(id));
    assert!(!m.take_bad_output(id), "flag is cleared once read");
}

#[test]
fn mono_through_a_stereo_only_plugin() {
    let (mut m, _) = setup(&["gain-stereo"]);
    let mut s = slot("gain-stereo");
    s.state = Some(gain_state(20.0 * 2f32.log10()));
    let mut b = m.build_preview(&rack_with(vec![s]), &cfg(1, 64));
    let out = run(&mut b.rack, &[0.25; 16]);
    assert!(
        (out[10] - 0.5).abs() < 1e-5,
        "both channels heard the input: {}",
        out[10]
    );
}

#[test]
fn parameter_changes_reach_the_processor() {
    let (mut m, _) = setup(&["gain"]);
    let s = slot("gain");
    let id = s.id;
    let mut b = m.build_preview(&rack_with(vec![s]), &cfg(1, 64));
    assert_eq!(run(&mut b.rack, &[0.5; 8]), vec![0.5; 8]);
    b.rack.set_param(0, 0, -6.0206);
    m.edit_param(id, 7, -6.0206);
    let out = run(&mut b.rack, &[0.5; 8]);
    assert!((out[0] - 0.25).abs() < 1e-4);
    let mut desc = rack_with(vec![slot("gain")]);
    desc.slots[0].as_mut().unwrap().id = id;
    m.snapshot(&mut desc);
    let s = desc.slots[0].as_ref().unwrap();
    assert_eq!(s.values, vec![-6.0206]);
    assert_eq!(&s.state.as_ref().unwrap().0[..4], b"MOCK");
}

#[test]
fn async_instantiation_is_picked_up_later() {
    let (mut m, stats) = setup(&["async"]);
    let desc = rack_with(vec![slot("async")]);
    let b = m.build_preview(&desc, &cfg(1, 64));
    assert_eq!(b.errors, vec![(0, SlotError::NotReady)]);
    MockFormat::deliver_deferred();
    let b = m.build_preview(&desc, &cfg(1, 64));
    assert!(b.errors.is_empty(), "{:?}", b.errors);
    assert_eq!(stats.instantiations.load(Relaxed), 1);
}

#[test]
fn removed_slots_are_swept_once_parked() {
    let (mut m, stats) = setup(&["gain"]);
    let desc = rack_with(vec![slot("gain")]);
    let id = desc.slots[0].as_ref().unwrap().id;
    let playing = m.build_preview(&desc, &cfg(1, 64)).rack;

    // The slot is deleted while its processor is still on the audio thread: nothing is torn down
    // until it comes back.
    m.sweep(&HashSet::new());
    assert_eq!(stats.live_instances.load(Relaxed), 1);
    m.retire(playing);
    assert_eq!(stats.live_instances.load(Relaxed), 0);
    assert!(!m.is_loaded(id));
    assert_eq!(stats.off_main_drops.load(Relaxed), 0);
}

#[test]
fn shutdown_returns_processors_and_leaks_the_rest() {
    let (mut m, stats) = setup(&["gain"]);
    let desc = rack_with(vec![slot("gain")]);
    let playing = m.build_preview(&desc, &cfg(1, 64)).rack;
    m.shutdown(vec![playing]);
    assert_eq!(stats.live_instances.load(Relaxed), 0);
    assert_eq!(m.leaked, 0);

    // A processor that never comes back (the audio thread is wedged) is not dropped off-main.
    let (mut m, stats) = setup(&["gain"]);
    let _stuck = m.build_preview(&desc, &cfg(1, 64)).rack;
    m.shutdown(Vec::new());
    assert_eq!(m.leaked, 1);
    assert_eq!(
        stats.live_instances.load(Relaxed),
        1,
        "leaked, not released"
    );
}

#[test]
fn slot_ids_are_distinct_and_survive_clone() {
    let a = slot("gain");
    assert_eq!(a.clone().id, a.id);
    assert_ne!(slot("gain").id, a.id);
}
