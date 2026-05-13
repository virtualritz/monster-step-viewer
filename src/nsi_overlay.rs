//! Bevy integration for the NSI (3Delight) renderer.
//!
//! Owns an `NsiRenderState` (when 3Delight is detected at startup),
//! pushes scene + camera + visibility updates from Bevy ECS. The render
//! itself runs asynchronously in 3Delight's `idisplay` window — we
//! never read pixels back into Bevy.
//!
//! Architecture:
//! * Geometry is pushed exactly once per shell, when the overlay is first
//!   enabled with a loaded scene (or when the scene reloads).
//! * Camera updates: `set_attribute` on the camera + `Synchronize`.
//! * Visibility updates: `set_attribute("visibility.*", ...)` on each face's
//!   attribute node + `Synchronize`. No geometry re-push.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_editor_cam::prelude::EditorCam;

use crate::{
    nsi_render::{NsiRenderState, detect_3delight},
    state::{MainCamera, ViewerState},
};

/// Bevy resource holding NSI overlay state.
#[derive(Resource, Default)]
pub(crate) struct NsiOverlayState {
    pub enabled: bool,
    pub render: Option<NsiRenderState>,
    /// 3Delight library was found at startup.
    pub available: bool,
    /// Last shell-key set we pushed to NSI (so we know what to retain
    /// across scene reloads).
    pub last_pushed_keys: HashSet<String>,
    /// Scene-data pointer last pushed. `scene_data` is replaced wholesale
    /// on reload / re-tessellate, so a pointer-eq is enough to detect
    /// "fresh geometry".
    pub last_pushed_scene_ptr: usize,
    /// Per-face visibility state we last sent to NSI.
    ///
    /// Keyed by `(shell_id, source_face_id)`.
    pub last_pushed_face_visibility: HashMap<(usize, usize), bool>,
    /// Set by the egui toolbar when the user toggles the overlay on; the
    /// `init_nsi_render_state` Update system picks it up to lazy-init
    /// the NSI context.
    pub init_requested: bool,
}

impl NsiOverlayState {
    pub fn new() -> Self {
        Self::default()
    }
}

pub(crate) struct NsiOverlayPlugin;

impl Plugin for NsiOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(NsiOverlayState::new())
            .add_systems(Startup, detect_3delight_at_startup)
            .add_systems(Update, init_nsi_render_state)
            .add_systems(Update, push_scene_brep_to_nsi)
            .add_systems(Update, push_visibility_to_nsi)
            .add_systems(Update, push_camera_to_nsi);
    }
}

fn detect_3delight_at_startup(mut overlay: ResMut<NsiOverlayState>) {
    match detect_3delight() {
        Some(path) => {
            log::info!("NSI overlay: 3Delight found at {}", path.display());
            overlay.available = true;
        }
        None => {
            log::info!(
                "NSI overlay: 3Delight not found; overlay will be unavailable"
            );
            overlay.available = false;
        }
    }
}

/// Lazy-init the NSI context the first time the overlay is enabled.
fn init_nsi_render_state(mut overlay: ResMut<NsiOverlayState>) {
    if !overlay.init_requested {
        return;
    }
    overlay.init_requested = false;
    if overlay.render.is_some() || !overlay.available {
        return;
    }
    match NsiRenderState::new() {
        Ok(state) => {
            state.start();
            overlay.render = Some(state);
            // Force a scene re-push now that we have a context.
            overlay.last_pushed_scene_ptr = 0;
            overlay.last_pushed_keys.clear();
            overlay.last_pushed_face_visibility.clear();
        }
        Err(error) => {
            log::error!("NSI overlay: failed to init NsiRenderState: {error}");
            overlay.available = false;
            overlay.enabled = false;
        }
    }
}

fn scene_normalize_matrix(state: &ViewerState) -> glam::Mat4 {
    // Bevy world coords use `(p - center) * scale` per shell mesh; the same
    // transform must be applied to the BRep so it overlays cleanly.
    let scale = state.scene_scale;
    let center = state.scene_center;
    glam::Mat4::from_scale(glam::Vec3::splat(scale))
        * glam::Mat4::from_translation(glam::Vec3::new(
            -center.x, -center.y, -center.z,
        ))
}

/// Push BRep geometry exactly once per scene load. After this the only
/// per-frame work is camera/visibility set_attribute + Sync.
fn push_scene_brep_to_nsi(
    mut overlay: ResMut<NsiOverlayState>,
    state: Res<ViewerState>,
) {
    if !overlay.enabled || overlay.render.is_none() {
        return;
    }
    let Some(scene) = state.scene_data.as_ref() else {
        return;
    };

    let scene_ptr = scene as *const _ as usize;
    if scene_ptr == overlay.last_pushed_scene_ptr {
        return;
    }

    let matrix = scene_normalize_matrix(&state);

    let new_keys: HashSet<String> = {
        let render = overlay.render.as_ref().expect("checked above");
        let keys: HashSet<String> = scene
            .shells
            .iter()
            .filter_map(|shell| {
                let original = shell.original_shell.as_ref()?;
                let key = format!("shell_{}", shell.id);
                render.update_shell_brep(&key, original, matrix);
                Some(key)
            })
            .collect();
        render.hide_unretained_shells(&keys);
        keys
    };

    overlay.last_pushed_scene_ptr = scene_ptr;
    overlay.last_pushed_keys = new_keys;
    // New scene → reset visibility tracking; the next visibility-push
    // tick will catch each face up.
    overlay.last_pushed_face_visibility.clear();
}

/// Mirror per-face visibility flips to NSI via `visibility.*`.
///
/// Shell visibility is folded into each face's effective visibility.
fn push_visibility_to_nsi(
    mut overlay: ResMut<NsiOverlayState>,
    state: Res<ViewerState>,
) {
    if !overlay.enabled || overlay.render.is_none() {
        return;
    }
    if state.shells.is_empty() {
        return;
    }

    let shell_visibility: HashMap<usize, bool> = state
        .shells
        .iter()
        .map(|record| (record.id, record.visible))
        .collect();

    // Snapshot the diff while holding only an immutable view of overlay, then
    // drop that view before applying the changes.
    let updates: Vec<(usize, usize, String, bool)> = {
        let last_keys = &overlay.last_pushed_keys;
        let last_vis = &overlay.last_pushed_face_visibility;
        state
            .faces
            .iter()
            .filter_map(|face| {
                let key = format!("shell_{}", face.shell_id);
                if !last_keys.contains(&key) {
                    return None;
                }
                let shell_visible = shell_visibility
                    .get(&face.shell_id)
                    .copied()
                    .unwrap_or(true);
                let visible = face.visible && shell_visible;
                let visibility_key = (face.shell_id, face.source_face_id);
                (last_vis.get(&visibility_key).copied() != Some(visible))
                    .then_some((
                        face.shell_id,
                        face.source_face_id,
                        key,
                        visible,
                    ))
            })
            .collect()
    };

    if updates.is_empty() {
        return;
    }

    if let Some(render) = overlay.render.as_ref() {
        for (_, source_face_id, key, visible) in &updates {
            render.set_face_visibility(key, *source_face_id, *visible);
        }
    }
    for (shell_id, source_face_id, _, visible) in updates {
        overlay
            .last_pushed_face_visibility
            .insert((shell_id, source_face_id), visible);
    }
}

fn push_camera_to_nsi(
    overlay: Res<NsiOverlayState>,
    camera_query: Query<
        (&GlobalTransform, &Projection, &EditorCam),
        With<MainCamera>,
    >,
) {
    if !overlay.enabled {
        return;
    }
    let Some(render) = overlay.render.as_ref() else {
        return;
    };
    let Ok((camera_xform, projection, _editor_cam)) = camera_query.single()
    else {
        return;
    };

    let bevy_view = camera_xform.to_matrix().inverse();
    let view_matrix = glam::Mat4::from_cols_array(&bevy_view.to_cols_array());

    let (fov_y_degrees, near, far) = match projection {
        Projection::Perspective(p) => (p.fov.to_degrees(), p.near, p.far),
        Projection::Orthographic(_) => (45.0, 0.1, 100.0),
        Projection::Custom(_) => (45.0, 0.1, 100.0),
    };

    render.update_camera(view_matrix, fov_y_degrees, near, far);
}
