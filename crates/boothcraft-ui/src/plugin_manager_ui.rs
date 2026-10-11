//! The Plugin Manager window: choose where to look, scan in the background, see what was found,
//! switch plugins off, and recover plugins that crashed or hung while scanning.

use boothcraft_plugin::{BlockReason, PluginManager, ScanReport};
use egui::{RichText, Ui};

#[derive(Default)]
pub struct PluginManagerPanel {
    pub open: bool,
    filter: String,
    /// Why a scan could not start, shown under the Scan button.
    notice: String,
}

impl PluginManagerPanel {
    /// A closed panel, or an open one (to show the crash prompt straight away).
    pub fn new(open: bool) -> PluginManagerPanel {
        PluginManagerPanel {
            open,
            ..Default::default()
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        plugins: &mut PluginManager,
        last_scan: Option<&ScanReport>,
    ) {
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Plugin Manager")
            .open(&mut open)
            .default_size([560.0, 520.0])
            .resizable(true)
            .show(ctx, |ui| {
                self.crash_prompt(ui, plugins);
                self.folders(ui, plugins);
                ui.separator();
                self.scan_row(ui, ctx, plugins, last_scan);
                ui.separator();
                self.blocked(ui, plugins);
                self.known(ui, plugins);
            });
        self.open = open;
    }

    /// Offered once after the app closed unexpectedly with plugins loaded.
    fn crash_prompt(&mut self, ui: &mut Ui, plugins: &mut PluginManager) {
        if plugins.unclean.is_empty() {
            return;
        }
        let names: Vec<String> = plugins.unclean.iter().map(|p| p.name.clone()).collect();
        ui.group(|ui| {
            ui.label(
                RichText::new("Boothcraft closed unexpectedly last time")
                    .strong()
                    .color(ui.visuals().warn_fg_color),
            );
            ui.label(format!(
                "These plugins were loaded when it happened: {}. One of them may be the cause.",
                names.join(", ")
            ));
            ui.horizontal(|ui| {
                if ui.button("Disable them").clicked() {
                    let ids: Vec<String> = plugins.unclean.iter().map(|p| p.effect_id()).collect();
                    plugins.disable(ids);
                    plugins.unclean.clear();
                }
                if ui.button("Keep them").clicked() {
                    plugins.unclean.clear();
                }
            });
        });
        ui.add_space(6.0);
    }

    fn folders(&mut self, ui: &mut Ui, plugins: &mut PluginManager) {
        ui.label(RichText::new("Folders to search").strong());
        let mut remove = None;
        for (i, p) in plugins.settings.extra_paths.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
                ui.label(p.display().to_string());
            });
        }
        if let Some(i) = remove {
            plugins.settings.extra_paths.remove(i);
            plugins.save_settings();
        }
        if plugins.settings.extra_paths.is_empty() && !plugins.settings.scan_standard_locations {
            ui.label(RichText::new("No folders yet.").weak());
        }
        ui.horizontal(|ui| {
            if ui.button("Add Folder…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    if !plugins.settings.extra_paths.contains(&dir) {
                        plugins.settings.extra_paths.push(dir);
                        plugins.save_settings();
                    }
                }
            }
        });
        let mut standard = plugins.settings.scan_standard_locations;
        if ui
            .checkbox(
                &mut standard,
                "Also search the standard system plugin folders",
            )
            .on_hover_text(
                "Every plugin installed on this Mac, which can be hundreds. Scanning them can take \
                 a long time. Off unless you want it.",
            )
            .changed()
        {
            plugins.settings.scan_standard_locations = standard;
            plugins.save_settings();
        }
    }

    fn scan_row(
        &mut self,
        ui: &mut Ui,
        ctx: &egui::Context,
        plugins: &mut PluginManager,
        last_scan: Option<&ScanReport>,
    ) {
        let scanning = plugins.is_scanning();
        let has_places =
            !plugins.settings.extra_paths.is_empty() || plugins.settings.scan_standard_locations;
        ui.horizontal(|ui| {
            let tip = if has_places {
                "Look for plugins in the folders above. Plugins already scanned and unchanged are skipped."
            } else {
                "Add a folder first"
            };
            if ui
                .add_enabled(!scanning && has_places, egui::Button::new("Scan"))
                .on_hover_text(tip)
                .clicked()
            {
                match plugins.scan_config() {
                    Ok(cfg) => {
                        self.notice.clear();
                        plugins.start_scan(cfg);
                    }
                    Err(e) => self.notice = format!("Could not start the scan: {e}"),
                }
            }
            if scanning {
                let (done, total) = plugins.scan_progress();
                let frac = if total == 0 {
                    0.0
                } else {
                    done as f32 / total as f32
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(220.0)
                        .text(format!("{done} of {total}")),
                );
                ui.label(RichText::new("Scanning in the background…").weak());
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        });
        if !self.notice.is_empty() {
            ui.label(RichText::new(&self.notice).color(ui.visuals().warn_fg_color));
        }
        if let (false, Some(r)) = (scanning, last_scan) {
            let mut line = format!(
                "Last scan: checked {} new or changed file(s), skipped {} already known, {} plugin(s) in total",
                r.probed, r.cached, r.found
            );
            if !r.blocked.is_empty() {
                line.push_str(&format!(", {} blocked", r.blocked.len()));
            }
            if !r.invalid.is_empty() {
                line.push_str(&format!(", {} not usable as effects", r.invalid.len()));
            }
            ui.label(RichText::new(line).weak());
        }
    }

    fn blocked(&mut self, ui: &mut Ui, plugins: &mut PluginManager) {
        let blocked = plugins.blocked();
        if blocked.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(RichText::new(format!("Blocked ({})", blocked.len())).strong())
            .default_open(true)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("These crashed or hung while being scanned and are skipped.")
                        .weak(),
                );
                for (path, why) in blocked {
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("Unblock")
                            .on_hover_text("Try this file again on the next scan")
                            .clicked()
                        {
                            plugins.unblock(&path);
                        }
                        let reason = match &why {
                            BlockReason::Crash(_) => "crashed",
                            BlockReason::Timeout => "hung",
                        };
                        let name = path
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                        ui.label(format!("{name} ({reason})"))
                            .on_hover_text(path.display().to_string());
                    });
                }
            });
    }

    fn known(&mut self, ui: &mut Ui, plugins: &mut PluginManager) {
        let mut by_vendor: std::collections::BTreeMap<String, Vec<(String, String, String, bool)>> =
            Default::default();
        let mut total = 0;
        for d in plugins.catalog().known() {
            let id = d.effect_id();
            let enabled = !plugins.settings.disabled.contains(&id);
            let vendor = if d.vendor.is_empty() {
                "Other".to_string()
            } else {
                d.vendor.clone()
            };
            by_vendor.entry(vendor).or_default().push((
                id,
                d.name.clone(),
                d.path.display().to_string(),
                enabled,
            ));
            total += 1;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Plugins found ({total})")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter")
                    .desired_width(160.0),
            );
        });
        if total == 0 {
            ui.label(RichText::new("None yet. Add a folder and press Scan.").weak());
            return;
        }
        let needle = self.filter.trim().to_lowercase();
        let mut toggle = None;
        egui::ScrollArea::vertical()
            .id_salt("plugin_manager_known")
            .auto_shrink([false, true])
            .max_height(300.0)
            .show(ui, |ui| {
                for (vendor, list) in &mut by_vendor {
                    list.sort_by_key(|p| p.1.to_lowercase());
                    let shown: Vec<_> = list
                        .iter()
                        .filter(|(_, name, _, _)| {
                            needle.is_empty()
                                || name.to_lowercase().contains(&needle)
                                || vendor.to_lowercase().contains(&needle)
                        })
                        .collect();
                    if shown.is_empty() {
                        continue;
                    }
                    egui::CollapsingHeader::new(format!("{vendor} ({})", shown.len()))
                        .id_salt(("pm_vendor", vendor.as_str()))
                        .default_open(!needle.is_empty() || by_vendor_small(total))
                        .show(ui, |ui| {
                            for (id, name, path, enabled) in shown {
                                let mut on = *enabled;
                                if ui.checkbox(&mut on, name).on_hover_text(path).changed() {
                                    toggle = Some((id.clone(), on));
                                }
                            }
                        });
                }
            });
        if let Some((id, on)) = toggle {
            plugins.set_enabled(&id, on);
        }
    }
}

/// With only a few plugins every vendor starts open; with many they start collapsed.
fn by_vendor_small(total: usize) -> bool {
    total <= 30
}
