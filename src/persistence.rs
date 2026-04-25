use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Instant};

use crate::state::{
    AppMode, BrowserState, ClipPlaneState, DEFAULT_PANEL_WIDTH,
    DEFAULT_TESSELLATION_FACTOR, ShadingMode, ViewerState,
};

const APP_NAME: &str = "monster-step-viewer";
const SETTINGS_FILE: &str = "settings.json";
/// Minimum interval between saves in seconds.
const SAVE_DEBOUNCE_SECS: f32 = 2.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PersistentSettings {
    pub last_file_path: Option<PathBuf>,
    pub panel_width: f32,
    pub right_panel_width: f32,
    pub show_random_colors: bool,
    pub show_bounding_box: bool,
    #[serde(default)]
    pub show_polygon_edges: bool,
    pub tessellation_factor: f64,
    #[serde(default)]
    pub mode: AppMode,
    #[serde(default)]
    pub last_browser_dir: Option<PathBuf>,
    #[serde(default = "default_true", alias = "show_edges")]
    pub show_wireframe: bool,
    #[serde(default)]
    pub clip_planes: [ClipPlaneState; 3],
    #[serde(default)]
    pub shading_mode: ShadingMode,
}

fn default_true() -> bool {
    true
}

impl Default for PersistentSettings {
    fn default() -> Self {
        Self {
            last_file_path: None,
            panel_width: DEFAULT_PANEL_WIDTH,
            right_panel_width: 380.0,
            show_random_colors: false,
            show_bounding_box: false,
            show_polygon_edges: false,
            tessellation_factor: DEFAULT_TESSELLATION_FACTOR,
            mode: AppMode::default(),
            last_browser_dir: None,
            show_wireframe: true,
            clip_planes: [ClipPlaneState::default(); 3],
            shading_mode: ShadingMode::default(),
        }
    }
}

/// Return path to settings file.
fn settings_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_NAME).join(SETTINGS_FILE))
}

/// Load persistent settings from disk.
pub(crate) fn load_settings() -> PersistentSettings {
    settings_path()
        .and_then(|path| std::fs::read_to_string(&path).ok())
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

/// Save persistent settings to disk.
fn save_settings(settings: &PersistentSettings) {
    let Some(path) = settings_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(&path, json);
    }
}

#[derive(Resource)]
pub(crate) struct SaveTimer {
    last_save: Instant,
}

impl Default for SaveTimer {
    fn default() -> Self {
        Self {
            last_save: Instant::now(),
        }
    }
}

/// Debounced auto-save system that writes settings when dirty.
pub(crate) fn auto_save_system(
    mut state: ResMut<ViewerState>,
    browser: Res<BrowserState>,
    mut timer: ResMut<SaveTimer>,
) {
    if !state.settings_dirty {
        return;
    }
    if timer.last_save.elapsed().as_secs_f32() < SAVE_DEBOUNCE_SECS {
        return;
    }

    let settings = PersistentSettings {
        last_file_path: state.loaded_path.clone(),
        panel_width: state.panel_width,
        right_panel_width: state.right_panel_width,
        show_random_colors: state.show_random_colors,
        show_bounding_box: state.show_bounding_box,
        show_polygon_edges: state.show_polygon_edges,
        show_wireframe: state.show_wireframe,
        tessellation_factor: state.tessellation_factor,
        mode: state.mode,
        last_browser_dir: browser.selected_dir.clone(),
        clip_planes: state.clip_planes,
        shading_mode: state.shading_mode,
    };
    save_settings(&settings);
    timer.last_save = Instant::now();
    state.settings_dirty = false;
}
