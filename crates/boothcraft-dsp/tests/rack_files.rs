//! Rack files written before plugin hosting must keep loading unchanged.

use boothcraft_dsp::preset;
use std::path::Path;

#[test]
fn rack_v0_fixture_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rack_v0.json");
    let rack = preset::load_rack(&path).unwrap();
    assert_eq!(rack.slots.len(), 16);
    let amp = rack.slots[0].as_ref().unwrap();
    assert_eq!(
        (amp.effect.as_str(), amp.values.as_slice()),
        ("amplify", &[6.0][..])
    );
    assert!(amp.plugin.is_none() && amp.state.is_none());
    assert!(rack.slots[2].as_ref().unwrap().bypass);
    assert_eq!(
        (rack.input_gain_db, rack.output_gain_db, rack.mix),
        (-3.0, 1.5, 0.75)
    );
    assert!(!rack.has_plugins());
}
