//! A native macOS menu bar (`NSMenu`, via `muda`), offering the same actions as the in-window
//! menu bar (`App::menu`, used on every other platform). Built once at startup; checked/enabled
//! states are refreshed every frame via `sync`, and clicks are drained every frame via `poll` and
//! dispatched by the caller, which owns all the actual editing logic.
//!
//! Keyboard accelerators are deliberately not attached to these items: `App::handle_shortcuts`
//! already owns keyboard-triggered actions (Cmd+O, Cmd+S, Cmd+Z, ...). Attaching the same key
//! combo as a native menu key-equivalent risks the action firing twice, once from the OS
//! intercepting the key for the menu and once from our own `ctx.input_mut` consumption, depending
//! on exactly how the event gets routed — simplest and safest to have one owner.

use crate::effects_ui::effects_by_category;
use crate::ops::ZeroAdjust;
use crate::spectral::{self, ColorMap, FreqAxis, ViewMode};
use boothcraft_dsp::StftWindow;
use muda::{AboutMetadata, CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use std::collections::HashMap;

/// Labels for the Window menu's panel toggles, in the same order as the in-window version's list
/// (`Tab::Files, History, Selection, Markers, EffectsRack, PhaseMeter, Loudness, MatchLoudness,
/// Levels`). Kept as plain strings rather than depending on `Tab` (private to `app.rs`) so this
/// module stays decoupled from the app's internal types; the caller maps indices back to `Tab`.
pub const PANEL_LABELS: [&str; 9] = [
    "Files",
    "History",
    "Selection/View",
    "Markers",
    "Effects Rack",
    "Phase Meter",
    "Loudness",
    "Match Loudness",
    "Levels",
];

/// An action a native menu item dispatches to; the caller (`App`) matches on this and calls the
/// same logic the in-window menu's button closures call.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    NewFile,
    Open,
    /// Index into the recent-files list, as of the last `sync`.
    OpenRecent(usize),
    ClearRecent,
    Save,
    SaveAs,
    Close,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    MixPaste,
    Delete,
    CropToSelection,
    SelectAll,
    ZeroAdjust(ZeroAdjust),
    ToggleSnapEnabled,
    ToggleSnapMarkers,
    ToggleSnapRuler,
    ToggleSnapZeroCrossings,
    ConvertSampleType,
    Preferences,
    SelectClipboard(usize),
    SetViewMode(ViewMode),
    SetFftSize(usize),
    SetWindow(StftWindow),
    SetColorMap(ColorMap),
    SetFreqAxis(FreqAxis),
    Invert,
    Reverse,
    Silence,
    InsertSilence,
    OpenEffect(&'static str),
    GenTones,
    GenNoise,
    GenDtmf,
    GenSilence,
    /// Index into [`PANEL_LABELS`].
    TogglePanel(usize),
}

/// Current app state the native menu's checkmarks/enabled flags should reflect. Built fresh by the
/// caller every frame from its own (private) `State`.
pub struct SyncState<'a> {
    pub recording: bool,
    pub has_file: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_selection: bool,
    pub has_clip: bool,
    pub snap_enabled: bool,
    pub snap_markers: bool,
    pub snap_ruler: bool,
    pub snap_zero_crossings: bool,
    pub active_clipboard: usize,
    pub view_mode: ViewMode,
    pub fft_size: usize,
    pub window: StftWindow,
    pub color_map: ColorMap,
    pub freq_axis: FreqAxis,
    /// Same order/length as [`PANEL_LABELS`].
    pub panels_open: &'a [bool],
    /// Most-recently-opened first.
    pub recent_files: &'a [std::path::PathBuf],
}

pub struct NativeMenu {
    /// Kept alive for as long as `NativeMenu` is: `Menu`'s `Drop` impl clears
    /// `NSApp.mainMenu` if it's still the active one, so letting this drop at the end of
    /// `install()` (as a local variable) was tearing the menu bar right back out again.
    _menu: Menu,
    /// Every `Submenu` this module creates, kept alive for the same reason as `_menu`: a
    /// `Submenu`'s `Drop` impl removes it from whatever menu it's attached to
    /// (`remove_from_nsapp_menu_slots`), so a nested submenu (Zero Crossings, Snapping,
    /// Spectral Settings, each effect category, ...) built as a local variable and never stored
    /// anywhere would vanish from its parent the moment `install()` returned. Plain `MenuItem`/
    /// `CheckMenuItem`s don't have this problem (their `destroy()` is a no-op unless they're
    /// secretly a submenu), so only `Submenu`s need this; the ones also needed for `sync()` keep
    /// their own named field below in addition to living here.
    _submenus: Vec<Submenu>,
    dispatch: HashMap<muda::MenuId, Action>,
    top_level: Vec<Submenu>,
    undo: MenuItem,
    redo: MenuItem,
    cut: MenuItem,
    copy: MenuItem,
    paste: MenuItem,
    mix_paste: MenuItem,
    delete: MenuItem,
    crop: MenuItem,
    convert_sample_type: MenuItem,
    snap_enabled: CheckMenuItem,
    snap_markers: CheckMenuItem,
    snap_ruler: CheckMenuItem,
    snap_zero: CheckMenuItem,
    clipboards: Vec<CheckMenuItem>,
    view_modes: Vec<CheckMenuItem>,
    fft_sizes: Vec<CheckMenuItem>,
    windows: Vec<CheckMenuItem>,
    color_maps: Vec<CheckMenuItem>,
    freq_axes: Vec<CheckMenuItem>,
    panels: Vec<CheckMenuItem>,
    effects_menu: Submenu,
    generate_silence: MenuItem,
    recent_menu: Submenu,
    /// The recent-files list the "Open Recent" submenu currently reflects; rebuilt only when
    /// `sync`'s input differs from this, since rebuilding tears down and recreates native
    /// `NSMenuItem`s (wasteful, and a little janky, to do every frame for a menu that's shown
    /// rarely and changes even more rarely).
    last_recent: Vec<std::path::PathBuf>,
}

/// Build the native menu bar and install it as the app's menu (`NSApp.mainMenu`). Call once at
/// startup.
pub fn install() -> NativeMenu {
    let mut dispatch = HashMap::new();
    let mut register = |item: &dyn muda::IsMenuItem, action: Action| {
        dispatch.insert(item.id().clone(), action);
    };

    let menu = Menu::new();
    let mut submenus: Vec<Submenu> = Vec::new();

    let app_menu = Submenu::new("Boothcraft", true);
    submenus.push(app_menu.clone());
    app_menu
        .append_items(&[
            &PredefinedMenuItem::about(
                None,
                Some(AboutMetadata {
                    name: Some("Boothcraft".into()),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                    ..Default::default()
                }),
            ),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ])
        .expect("static menu construction");

    let file_menu = Submenu::new("File", true);
    submenus.push(file_menu.clone());
    let new_file = MenuItem::new("New…", true, None);
    let open = MenuItem::new("Open…", true, None);
    let recent_menu = Submenu::new("Open Recent", true);
    submenus.push(recent_menu.clone());
    let no_recent = MenuItem::new("No Recent Files", false, None);
    recent_menu
        .append(&no_recent)
        .expect("static menu construction");
    let save = MenuItem::new("Save", true, None);
    let save_as = MenuItem::new("Save As…", true, None);
    let close = MenuItem::new("Close", true, None);
    file_menu
        .append_items(&[
            &new_file,
            &open,
            &recent_menu,
            &save,
            &save_as,
            &PredefinedMenuItem::separator(),
            &close,
        ])
        .expect("static menu construction");
    register(&new_file, Action::NewFile);
    register(&open, Action::Open);
    register(&save, Action::Save);
    register(&save_as, Action::SaveAs);
    register(&close, Action::Close);

    let edit_menu = Submenu::new("Edit", true);
    submenus.push(edit_menu.clone());
    let undo = MenuItem::new("Undo", false, None);
    let redo = MenuItem::new("Redo", false, None);
    let cut = MenuItem::new("Cut", false, None);
    let copy = MenuItem::new("Copy", false, None);
    let paste = MenuItem::new("Paste", false, None);
    let mix_paste = MenuItem::new("Mix Paste", false, None);
    let delete = MenuItem::new("Delete", false, None);
    let crop = MenuItem::new("Crop to Selection", false, None);
    let select_all = MenuItem::new("Select All", true, None);
    let zero_crossings = Submenu::new("Zero Crossings", true);
    submenus.push(zero_crossings.clone());
    let mut zero_items = Vec::new();
    for (label, mode) in ZeroAdjust::ALL {
        let item = MenuItem::new(label, true, None);
        register(&item, Action::ZeroAdjust(mode));
        zero_items.push(item);
    }
    zero_crossings
        .append_items(
            &zero_items
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");
    let snap_enabled = CheckMenuItem::new("Snapping", true, true, None);
    let snap_markers = CheckMenuItem::new("Snap to Markers", true, true, None);
    let snap_ruler = CheckMenuItem::new("Snap to Ruler Ticks", true, false, None);
    let snap_zero = CheckMenuItem::new("Snap to Zero Crossings", true, false, None);
    let snapping = Submenu::new("Snapping", true);
    submenus.push(snapping.clone());
    snapping
        .append_items(&[&snap_enabled, &snap_markers, &snap_ruler, &snap_zero])
        .expect("static menu construction");
    let convert_sample_type = MenuItem::new("Convert Sample Type…", false, None);
    let preferences = MenuItem::new("Preferences…", true, None);
    let clipboard_menu = Submenu::new("Clipboard", true);
    submenus.push(clipboard_menu.clone());
    let mut clipboards = Vec::new();
    for i in 0..boothcraft_core::Clipboards::COUNT {
        let item = CheckMenuItem::new(format!("Clipboard {}", i + 1), true, i == 0, None);
        register(&item, Action::SelectClipboard(i));
        clipboards.push(item);
    }
    clipboard_menu
        .append_items(
            &clipboards
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");
    edit_menu
        .append_items(&[
            &undo,
            &redo,
            &PredefinedMenuItem::separator(),
            &cut,
            &copy,
            &paste,
            &mix_paste,
            &delete,
            &crop,
            &PredefinedMenuItem::separator(),
            &select_all,
            &zero_crossings,
            &snapping,
            &PredefinedMenuItem::separator(),
            &convert_sample_type,
            &preferences,
            &PredefinedMenuItem::separator(),
            &clipboard_menu,
        ])
        .expect("static menu construction");
    register(&undo, Action::Undo);
    register(&redo, Action::Redo);
    register(&cut, Action::Cut);
    register(&copy, Action::Copy);
    register(&paste, Action::Paste);
    register(&mix_paste, Action::MixPaste);
    register(&delete, Action::Delete);
    register(&crop, Action::CropToSelection);
    register(&select_all, Action::SelectAll);
    register(&snap_enabled, Action::ToggleSnapEnabled);
    register(&snap_markers, Action::ToggleSnapMarkers);
    register(&snap_ruler, Action::ToggleSnapRuler);
    register(&snap_zero, Action::ToggleSnapZeroCrossings);
    register(&convert_sample_type, Action::ConvertSampleType);
    register(&preferences, Action::Preferences);

    let view_menu = Submenu::new("View", true);
    submenus.push(view_menu.clone());
    let mut view_modes = Vec::new();
    for mode in ViewMode::ALL {
        let item = CheckMenuItem::new(mode.name(), true, mode == ViewMode::default(), None);
        register(&item, Action::SetViewMode(mode));
        view_modes.push(item);
    }
    let spectral_settings = Submenu::new("Spectral Settings", true);
    submenus.push(spectral_settings.clone());
    let fft_size_menu = Submenu::new("FFT Size", true);
    submenus.push(fft_size_menu.clone());
    let mut fft_sizes = Vec::new();
    for size in spectral::FFT_SIZES {
        let item = CheckMenuItem::new(size.to_string(), true, size == 2048, None);
        register(&item, Action::SetFftSize(size));
        fft_sizes.push(item);
    }
    fft_size_menu
        .append_items(
            &fft_sizes
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");
    let window_menu = Submenu::new("Window", true);
    submenus.push(window_menu.clone());
    let mut windows = Vec::new();
    for (label, w) in [
        ("Hann", StftWindow::Hann),
        ("Blackman-Harris", StftWindow::BlackmanHarris),
    ] {
        let item = CheckMenuItem::new(label, true, w == StftWindow::Hann, None);
        register(&item, Action::SetWindow(w));
        windows.push(item);
    }
    window_menu
        .append_items(
            &windows
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");
    let color_map_menu = Submenu::new("Colors", true);
    submenus.push(color_map_menu.clone());
    let mut color_maps = Vec::new();
    for m in ColorMap::ALL {
        let item = CheckMenuItem::new(m.name(), true, m == ColorMap::default(), None);
        register(&item, Action::SetColorMap(m));
        color_maps.push(item);
    }
    color_map_menu
        .append_items(
            &color_maps
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");
    let freq_axis_menu = Submenu::new("Frequency Axis", true);
    submenus.push(freq_axis_menu.clone());
    let mut freq_axes = Vec::new();
    for a in FreqAxis::ALL {
        let item = CheckMenuItem::new(a.name(), true, a == FreqAxis::default(), None);
        register(&item, Action::SetFreqAxis(a));
        freq_axes.push(item);
    }
    freq_axis_menu
        .append_items(
            &freq_axes
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");
    spectral_settings
        .append_items(&[
            &fft_size_menu,
            &window_menu,
            &color_map_menu,
            &freq_axis_menu,
        ])
        .expect("static menu construction");
    view_menu
        .append_items(
            &view_modes
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .chain(std::iter::once(&spectral_settings as &dyn muda::IsMenuItem))
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");

    let effects_menu = Submenu::new("Effects", false);
    submenus.push(effects_menu.clone());
    let invert = MenuItem::new("Invert", true, None);
    let reverse = MenuItem::new("Reverse", true, None);
    let silence = MenuItem::new("Silence", true, None);
    let insert_silence = MenuItem::new("Insert Silence…", true, None);
    effects_menu
        .append_items(&[
            &invert,
            &reverse,
            &silence,
            &insert_silence,
            &PredefinedMenuItem::separator(),
        ])
        .expect("static menu construction");
    register(&invert, Action::Invert);
    register(&reverse, Action::Reverse);
    register(&silence, Action::Silence);
    register(&insert_silence, Action::InsertSilence);
    for (cat, list) in effects_by_category(false) {
        let cat_menu = Submenu::new(cat.name(), true);
        submenus.push(cat_menu.clone());
        let mut items = Vec::new();
        for e in list {
            let label = if e.is_realtime() {
                e.name.to_string()
            } else {
                format!("{} (process)", e.name)
            };
            let item = MenuItem::new(label, true, None);
            register(&item, Action::OpenEffect(e.id));
            items.push(item);
        }
        cat_menu
            .append_items(
                &items
                    .iter()
                    .map(|i| i as &dyn muda::IsMenuItem)
                    .collect::<Vec<_>>(),
            )
            .expect("static menu construction");
        effects_menu
            .append(&cat_menu)
            .expect("static menu construction");
    }

    let generate_menu = Submenu::new("Generate", true);
    submenus.push(generate_menu.clone());
    let gen_tones = MenuItem::new("Tones…", true, None);
    let gen_noise = MenuItem::new("Noise…", true, None);
    let gen_dtmf = MenuItem::new("DTMF Tones…", true, None);
    let generate_silence = MenuItem::new("Silence…", false, None);
    generate_menu
        .append_items(&[&gen_tones, &gen_noise, &gen_dtmf, &generate_silence])
        .expect("static menu construction");
    register(&gen_tones, Action::GenTones);
    register(&gen_noise, Action::GenNoise);
    register(&gen_dtmf, Action::GenDtmf);
    register(&generate_silence, Action::GenSilence);

    let window_tabs_menu = Submenu::new("Window", true);
    submenus.push(window_tabs_menu.clone());
    let mut panels = Vec::new();
    for (i, label) in PANEL_LABELS.iter().enumerate() {
        let item = CheckMenuItem::new(*label, true, false, None);
        register(&item, Action::TogglePanel(i));
        panels.push(item);
    }
    window_tabs_menu
        .append_items(
            &panels
                .iter()
                .map(|i| i as &dyn muda::IsMenuItem)
                .collect::<Vec<_>>(),
        )
        .expect("static menu construction");

    let top_level = vec![
        file_menu.clone(),
        edit_menu.clone(),
        view_menu.clone(),
        effects_menu.clone(),
        generate_menu.clone(),
        window_tabs_menu.clone(),
    ];
    menu.append_items(&[
        &app_menu,
        &file_menu,
        &edit_menu,
        &view_menu,
        &effects_menu,
        &generate_menu,
        &window_tabs_menu,
    ])
    .expect("static menu construction");

    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();

    NativeMenu {
        _menu: menu,
        _submenus: submenus,
        dispatch,
        top_level,
        undo,
        redo,
        cut,
        copy,
        paste,
        mix_paste,
        delete,
        crop,
        convert_sample_type,
        snap_enabled,
        snap_markers,
        snap_ruler,
        snap_zero,
        clipboards,
        view_modes,
        fft_sizes,
        windows,
        color_maps,
        freq_axes,
        panels,
        effects_menu,
        generate_silence,
        recent_menu,
        last_recent: Vec::new(),
    }
}

impl NativeMenu {
    /// Drain every menu click that happened since the last call.
    pub fn poll(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            if let Some(action) = self.dispatch.get(&event.id) {
                actions.push(*action);
            }
        }
        actions
    }

    /// Refresh every checked/enabled state from the app's current state. Cheap (plain property
    /// setters on the native menu items); safe to call every frame. Rebuilds the "Open Recent"
    /// submenu too, but only when its contents actually changed.
    pub fn sync(&mut self, s: &SyncState<'_>) {
        if s.recent_files != self.last_recent.as_slice() {
            self.rebuild_recent(s.recent_files);
        }
        for sub in &self.top_level {
            sub.set_enabled(!s.recording);
        }
        self.undo.set_enabled(s.has_file && s.can_undo);
        self.redo.set_enabled(s.has_file && s.can_redo);
        self.cut.set_enabled(s.has_file && s.has_selection);
        self.copy.set_enabled(s.has_file && s.has_selection);
        self.paste.set_enabled(s.has_file && s.has_clip);
        self.mix_paste.set_enabled(s.has_file && s.has_clip);
        self.delete.set_enabled(s.has_file && s.has_selection);
        self.crop.set_enabled(s.has_file && s.has_selection);
        self.convert_sample_type.set_enabled(s.has_file);
        self.effects_menu.set_enabled(s.has_file);
        self.generate_silence.set_enabled(s.has_file);

        self.snap_enabled.set_checked(s.snap_enabled);
        self.snap_markers.set_checked(s.snap_markers);
        self.snap_markers.set_enabled(s.snap_enabled);
        self.snap_ruler.set_checked(s.snap_ruler);
        self.snap_ruler.set_enabled(s.snap_enabled);
        self.snap_zero.set_checked(s.snap_zero_crossings);
        self.snap_zero.set_enabled(s.snap_enabled);

        for (i, item) in self.clipboards.iter().enumerate() {
            item.set_checked(i == s.active_clipboard);
        }
        for (mode, item) in ViewMode::ALL.into_iter().zip(&self.view_modes) {
            item.set_checked(mode == s.view_mode);
        }
        for (size, item) in spectral::FFT_SIZES.into_iter().zip(&self.fft_sizes) {
            item.set_checked(size == s.fft_size);
        }
        for ((_, w), item) in [
            ("Hann", StftWindow::Hann),
            ("Blackman-Harris", StftWindow::BlackmanHarris),
        ]
        .into_iter()
        .zip(&self.windows)
        {
            item.set_checked(w == s.window);
        }
        for (m, item) in ColorMap::ALL.into_iter().zip(&self.color_maps) {
            item.set_checked(m == s.color_map);
        }
        for (a, item) in FreqAxis::ALL.into_iter().zip(&self.freq_axes) {
            item.set_checked(a == s.freq_axis);
        }
        for (open, item) in s.panels_open.iter().zip(&self.panels) {
            item.set_checked(*open);
        }
    }

    /// Clears and repopulates "Open Recent" from `paths`. Stale dispatch entries for the removed
    /// items are simply left orphaned in the map (harmless — it only grows by a handful of
    /// entries per file actually opened, not per frame).
    fn rebuild_recent(&mut self, paths: &[std::path::PathBuf]) {
        while self.recent_menu.remove_at(0).is_some() {}
        if paths.is_empty() {
            let placeholder = MenuItem::new("No Recent Files", false, None);
            self.recent_menu
                .append(&placeholder)
                .expect("menu mutation");
        } else {
            for (i, path) in paths.iter().enumerate() {
                let label = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let item = MenuItem::new(label, true, None);
                self.dispatch
                    .insert(item.id().clone(), Action::OpenRecent(i));
                self.recent_menu.append(&item).expect("menu mutation");
            }
            self.recent_menu
                .append(&PredefinedMenuItem::separator())
                .expect("menu mutation");
            let clear = MenuItem::new("Clear Recent", true, None);
            self.dispatch
                .insert(clear.id().clone(), Action::ClearRecent);
            self.recent_menu.append(&clear).expect("menu mutation");
        }
        self.last_recent = paths.to_vec();
    }
}
