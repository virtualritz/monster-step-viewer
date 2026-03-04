use bevy::prelude::*;
use monster_step_viewer::{LoadMessage, StepMetadata, StepScene};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

pub(crate) const DEFAULT_PANEL_WIDTH: f32 = 340.0;
pub(crate) const DEFAULT_TESSELLATION_FACTOR: f64 = 0.001;
pub(crate) const AMBIENT_BRIGHTNESS: f32 = 200.0;
pub(crate) const KEY_LIGHT_ILLUMINANCE: f32 = 15000.0;
pub(crate) const BACK_LIGHT_ILLUMINANCE: f32 = 2000.0;
pub(crate) const MATERIAL_ROUGHNESS: f32 = 0.4;
pub(crate) const MATERIAL_METALLIC: f32 = 0.0;
pub(crate) const NEUTRAL_GRAY: [f32; 4] = [0.7, 0.7, 0.7, 1.0];

#[derive(Debug, Resource)]
pub(crate) struct ViewerState {
    pub pending_path: Option<PathBuf>,
    pub loaded_path: Option<PathBuf>,
    pub metadata: Option<StepMetadata>,
    pub shells: Vec<ShellRecord>,
    pub faces: Vec<FaceRecord>,
    pub error: Option<String>,
    pub loading_job: Option<LoadJob>,
    pub pending_bounds: Option<Bounds>,
    pub panel_width: f32,
    // Viewport overlay toggles.
    pub show_random_colors: bool,
    pub show_bounding_box: bool,
    pub show_wireframe: bool,
    pub scene_data: Option<StepScene>,
    pub needs_mesh_rebuild: bool,
    pub current_bounds: Option<Bounds>,
    /// Tessellation density factor (smaller = more triangles). Range: 0.0005 to 0.02.
    pub tessellation_factor: f64,
    /// Tessellation factor used for currently loaded scene (to detect changes).
    pub applied_tessellation_factor: f64,
    /// Flag to trigger visibility update (avoids costly is_changed() checks).
    pub visibility_changed: bool,
    /// Scene normalization: original center (for wireframe rendering).
    pub scene_center: Vec3,
    /// Scene normalization: scale factor (for wireframe rendering).
    pub scene_scale: f32,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self {
            pending_path: None,
            loaded_path: None,
            metadata: None,
            shells: Vec::new(),
            faces: Vec::new(),
            error: None,
            loading_job: None,
            pending_bounds: None,
            panel_width: DEFAULT_PANEL_WIDTH,
            show_random_colors: false,
            show_bounding_box: false,
            show_wireframe: false,
            scene_data: None,
            needs_mesh_rebuild: false,
            current_bounds: None,
            tessellation_factor: DEFAULT_TESSELLATION_FACTOR,
            applied_tessellation_factor: DEFAULT_TESSELLATION_FACTOR,
            visibility_changed: false,
            scene_center: Vec3::ZERO,
            scene_scale: 1.0,
        }
    }
}

#[derive(Debug)]
pub(crate) struct FaceRecord {
    pub id: usize,
    pub shell_id: usize,
    pub name: String,
    pub triangles: usize,
    pub visible: bool,
    pub ui_color: [f32; 3],
    pub mesh_handle: Handle<Mesh>,
}

#[derive(Debug)]
pub(crate) struct ShellRecord {
    pub id: usize,
    pub name: String,
    pub expanded: bool,
    // Indices into ViewerState.faces.
    pub face_ids: Vec<usize>,
}

#[derive(Component, Debug)]
pub(crate) struct FaceMesh {
    pub face_id: usize,
}

#[derive(Debug)]
pub(crate) struct LoadJob {
    pub path: PathBuf,
    pub receiver: Mutex<Receiver<LoadMessage>>,
    pub current_shell: usize,
    pub total_shells: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bounds {
    pub center: Vec3,
    pub min: Vec3,
    pub max: Vec3,
}

#[derive(Component)]
pub(crate) struct MainCamera;
