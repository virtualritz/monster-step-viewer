use bevy::prelude::*;
use monster_step_viewer::{
    LoadMessage, LoadPhase, StepMetadata, StepScene, StepShell,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc::Receiver},
};

pub(crate) const DEFAULT_PANEL_WIDTH: f32 = 340.0;
pub(crate) const DEFAULT_TESSELLATION_FACTOR: f64 = 0.001;
pub(crate) const PREVIEW_TESSELLATION_FACTOR: f64 = 0.01;
pub(crate) const PREVIEW_SIZE: u32 = 256;
pub(crate) const MAX_RENDER_SLOTS: usize = 20;
pub(crate) const AMBIENT_BRIGHTNESS: f32 = 200.0;
pub(crate) const KEY_LIGHT_ILLUMINANCE: f32 = 15000.0;
pub(crate) const BACK_LIGHT_ILLUMINANCE: f32 = 2000.0;
pub(crate) const MATERIAL_ROUGHNESS: f32 = 0.4;
pub(crate) const MATERIAL_METALLIC: f32 = 0.0;
pub(crate) const NEUTRAL_GRAY: [f32; 4] = [0.7, 0.7, 0.7, 1.0];

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub(crate) enum AppMode {
    #[default]
    Viewer,
    Browser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClipPlaneState {
    pub enabled: bool,
    pub position: u16,
    pub flip: bool,
}

impl Default for ClipPlaneState {
    fn default() -> Self {
        Self {
            enabled: false,
            position: 500,
            flip: false,
        }
    }
}

impl ClipPlaneState {
    /// Get position as f32 in 0.0..=1.0 range.
    pub fn position_f32(&self) -> f32 {
        self.position as f32 / 1000.0
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub(crate) enum ShadingMode {
    #[default]
    Shaded,
    Flat,
    Matcap,
    XRay,
    Wireframe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    #[allow(dead_code)]
    Shell(usize),
    Face(usize),
    Edge(usize),
    Loop(usize),
}

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
    pub show_polygon_edges: bool,
    pub scene_data: Option<StepScene>,
    pub needs_mesh_rebuild: bool,
    pub current_bounds: Option<Bounds>,
    /// Tessellation density factor (smaller = more triangles). Range: 0.0005
    /// to 0.02.
    pub tessellation_factor: f64,
    /// Tessellation factor used for currently loaded scene (to detect
    /// changes).
    pub applied_tessellation_factor: f64,
    /// Flag to trigger visibility update (avoids costly is_changed() checks).
    pub visibility_changed: bool,
    /// Scene normalization: original center (for wireframe rendering).
    pub scene_center: Vec3,
    /// Scene normalization: scale factor (for wireframe rendering).
    pub scene_scale: f32,
    /// Right panel width for persistence.
    pub right_panel_width: f32,
    /// Flag indicating settings need saving.
    pub settings_dirty: bool,
    /// Whether egui fonts have been configured.
    pub fonts_configured: bool,
    /// Current app mode (Viewer or Browser).
    pub mode: AppMode,
    /// Edge records for STEP curve edges.
    pub edges: Vec<EdgeRecord>,
    /// Loop records for face boundary loops.
    pub loops: Vec<LoopRecord>,
    /// Global toggle for showing STEP curve wireframe.
    pub show_wireframe: bool,
    /// Flag to trigger edge visibility update.
    pub edge_visibility_changed: bool,
    /// Face ID needing re-tessellation (loop trim changed).
    pub retessellate_face: Option<usize>,
    /// Currently selected hierarchy item (highlighted in 3D view).
    pub selection: Option<Selection>,
    /// Previous selection (to detect changes and update materials).
    pub prev_selection: Option<Selection>,
    /// When true, selection was set from the viewport (click on mesh) — UI
    /// should expand the parent shell to reveal the selected face.
    pub selection_from_viewport: bool,
    /// Currently hovered hierarchy item (lighter highlight than selection).
    pub hover: Option<Selection>,
    /// Previous hover (to detect changes and update materials).
    pub prev_hover: Option<Selection>,
    /// Clip plane state for X, Y, Z axes.
    pub clip_planes: [ClipPlaneState; 3],
    /// Flag indicating clip plane uniforms need updating on materials.
    pub clip_planes_dirty: bool,
    /// Current shading mode.
    pub shading_mode: ShadingMode,
    /// Flag indicating shading mode changed and materials need updating.
    pub shading_mode_changed: bool,
    /// Previous shading mode (to detect transitions requiring mesh rebuilds).
    pub previous_shading_mode: ShadingMode,
    /// Flag indicating normals need rebuilding (flat <-> smooth transition).
    pub needs_normal_rebuild: bool,
    /// Whether any loaded shell has solid (manifold_solid_brep) topology,
    /// enabling the "Solidify Clip" feature.
    pub has_solid_topology: bool,
    /// Active solidify-clip background job.
    pub solidify_job: Option<SolidifyJob>,
    /// Flag to trigger solidify-clip computation.
    pub start_solidify: bool,
    /// Original scene data saved before solidify (for restoring on unclip).
    pub pre_solidify_scene: Option<StepScene>,
    /// Flag to trigger restore of pre-solidify scene.
    pub restore_original: bool,
    /// Whether the "Open URL" dialog is shown.
    pub show_url_dialog: bool,
    /// Text input for the URL dialog.
    pub url_input: String,
    /// In-flight URL fetch receiver.
    pub url_fetch: Option<Mutex<Receiver<Result<String, String>>>>,
    /// Downloaded STEP data ready to load.
    pub pending_url_data: Option<String>,
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
            show_polygon_edges: false,
            scene_data: None,
            needs_mesh_rebuild: false,
            current_bounds: None,
            tessellation_factor: DEFAULT_TESSELLATION_FACTOR,
            applied_tessellation_factor: DEFAULT_TESSELLATION_FACTOR,
            visibility_changed: false,
            scene_center: Vec3::ZERO,
            scene_scale: 1.0,
            right_panel_width: 380.0,
            settings_dirty: false,
            fonts_configured: false,
            mode: AppMode::default(),
            edges: Vec::new(),
            loops: Vec::new(),
            show_wireframe: true,
            edge_visibility_changed: false,
            retessellate_face: None,
            selection: None,
            prev_selection: None,
            selection_from_viewport: false,
            hover: None,
            prev_hover: None,
            clip_planes: [ClipPlaneState::default(); 3],
            clip_planes_dirty: false,
            shading_mode: ShadingMode::default(),
            shading_mode_changed: false,
            previous_shading_mode: ShadingMode::default(),
            needs_normal_rebuild: false,
            has_solid_topology: false,
            solidify_job: None,
            start_solidify: false,
            pre_solidify_scene: None,
            restore_original: false,
            show_url_dialog: false,
            url_input: String::new(),
            url_fetch: None,
            pending_url_data: None,
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
    pub material_handle: Handle<crate::viewer_material::ViewerMaterial>,
    /// Global edge IDs belonging to this face's boundary loops.
    pub edge_ids: Vec<usize>,
    /// Global loop IDs for this face.
    pub loop_ids: Vec<usize>,
}

#[derive(Debug)]
pub(crate) struct EdgeRecord {
    pub id: usize,
    #[allow(dead_code)]
    pub shell_id: usize,
    pub name: String,
    pub point_count: usize,
    pub visible: bool,
}

#[derive(Debug)]
pub(crate) struct LoopRecord {
    pub id: usize,
    pub face_id: usize,
    #[allow(dead_code)]
    pub shell_id: usize,
    pub is_outer: bool,
    pub edge_ids: Vec<usize>,
    pub trimming_active: bool,
}

#[derive(Debug)]
pub(crate) struct ShellRecord {
    pub id: usize,
    pub name: String,
    pub expanded: bool,
    /// Master visibility toggle for the entire shell.
    pub visible: bool,
    /// Number of faces that failed to tessellate.
    pub failed_faces: usize,
    // Indices into ViewerState.faces.
    pub face_ids: Vec<usize>,
    /// Edge IDs not referenced by any face boundary (standalone curves).
    pub standalone_edge_ids: Vec<usize>,
}

#[derive(Component, Debug)]
pub(crate) struct FaceMesh {
    pub face_id: usize,
}

/// Marker for the translucent 3D quad that visualises a clip plane.
#[derive(Component, Debug)]
pub(crate) struct ClipPlaneHandle {
    pub axis: usize, // 0=X, 1=Y, 2=Z
}

/// Resource tracking whether a clip-plane handle is being dragged.
/// While active the `PanOrbitCamera` is disabled.
#[derive(Resource, Default, Debug)]
pub(crate) struct ClipPlaneDragState {
    pub dragging: bool,
}

#[derive(Debug)]
pub(crate) struct LoadJob {
    pub path: PathBuf,
    pub receiver: Mutex<Receiver<LoadMessage>>,
    pub phase: LoadPhase,
    pub current_shell: usize,
    pub total_shells: usize,
}

/// Background job for solidify-clip boolean operation.
pub(crate) struct SolidifyJob {
    pub receiver: Mutex<Receiver<Result<StepScene, String>>>,
    pub cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for SolidifyJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolidifyJob").finish()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bounds {
    pub center: Vec3,
    pub min: Vec3,
    pub max: Vec3,
}

#[derive(Component)]
pub(crate) struct MainCamera;

// --- Browser mode types ---

#[derive(Debug)]
pub(crate) struct DirectoryEntry {
    pub path: PathBuf,
    pub name: String,
    pub expanded: bool,
    /// None = not yet scanned for subdirectories.
    pub children: Option<Vec<DirectoryEntry>>,
}

#[derive(Debug)]
pub(crate) enum PreviewStatus {
    Pending,
    Loading,
    Ready(PreviewData),
    Failed(String),
}

#[derive(Debug)]
pub(crate) struct PreviewData {
    pub shells: Vec<StepShell>,
    pub bounds_center: Vec3,
    pub bounds_scale: f32,
}

#[derive(Debug)]
pub(crate) struct PreviewEntry {
    pub path: PathBuf,
    pub filename: String,
    pub status: PreviewStatus,
}

/// Marker component for preview mesh entities.
#[derive(Component, Debug)]
pub(crate) struct PreviewMesh {
    pub slot: usize,
}

/// Marker component for preview cameras.
#[derive(Component, Debug)]
pub(crate) struct PreviewCamera {
    pub slot: usize,
}

/// Marker component for preview lights.
#[derive(Component, Debug)]
pub(crate) struct PreviewLight {
    pub slot: usize,
}

#[derive(Debug)]
pub(crate) struct RenderSlot {
    pub image: Handle<Image>,
    pub egui_texture_id: Option<egui::TextureId>,
    /// Index into BrowserState.previews that this slot is rendering.
    pub preview_index: Option<usize>,
    pub yaw: f32,
}

use bevy_egui::egui;

#[derive(Debug, Resource)]
pub(crate) struct BrowserState {
    pub root: PathBuf,
    pub tree: Vec<DirectoryEntry>,
    pub selected_dir: Option<PathBuf>,
    pub previews: Vec<PreviewEntry>,
    pub render_slots: Vec<RenderSlot>,
    /// Cancel flag for in-flight preview loads.
    pub cancel_flag: Arc<AtomicBool>,
    /// Receiver for completed preview loads.
    #[allow(clippy::type_complexity)]
    pub preview_receiver:
        Option<Mutex<Receiver<(usize, Result<PreviewData, String>)>>>,
    /// Scroll offset for virtualizing the grid.
    pub scroll_offset: f32,
    /// Number of visible rows (updated each frame from UI).
    pub visible_rows: usize,
    /// Number of grid columns (updated each frame from UI).
    pub grid_cols: usize,
    /// Thumbnail size in UI pixels.
    pub thumb_size: f32,
}
