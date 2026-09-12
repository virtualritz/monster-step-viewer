//! Bevy integration for the NSI (3Delight) renderer.
//!
//! Owns an `NsiRenderState` (when 3Delight is detected at startup),
//! pushes scene + camera + visibility updates from Bevy ECS. The render
//! itself runs asynchronously in 3Delight and is composited into the Bevy
//! viewport from its progressive pixel callbacks.
//!
//! Architecture:
//! * Geometry is pushed exactly once per shell, when the overlay is first
//!   enabled with a loaded scene (or when the scene reloads).
//! * Camera updates: `set_attribute` on the camera + `Synchronize`.
//! * Visibility updates: `set_attribute("visibility.*", ...)` on each changed
//!   face's attribute node + one batched `Synchronize`. No geometry re-push.

use crate::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_editor_cam::prelude::EditorCam;
use bevy_egui::egui;

use crate::{
    nsi_render::{NsiRenderState, detect_3delight},
    state::{
        IsoparamsMesh, MainCamera, PolygonEdgesMesh,
        RealtimeViewportSuppressed, ShellMesh, ViewerState,
    },
};
use monster_step_viewer::StepShell;

type RealtimeMeshQuery<'world, 'state> = Query<
    'world,
    'state,
    (Option<&'static ShellMesh>, &'static mut Visibility),
    Or<(With<ShellMesh>, With<PolygonEdgesMesh>, With<IsoparamsMesh>)>,
>;

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
    /// egui texture receiving the progressive 3Delight pixel callbacks.
    pub texture: Option<egui::TextureHandle>,
    /// Whether an NSI frame has reached the viewport at least once.
    pub has_viewport_frame: bool,
}

/// Paint the current 3Delight image in the 3D viewport beneath egui chrome.
pub(crate) fn draw_nsi_overlay(
    ui: &mut egui::Ui,
    overlay: &mut NsiOverlayState,
    rect: egui::Rect,
    content_top: f32,
) {
    if !overlay.enabled {
        return;
    }
    let Some(render) = overlay.render.as_ref() else {
        return;
    };
    let pixels_per_point = ui.pixels_per_point();
    render.set_overlay_resolution(
        (rect.width() * pixels_per_point).round() as u32,
        (rect.height() * pixels_per_point).round() as u32,
    );
    if let Some((pixels, width, height, first_row, row_count)) =
        render.take_overlay_rows()
    {
        let rows = egui::ColorImage::from_rgba_unmultiplied(
            [width, row_count],
            &pixels,
        );
        let texture_matches_image = overlay
            .texture
            .as_ref()
            .is_some_and(|texture| texture.size() == [width, height]);
        match &mut overlay.texture {
            Some(texture) if texture_matches_image => texture.set_partial(
                [0, first_row],
                rows,
                egui::TextureOptions::LINEAR,
            ),
            None => {
                let (pixels, width, height) = render.overlay_image();
                overlay.texture = Some(ui.ctx().load_texture(
                    "mstpv_nsi_overlay",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [width, height],
                        &pixels,
                    ),
                    egui::TextureOptions::LINEAR,
                ));
            }
            Some(texture) => {
                let (pixels, width, height) = render.overlay_image();
                texture.set(
                    egui::ColorImage::from_rgba_unmultiplied(
                        [width, height],
                        &pixels,
                    ),
                    egui::TextureOptions::LINEAR,
                );
            }
        }
        overlay.has_viewport_frame = true;
    }
    if let Some(texture) = &overlay.texture {
        let clip_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, content_top.max(rect.min.y)),
            rect.max,
        );
        ui.ctx()
            .layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("nsi_overlay"),
            ))
            .with_clip_rect(clip_rect)
            .image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(
                    egui::Pos2::ZERO,
                    egui::pos2(1.0, 1.0),
                ),
                egui::Color32::WHITE,
            );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
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
            .add_systems(Update, push_camera_to_nsi)
            .add_systems(
                Update,
                suppress_realtime_viewport
                    .after(crate::scene::apply_face_visibility)
                    .after(crate::scene::apply_polygon_edges_visibility)
                    .after(crate::scene::apply_isoparams_visibility)
                    .before(crate::scene::draw_gizmos),
            );
    }
}

/// Keep the real-time meshes out of the NSI image once the latter has pixels.
/// Rendering both cameras during interactive motion produces two differently
/// timed projections, which reads as flicker rather than an overlay.
fn suppress_realtime_viewport(
    overlay: Res<NsiOverlayState>,
    mut state: ResMut<ViewerState>,
    mut realtime_viewport: ResMut<RealtimeViewportSuppressed>,
    mut meshes: RealtimeMeshQuery,
    mut was_suppressed: Local<bool>,
) {
    let suppress = overlay.enabled && overlay.has_viewport_frame;
    realtime_viewport.0 = suppress;
    if suppress {
        meshes.iter_mut().for_each(|(_, mut visibility)| {
            *visibility = Visibility::Hidden;
        });
    } else if *was_suppressed {
        meshes
            .iter_mut()
            .filter_map(|(shell_mesh, visibility)| {
                shell_mesh.map(|shell_mesh| (shell_mesh, visibility))
            })
            .for_each(|(shell_mesh, mut visibility)| {
                let visible = state
                    .shells
                    .iter()
                    .find(|shell| shell.id == shell_mesh.shell_id)
                    .is_none_or(|shell| shell.visible);
                *visibility = if visible {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
            });
        state.visibility_changed = true;
    }
    *was_suppressed = suppress;
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

/// A shell's STEP assembly placement, or the identity when it has none.
fn shell_placement(shell: &StepShell) -> glam::Mat4 {
    shell
        .transform
        .as_ref()
        .map_or(glam::Mat4::IDENTITY, |transform| transform.to_mat4())
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
                // The BRep is in the shell's own coordinates; the mesh path
                // bakes the assembly placement into its vertices, so the
                // exporter has to apply it here or every shell renders at the
                // origin.
                render.update_shell_brep(
                    &key,
                    original,
                    matrix * shell_placement(shell),
                );
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
        render.set_face_visibilities(updates.iter().map(
            |(_, source_face_id, key, visible)| {
                (key.as_str(), *source_face_id, *visible)
            },
        ));
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
