//! A scan worker over the mock format, for the scanning tests.

use boothcraft_plugin::mock::MockFormat;
use boothcraft_plugin::{maybe_run_scan_worker, PluginFormat};

fn main() {
    let formats: Vec<Box<dyn PluginFormat>> = vec![Box::new(MockFormat::new())];
    maybe_run_scan_worker(&formats);
    eprintln!("not started in scan worker mode");
    std::process::exit(2);
}
