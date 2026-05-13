use bevy::{
    asset::RenderAssetUsages,
    camera::{RenderTarget, visibility::RenderLayers},
    input::mouse::MouseWheel,
    mesh::Indices,
    picking::pointer::{PointerButton, PointerId, PointerLocation},
    prelude::*,
    render::render_resource::PrimitiveTopology,
    window::PrimaryWindow,
};
use bevy_editor_cam::{
    input::{CameraPointerMap, EditorCamInputMessage, MotionKind},
    prelude::{EditorCam, EnabledMotion},
};
use bevy_egui::{EguiContexts, EguiGlobalSettings, PrimaryEguiContext};
use monster_step_viewer::{
    LoadPhase, StepBounds, StepScene, StepShell, StepTopology,
};
use monstertruck::meshing::prelude::PolygonMesh;
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    f32::consts::{FRAC_PI_2, FRAC_PI_4, FRAC_PI_6, PI},
    path::PathBuf,
    sync::mpsc::TryRecvError,
    time::Instant,
};

use crate::{
    state::{
        AMBIENT_BRIGHTNESS, BACK_LIGHT_ILLUMINANCE, Bounds, ClipPlaneDragState,
        ClipPlaneHandle, EdgeRecord, FaceRecord, IsoparamsMaterial,
        IsoparamsMesh, KEY_LIGHT_ILLUMINANCE, LoadJob, LoopRecord, MainCamera,
        NEUTRAL_GRAY, PolygonEdgesMaterial, PolygonEdgesMesh, Selection,
        ShadingMode, ShellMesh, ShellRecord, ViewerState, ViewportClickGuard,
    },
    viewer_material::{
        ATTRIBUTE_FACE_ID, FACE_STATE_ANNOTATION_SHIFT, FACE_STATE_HIDDEN,
        FACE_STATE_HOVERED, FACE_STATE_SELECTED, FaceStateBuffer,
        MatcapTexture, MaterialPalette, SHADING_FLAG_FLAT, SHADING_FLAG_MATCAP,
        ViewerMaterial,
    },
};

pub(crate) fn setup_scene(
    mut commands: Commands,
    mut egui_global_settings: ResMut<EguiGlobalSettings>,
) {
    // Disable auto egui context - we create our own camera for it.
    egui_global_settings.auto_create_primary_context = false;

    // Ambient light - low for more contrast.
    commands.insert_resource(GlobalAmbientLight {
        color: Color::WHITE,
        brightness: AMBIENT_BRIGHTNESS,
        affects_lightmapped_meshes: false,
    });

    // Main 3D camera with lights as children (so lights move with camera).
    // Camera at ~2 units from origin for viewing unit-sized normalized scene.
    commands
        .spawn((
            MainCamera,
            Camera3d::default(),
            Transform::from_xyz(1.5, 1.0, 1.5).looking_at(Vec3::ZERO, Vec3::Y),
            EditorCam::default().with_initial_anchor_depth(2.0),
        ))
        .with_children(|parent| {
            // Key light - main directional light from top-left (relative to
            // camera).
            parent.spawn((
                DirectionalLight {
                    illuminance: KEY_LIGHT_ILLUMINANCE,
                    shadows_enabled: true,
                    ..Default::default()
                },
                Transform::from_rotation(Quat::from_euler(
                    EulerRot::YXZ,
                    PI * 0.25,
                    PI * -0.3,
                    0.0,
                )),
            ));

            // Back light - from bottom-right-back (relative to camera).
            parent.spawn((
                DirectionalLight {
                    illuminance: BACK_LIGHT_ILLUMINANCE,
                    shadows_enabled: false,
                    ..Default::default()
                },
                Transform::from_rotation(Quat::from_euler(
                    EulerRot::YXZ,
                    PI * -0.7,
                    PI * 0.15,
                    0.0,
                )),
            ));
        });

    // Egui-only camera for UI overlay.
    commands.spawn((
        PrimaryEguiContext,
        Camera3d::default(),
        RenderLayers::none(),
        Camera {
            order: 1,
            ..Default::default()
        },
    ));
}

pub(crate) fn editor_cam_mouse_inputs(
    pointers: Query<(&PointerId, &PointerLocation)>,
    pointer_map: Res<CameraPointerMap>,
    mut controller: MessageWriter<EditorCamInputMessage>,
    mut mouse_wheel: MessageReader<MouseWheel>,
    mouse_input: Res<ButtonInput<MouseButton>>,
    cameras: Query<
        (Entity, &Camera, &RenderTarget, &EditorCam),
        With<MainCamera>,
    >,
    primary_window: Query<Entity, With<PrimaryWindow>>,
) {
    let orbit_start = MouseButton::Right;
    let pan_start = MouseButton::Middle;
    let zoom_stop = 0.0;

    if let Some(&camera) = pointer_map.get(&PointerId::Mouse) {
        let camera_query = cameras.get(camera).ok();
        let is_in_zoom_mode = camera_query
            .map(|(.., editor_cam)| editor_cam.current_motion.is_zooming_only())
            .unwrap_or_default();
        let zoom_amount_abs = camera_query
            .and_then(|(.., editor_cam)| {
                editor_cam.current_motion.inputs().map(|inputs| {
                    inputs.zoom_velocity_abs(
                        editor_cam.smoothing.zoom.mul_f32(2.0),
                    )
                })
            })
            .unwrap_or(0.0);
        let should_zoom_end = is_in_zoom_mode && zoom_amount_abs <= zoom_stop;

        if mouse_input.any_just_released([orbit_start, pan_start])
            || should_zoom_end
        {
            controller.write(EditorCamInputMessage::End { camera });
        }
    }

    for (&pointer, pointer_location) in pointers
        .iter()
        .filter_map(|(id, loc)| loc.location().map(|loc| (id, loc)))
    {
        if !matches!(pointer, PointerId::Mouse) {
            continue;
        }

        let Some((camera, ..)) =
            cameras.iter().find(|(_, camera, render_target, _)| {
                pointer_location.is_in_viewport(
                    camera,
                    render_target,
                    &primary_window,
                )
            })
        else {
            continue;
        };

        if mouse_input.just_pressed(orbit_start) {
            controller.write(EditorCamInputMessage::Start {
                kind: MotionKind::OrbitZoom,
                camera,
                pointer,
            });
        } else if mouse_input.just_pressed(pan_start) {
            controller.write(EditorCamInputMessage::Start {
                kind: MotionKind::PanZoom,
                camera,
                pointer,
            });
        } else if mouse_wheel.read().map(|mw| mw.y.abs()).sum::<f32>() > 0.0 {
            controller.write(EditorCamInputMessage::Start {
                kind: MotionKind::Zoom,
                camera,
                pointer,
            });
        }
    }

    mouse_wheel.clear();
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn process_load_requests(
    mut commands: Commands,
    mut state: ResMut<ViewerState>,
    mut meshes: ResMut<Assets<Mesh>>,
    palette: Res<MaterialPalette>,
    edges_material: Res<PolygonEdgesMaterial>,
    isoparams_material: Res<IsoparamsMaterial>,
    existing_meshes: Query<
        Entity,
        Or<(With<ShellMesh>, With<PolygonEdgesMesh>, With<IsoparamsMesh>)>,
    >,
    clip_handles: Query<Entity, With<ClipPlaneHandle>>,
) {
    // Re-tessellate the cached scene if the slider was changed (no re-parse).
    if let Some(factor) = state.pending_retessellate.take()
        && state.loading_job.is_none()
        && let Some(scene) = state.scene_data.take()
    {
        let StepScene { metadata, shells } = scene;
        let path = state.loaded_path.clone().unwrap_or_default();

        let receiver =
            monster_step_viewer::retessellate_scene_streaming(shells, factor);

        for entity in existing_meshes.iter() {
            commands.entity(entity).despawn();
        }
        for entity in clip_handles.iter() {
            commands.entity(entity).despawn();
        }
        state.shells.clear();
        state.faces.clear();
        state.edges.clear();
        state.loops.clear();
        state.selection = None;
        state.prev_selection = None;
        state.error = None;

        // Keep metadata; shells will be repopulated as messages arrive.
        state.scene_data = Some(StepScene {
            metadata,
            shells: Vec::new(),
        });

        state.loading_job = Some(LoadJob {
            path,
            receiver: parking_lot::Mutex::new(receiver),
            phase: LoadPhase::Meshing,
            current_shell: 0,
            total_shells: 0,
        });
        info!(
            "Re-tessellating scene at factor {:.6} (cached parse reused)",
            factor
        );
        return;
    }

    // Determine the load source: local file path or fetched URL data.
    let load_source = if let Some(path) = state.pending_path.take() {
        let receiver = monster_step_viewer::load_step_file_streaming(
            path.clone(),
            state.tessellation_factor,
        );
        Some((path, receiver))
    } else if let Some(data) = state.pending_url_data.take() {
        let path = PathBuf::from("(URL)");
        let receiver = monster_step_viewer::load_step_from_string_streaming(
            data,
            state.tessellation_factor,
        );
        Some((path, receiver))
    } else {
        None
    };

    if let Some((path, receiver)) = load_source {
        for entity in existing_meshes.iter() {
            commands.entity(entity).despawn();
        }
        // Also remove clip-plane handles — they'll be re-created if needed.
        for entity in clip_handles.iter() {
            commands.entity(entity).despawn();
        }
        state.shells.clear();
        state.faces.clear();
        state.edges.clear();
        state.loops.clear();
        state.selection = None;
        state.prev_selection = None;
        state.metadata = None;
        state.loaded_path = None;
        state.error = None;
        state.scene_data = None;

        state.loading_job = Some(LoadJob {
            path,
            receiver: parking_lot::Mutex::new(receiver),
            phase: LoadPhase::Reading,
            current_shell: 0,
            total_shells: 0,
        });
        info!("Started loading STEP file");
    }

    // Poll the loading job for new messages.
    let Some(job) = state.loading_job.as_mut() else {
        return;
    };

    // Drain messages, but cap per-frame Shell processing so a flood of
    // streamed shells doesn't stall the main thread. Non-Shell messages are
    // cheap and keep flowing.
    const MAX_SHELLS_PER_FRAME: usize = 4;
    let (messages, disconnected): (Vec<_>, bool) = {
        let receiver = job.receiver.lock();
        let mut messages = Vec::new();
        let mut disconnected = false;
        let mut shells_this_frame = 0usize;

        loop {
            match receiver.try_recv() {
                Ok(msg) => {
                    let is_shell = matches!(
                        msg,
                        monster_step_viewer::LoadMessage::Shell(_)
                    );
                    messages.push(msg);
                    if is_shell {
                        shells_this_frame += 1;
                        if shells_this_frame >= MAX_SHELLS_PER_FRAME {
                            break;
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        (messages, disconnected)
    };

    for msg in messages {
        match msg {
            monster_step_viewer::LoadMessage::Phase(phase) => {
                if let Some(job) = state.loading_job.as_mut() {
                    job.phase = phase;
                    job.current_shell = 0;
                }
            }
            monster_step_viewer::LoadMessage::Bounds(bounds) => {
                apply_step_bounds_to_state(bounds, &mut state);
            }
            monster_step_viewer::LoadMessage::Metadata(meta) => {
                state.metadata = Some(meta);
            }
            monster_step_viewer::LoadMessage::TotalShells(total) => {
                if let Some(job) = state.loading_job.as_mut() {
                    job.total_shells = total;
                }
            }
            monster_step_viewer::LoadMessage::Progress {
                phase,
                current,
                total,
            } => {
                if let Some(job) = state.loading_job.as_mut() {
                    job.phase = phase;
                    job.current_shell = current;
                    job.total_shells = total;
                }
            }
            monster_step_viewer::LoadMessage::Shell(shell) => {
                if state.current_bounds.is_some() {
                    let center = state.scene_center;
                    let scale = state.scene_scale;
                    spawn_shell_faces_normalized(
                        &shell,
                        &mut commands,
                        &mut meshes,
                        &palette,
                        &edges_material,
                        &isoparams_material,
                        &mut state,
                        center,
                        scale,
                    );
                }

                if let Some(scene) = state.scene_data.as_mut() {
                    scene.shells.push(shell);
                } else {
                    state.scene_data = Some(StepScene {
                        metadata: state.metadata.clone().unwrap_or_default(),
                        shells: vec![shell],
                    });
                }
            }
            monster_step_viewer::LoadMessage::Done => {
                let path =
                    state.loading_job.as_ref().map(|job| job.path.clone());
                state.loaded_path = path;
                state.loading_job = None;

                if state.faces.is_empty() {
                    spawn_deferred_scene_after_load(
                        &mut commands,
                        &mut meshes,
                        &palette,
                        &edges_material,
                        &isoparams_material,
                        &mut state,
                    );
                }

                // Track the tessellation factor used for this load.
                state.applied_tessellation_factor = state.tessellation_factor;

                // Track whether any shell has solid topology.
                state.has_solid_topology =
                    state.scene_data.as_ref().is_some_and(|scene| {
                        scene.shells.iter().any(|s| {
                            matches!(s.topology, Some(StepTopology::Solid(_)))
                        })
                    });

                // Apply persisted clip plane / shading state to the newly
                // spawned materials.
                state.clip_planes_dirty = true;
                if state.shading_mode != ShadingMode::default() {
                    state.shading_mode_changed = true;
                }

                info!(
                    "Finished loading {} shells, {} faces, {} triangles (has_solid_topology={})",
                    state.shells.len(),
                    state.faces.len(),
                    state.faces.iter().map(|f| f.triangles).sum::<usize>(),
                    state.has_solid_topology,
                );
                return;
            }
            monster_step_viewer::LoadMessage::Error(err) => {
                state.error = Some(err);
                state.loading_job = None;
                return;
            }
        }
    }

    if disconnected {
        state.error = Some(
            "STEP loader stopped unexpectedly before completion".to_string(),
        );
        state.loading_job = None;
    }
}

fn apply_step_bounds_to_state(
    bounds: StepBounds,
    state: &mut ResMut<ViewerState>,
) {
    let center = Vec3::new(
        bounds.center[0] as f32,
        bounds.center[1] as f32,
        bounds.center[2] as f32,
    );
    let min = Vec3::new(
        bounds.min[0] as f32,
        bounds.min[1] as f32,
        bounds.min[2] as f32,
    );
    let max = Vec3::new(
        bounds.max[0] as f32,
        bounds.max[1] as f32,
        bounds.max[2] as f32,
    );
    let scale = bounds.normalization_scale() as f32;
    let normalized = Bounds {
        center: Vec3::ZERO,
        min: (min - center) * scale,
        max: (max - center) * scale,
    };

    state.scene_center = center;
    state.scene_scale = scale;
    state.current_bounds = Some(normalized);
    state.pending_bounds = Some(normalized);

    info!(
        "Scene bounds: center=({:.2}, {:.2}, {:.2}), scale={:.4}",
        center.x, center.y, center.z, scale
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_deferred_scene_after_load(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    palette: &MaterialPalette,
    edges_material: &PolygonEdgesMaterial,
    isoparams_material: &IsoparamsMaterial,
    state: &mut ResMut<ViewerState>,
) {
    let bounds = state.scene_data.as_ref().and_then(compute_bounds);
    if let Some(bounds) = bounds {
        let size = bounds.max - bounds.min;
        let max_dim = size.x.max(size.y).max(size.z);
        let scale = if max_dim > 0.0 { 1.0 / max_dim } else { 1.0 };
        state.scene_center = bounds.center;
        state.scene_scale = scale;
        state.current_bounds = Some(Bounds {
            center: Vec3::ZERO,
            min: (bounds.min - bounds.center) * scale,
            max: (bounds.max - bounds.center) * scale,
        });
        state.pending_bounds = state.current_bounds;

        if let Some(scene) = state.scene_data.take() {
            for shell in &scene.shells {
                spawn_shell_faces_normalized(
                    shell,
                    commands,
                    meshes,
                    palette,
                    edges_material,
                    isoparams_material,
                    state,
                    bounds.center,
                    scale,
                );
            }
            state.scene_data = Some(scene);
        }
    }
}

/// Build the merged mesh for a shell, spawn one entity for it, and register
/// face/edge/loop records for the UI hierarchy.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_shell_faces_normalized(
    shell: &StepShell,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    palette: &MaterialPalette,
    edges_material: &PolygonEdgesMaterial,
    isoparams_material: &IsoparamsMaterial,
    state: &mut ResMut<ViewerState>,
    scene_center: Vec3,
    scale: f32,
) {
    let base_face_id = state.faces.len();
    let face_ids: Vec<usize> = (0..shell.faces.len())
        .map(|idx| base_face_id + idx)
        .collect();

    let ShellBuildResult {
        mesh: merged_mesh,
        edges_mesh,
        per_face_tri_counts,
        per_face_ui_color,
        vertex_face_index,
    } = build_shell_merged_mesh(
        shell,
        base_face_id,
        state.show_random_colors,
        state.show_step_colors,
        scene_center,
        scale,
    );
    let mesh_handle = meshes.add(merged_mesh);
    let edges_mesh_handle = meshes.add(edges_mesh);
    let material_handle = palette.default.clone();

    commands.spawn((
        ShellMesh { shell_id: shell.id },
        Mesh3d(mesh_handle.clone()),
        MeshMaterial3d(material_handle.clone()),
        Transform::default(),
        Visibility::Visible,
    ));

    let initial_edges_visibility = if state.show_polygon_edges {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    commands.spawn((
        PolygonEdgesMesh { shell_id: shell.id },
        Mesh3d(edges_mesh_handle),
        MeshMaterial3d(edges_material.0.clone()),
        Transform::default(),
        initial_edges_visibility,
    ));

    // Isoparametric curves — sampled from each face's parametric surface.
    // Builds an empty mesh if the surfaces have no bounded parameter range
    // or if the original shell data isn't a `CompressedTrimmedShell`.
    let iso_t0 = Instant::now();
    let iso_polylines =
        monster_step_viewer::sample_shell_isoparams(shell, 4, 24);
    let iso_sampled = iso_t0.elapsed();
    let iso_mesh = build_isoparams_mesh(&iso_polylines, scene_center, scale);
    let iso_total = iso_t0.elapsed();
    log::info!(
        "iso build: shell {} faces={} polylines={} sample={}ms total={}ms",
        shell.id,
        shell.faces.len(),
        iso_polylines.len(),
        iso_sampled.as_millis(),
        iso_total.as_millis(),
    );
    let iso_mesh_handle = meshes.add(iso_mesh);
    let initial_iso_visibility = if state.show_isoparams {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    commands.spawn((
        IsoparamsMesh { shell_id: shell.id },
        Mesh3d(iso_mesh_handle),
        MeshMaterial3d(isoparams_material.0.clone()),
        Transform::default(),
        initial_iso_visibility,
    ));

    // Face records still feed the hierarchy panel; the per-face mesh handle is
    // empty because faces no longer own dedicated ECS meshes.
    for (idx, face) in shell.faces.iter().enumerate() {
        let global_face_id = base_face_id + idx;
        let tri_count = per_face_tri_counts.get(idx).copied().unwrap_or(0);
        let ui_color =
            per_face_ui_color.get(idx).copied().unwrap_or_else(|| {
                [NEUTRAL_GRAY[0], NEUTRAL_GRAY[1], NEUTRAL_GRAY[2]]
            });

        state.faces.push(FaceRecord {
            id: global_face_id,
            shell_id: shell.id,
            source_face_id: face.id,
            name: face.name.clone(),
            triangles: tri_count,
            visible: true,
            ui_color,
            edge_ids: Vec::new(),
            loop_ids: Vec::new(),
            annotation: Default::default(),
        });
    }

    // Register edge records for this shell's curve edges.
    let base_edge_id = state.edges.len();
    for (i, curve_edge) in shell.curve_edges.iter().enumerate() {
        let global_edge_id = base_edge_id + i;
        state.edges.push(EdgeRecord {
            id: global_edge_id,
            shell_id: shell.id,
            name: format!("Edge {} ({})", i + 1, curve_edge.curve_type),
            point_count: curve_edge.points.len(),
            visible: true,
        });
    }

    // Register loop records and link edges to faces.
    let mut referenced_edge_ids = HashSet::new();
    let mut face_edge_loop_data: Vec<(usize, Vec<usize>, Vec<usize>)> =
        Vec::new();

    for (idx, face) in shell.faces.iter().enumerate() {
        let global_face_id = base_face_id + idx;
        let mut face_edge_ids = Vec::new();
        let mut face_loop_ids = Vec::new();

        for (loop_idx, boundary_loop) in face.boundary_loops.iter().enumerate()
        {
            let global_loop_id = state.loops.len();
            let loop_edge_ids: Vec<usize> = boundary_loop
                .edge_indices
                .iter()
                .map(|&local_idx| base_edge_id + local_idx)
                .collect();

            for &eid in &loop_edge_ids {
                referenced_edge_ids.insert(eid);
            }
            face_edge_ids.extend(&loop_edge_ids);
            face_loop_ids.push(global_loop_id);

            state.loops.push(LoopRecord {
                id: global_loop_id,
                face_id: global_face_id,
                shell_id: shell.id,
                is_outer: loop_idx == 0,
                edge_ids: loop_edge_ids,
                trimming_active: true,
            });
        }

        face_edge_loop_data.push((
            global_face_id,
            face_edge_ids,
            face_loop_ids,
        ));
    }

    // Assign collected edge/loop data to face records (avoids overlapping
    // borrows).
    for (face_id, edge_ids, loop_ids) in face_edge_loop_data {
        state.faces[face_id].edge_ids = edge_ids;
        state.faces[face_id].loop_ids = loop_ids;
    }

    // Compute standalone edges (not referenced by any face boundary).
    let standalone_edge_ids: Vec<usize> = (base_edge_id
        ..base_edge_id + shell.curve_edges.len())
        .filter(|id| !referenced_edge_ids.contains(id))
        .collect();

    // Collapse shells by default; otherwise the egui hierarchy panel lays
    // out every face row (potentially thousands) every frame, which is the
    // dominant CPU cost for parts with many shells.
    state.shells.push(ShellRecord {
        id: shell.id,
        name: shell.name.clone(),
        expanded: false,
        visible: true,
        failed_faces: shell.failed_faces,
        face_ids,
        standalone_edge_ids,
        mesh_handle,
        vertex_face_index,
    });
    // Trigger a fresh upload of the per-face state buffer so newly-spawned
    // faces have an entry.
    state.face_state_visibility_dirty = true;
}

pub(crate) fn compute_bounds(scene: &StepScene) -> Option<Bounds> {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut has_points = false;

    for shell in &scene.shells {
        for face in &shell.faces {
            for p in face.mesh.positions() {
                let pos = Vec3::new(p.x as f32, p.y as f32, p.z as f32);
                min = min.min(pos);
                max = max.max(pos);
                has_points = true;
            }
        }
    }

    if !has_points {
        return None;
    }

    let center = (min + max) * 0.5;
    let size = max - min;
    log::info!(
        "Scene bounds: min=({:.2}, {:.2}, {:.2}), max=({:.2}, {:.2}, {:.2}), size=({:.2}, {:.2}, {:.2})",
        min.x,
        min.y,
        min.z,
        max.x,
        max.y,
        max.z,
        size.x,
        size.y,
        size.z
    );
    Some(Bounds { center, min, max })
}

/// Build the indexed (positions, normals, indices) geometry for one polygon
/// face mesh, normalised into scene space. Returned vertex buffers contain
/// only unique `(pos_idx, nor_idx)` pairs; indices are u32 triangle corners.
pub(crate) fn build_indexed_face_geometry(
    mesh: &PolygonMesh,
    scene_center: Vec3,
    scale: f32,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    // Apply normalization: (pos - center) * scale.
    let positions: Vec<[f32; 3]> = mesh
        .positions()
        .par_iter()
        .map(|p| {
            let pos = Vec3::new(p.x as f32, p.y as f32, p.z as f32);
            let normalized = (pos - scene_center) * scale;
            [normalized.x, normalized.y, normalized.z]
        })
        .collect();

    let normals: Vec<[f32; 3]> = mesh
        .normals()
        .par_iter()
        .map(|n| [n.x as f32, n.y as f32, n.z as f32])
        .collect();

    // Collect triangle corners as (pos_idx, nor_idx) tuples; tris/quads/ngons
    // are all triangulated here. Each branch produces a fixed-size array per
    // input face, so no per-face heap allocations.
    let tri_corners = mesh.tri_faces().iter().flat_map(|tri| {
        [
            (tri[0].pos, tri[0].nor),
            (tri[1].pos, tri[1].nor),
            (tri[2].pos, tri[2].nor),
        ]
    });
    let quad_corners = mesh.quad_faces().iter().flat_map(|quad| {
        [
            (quad[0].pos, quad[0].nor),
            (quad[1].pos, quad[1].nor),
            (quad[2].pos, quad[2].nor),
            (quad[0].pos, quad[0].nor),
            (quad[2].pos, quad[2].nor),
            (quad[3].pos, quad[3].nor),
        ]
    });
    let ngon_corners = mesh.other_faces().iter().flat_map(|face| {
        let first = face.first().map(|v| (v.pos, v.nor));
        face.windows(2).skip(1).flat_map(move |w| {
            first
                .into_iter()
                .chain([(w[0].pos, w[0].nor), (w[1].pos, w[1].nor)])
        })
    });
    let corners: Vec<(usize, Option<usize>)> = tri_corners
        .chain(quad_corners)
        .chain(ngon_corners)
        .collect();

    // Dedup corners into unique vertices, emitting one u32 index per corner.
    // Indexed geometry unlocks GPU vertex caching and ~halves vertex shading
    // work for typical triangle meshes (each vertex is shared by 4–6
    // triangles).
    let mut vertex_lookup: HashMap<(usize, Option<usize>), u32> =
        HashMap::with_capacity(corners.len() / 2);
    let mut unique_vertices: Vec<(usize, Option<usize>)> =
        Vec::with_capacity(corners.len() / 2);
    let indices: Vec<u32> = corners
        .iter()
        .map(|corner| {
            *vertex_lookup.entry(*corner).or_insert_with(|| {
                let i = unique_vertices.len() as u32;
                unique_vertices.push(*corner);
                i
            })
        })
        .collect();

    let vertex_positions: Vec<[f32; 3]> = unique_vertices
        .par_iter()
        .map(|(pos_idx, _)| positions[*pos_idx])
        .collect();
    let vertex_normals: Vec<[f32; 3]> = unique_vertices
        .par_iter()
        .map(|(_, nor_idx)| {
            nor_idx.map(|ni| normals[ni]).unwrap_or([0.0, 0.0, 1.0])
        })
        .collect();

    (vertex_positions, vertex_normals, indices)
}

/// Build a Bevy mesh from a single polygon face (kept for the
/// per-face mesh-rebuild paths used by retessellate / shading toggles).
/// Returns `(mesh, triangle_count)`.
pub(crate) fn bevy_mesh_from_polygon_normalized(
    mesh: &PolygonMesh,
    face_color_rgb: [f32; 3],
    apply_color: bool,
    scene_center: Vec3,
    scale: f32,
) -> (Mesh, usize) {
    let (positions, normals, indices) =
        build_indexed_face_geometry(mesh, scene_center, scale);
    let color = if apply_color {
        [face_color_rgb[0], face_color_rgb[1], face_color_rgb[2], 1.0]
    } else {
        NEUTRAL_GRAY
    };
    let mut colors: Vec<[f32; 4]> = vec![color; positions.len()];

    let (positions, normals, indices) =
        optimize_indexed_mesh(positions, normals, &mut colors, indices);
    let triangle_count = indices.len() / 3;

    let mut bevy_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    bevy_mesh.insert_indices(Indices::U32(indices));

    (bevy_mesh, triangle_count)
}

/// Build one merged mesh containing every face in the shell. Returns the
/// merged mesh, the polygon-edge `LineList` mesh, per-face triangle counts,
/// per-face ui_color, and the per-vertex global `face_id`.
pub(crate) struct ShellBuildResult {
    pub mesh: Mesh,
    pub edges_mesh: Mesh,
    pub per_face_tri_counts: Vec<usize>,
    pub per_face_ui_color: Vec<[f32; 3]>,
    pub vertex_face_index: Vec<u32>,
}

pub(crate) fn build_shell_merged_mesh(
    shell: &StepShell,
    base_face_id: usize,
    show_random: bool,
    show_step: bool,
    scene_center: Vec3,
    scale: f32,
) -> ShellBuildResult {
    let shell_color = shell.color;

    // Build each face's indexed geometry in parallel — these are independent
    // and meshopt-free at this stage. The merge below is a serial fold, but
    // it's just memcpy-shaped work.
    struct FaceChunk {
        positions: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
        indices: Vec<u32>,
        color: [f32; 4],
        ui_rgb: [f32; 3],
    }

    let chunks: Vec<FaceChunk> = shell
        .faces
        .par_iter()
        .enumerate()
        .map(|(idx, face)| {
            let global_face_id = base_face_id + idx;
            let step_color = face.color.or(shell_color);
            let (ui_rgb, apply_color) = face_display_color(
                global_face_id,
                step_color,
                show_random,
                show_step,
            );
            let (positions, normals, indices) =
                build_indexed_face_geometry(&face.mesh, scene_center, scale);
            let color = if apply_color {
                [ui_rgb[0], ui_rgb[1], ui_rgb[2], 1.0]
            } else {
                NEUTRAL_GRAY
            };
            FaceChunk {
                positions,
                normals,
                indices,
                color,
                ui_rgb,
            }
        })
        .collect();

    // Per-attribute concatenation: independent of order within and across
    // chunks (rayon-friendly). The chunks are small enough that par_iter
    // overhead may eat the win for small parts; on big STEP files with many
    // chunks it parallelises well.
    let mut merged_positions: Vec<[f32; 3]> = chunks
        .par_iter()
        .flat_map_iter(|c| c.positions.iter().copied())
        .collect();
    let mut merged_normals: Vec<[f32; 3]> = chunks
        .par_iter()
        .flat_map_iter(|c| c.normals.iter().copied())
        .collect();
    let mut merged_colors: Vec<[f32; 4]> = chunks
        .par_iter()
        .flat_map_iter(|c| std::iter::repeat_n(c.color, c.positions.len()))
        .collect();
    // Per-vertex global face_id. Goes both into the GPU as
    // `ATTRIBUTE_FACE_ID` for the shader to look up `face_state`, and into
    // the CPU `vertex_face_index` for click picking.
    let base = base_face_id as u32;
    let mut vertex_face_index: Vec<u32> = chunks
        .par_iter()
        .enumerate()
        .flat_map_iter(|(i, c)| {
            std::iter::repeat_n(base + i as u32, c.positions.len())
        })
        .collect();
    let per_face_tri_counts: Vec<usize> =
        chunks.par_iter().map(|c| c.indices.len() / 3).collect();
    let per_face_ui_color: Vec<[f32; 3]> =
        chunks.par_iter().map(|c| c.ui_rgb).collect();
    // Index offsets accumulate sequentially; can't be par_iter'd as a whole
    // without a prefix-sum pass. Compute per-chunk vertex offsets first
    // (sequential prefix sum, cheap), then map+collect indices in parallel.
    let mut chunk_vertex_offsets = Vec::with_capacity(chunks.len() + 1);
    let mut running = 0u32;
    chunks.iter().for_each(|c| {
        chunk_vertex_offsets.push(running);
        running += c.positions.len() as u32;
    });
    chunk_vertex_offsets.push(running);
    let mut merged_indices: Vec<u32> = chunks
        .par_iter()
        .zip(chunk_vertex_offsets.par_iter().copied())
        .flat_map_iter(|(c, off)| c.indices.iter().map(move |&i| i + off))
        .collect();

    // Apply meshopt's vertex-cache + vertex-fetch optimisations to the
    // merged mesh, threading vertex_face_index through the same fetch
    // remap so it stays in lockstep with the new vertex order.
    let vertex_count = merged_positions.len();
    if vertex_count > 0 && !merged_indices.is_empty() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            meshopt::optimize_vertex_cache_in_place(
                &mut merged_indices,
                vertex_count,
            );
            let remap = meshopt::optimize_vertex_fetch_remap(
                &merged_indices,
                vertex_count,
            );
            merged_positions = meshopt::remap_vertex_buffer(
                &merged_positions,
                vertex_count,
                &remap,
            );
            merged_normals = meshopt::remap_vertex_buffer(
                &merged_normals,
                vertex_count,
                &remap,
            );
            merged_colors = meshopt::remap_vertex_buffer(
                &merged_colors,
                vertex_count,
                &remap,
            );
            vertex_face_index = meshopt::remap_vertex_buffer(
                &vertex_face_index,
                vertex_count,
                &remap,
            );
            merged_indices = meshopt::remap_index_buffer(
                Some(&merged_indices),
                vertex_count,
                &remap,
            );
        }
    }

    // Polygon-edges LineList mesh — built once at shell spawn so the
    // edge-overlay toggle is a free visibility flip rather than per-frame
    // gizmo work. Positions are cloned (Bevy meshes own their attributes);
    // indices reference the same vertex buffer via shared positions.
    let edges_mesh =
        build_polygon_edges_mesh(merged_positions.clone(), &merged_indices);

    let mut bevy_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, merged_positions);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, merged_normals);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, merged_colors);
    bevy_mesh.insert_attribute(ATTRIBUTE_FACE_ID, vertex_face_index.clone());
    bevy_mesh.insert_indices(Indices::U32(merged_indices));

    ShellBuildResult {
        mesh: bevy_mesh,
        edges_mesh,
        per_face_tri_counts,
        per_face_ui_color,
        vertex_face_index,
    }
}

/// Reorder index and vertex buffers for better GPU cache behaviour. On
/// native: runs meshopt's vertex-cache then vertex-fetch optimisations. On
/// wasm meshopt isn't available; returns the inputs unchanged (they're still
/// indexed).
#[cfg(not(target_arch = "wasm32"))]
fn optimize_indexed_mesh(
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    mut indices: Vec<u32>,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    let vertex_count = positions.len();
    if vertex_count == 0 || indices.is_empty() {
        return (positions, normals, indices);
    }

    // 1) Reorder indices for vertex-cache locality.
    meshopt::optimize_vertex_cache_in_place(&mut indices, vertex_count);

    // 2) Compute a single fetch remap and apply it to every per-vertex
    //    attribute, then renumber the indices.
    let remap = meshopt::optimize_vertex_fetch_remap(&indices, vertex_count);
    let positions =
        meshopt::remap_vertex_buffer(&positions, vertex_count, &remap);
    let normals = meshopt::remap_vertex_buffer(&normals, vertex_count, &remap);
    *colors = meshopt::remap_vertex_buffer(colors, vertex_count, &remap);
    let indices =
        meshopt::remap_index_buffer(Some(&indices), vertex_count, &remap);

    (positions, normals, indices)
}

#[cfg(target_arch = "wasm32")]
fn optimize_indexed_mesh(
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    _colors: &mut Vec<[f32; 4]>,
    indices: Vec<u32>,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
    (positions, normals, indices)
}

/// Apply the per-shell visibility toggle. Per-face visibility is currently
/// inert because faces don't own ECS entities; restoring it requires the
/// shader-side mask that goes with the future GPU face-id pass.
pub(crate) fn apply_face_visibility(
    mut state: ResMut<ViewerState>,
    mut query: Query<(&ShellMesh, &mut Visibility)>,
) {
    if !state.visibility_changed {
        return;
    }
    state.visibility_changed = false;

    for (shell_mesh, mut visibility) in query.iter_mut() {
        let shell_visible = state
            .shells
            .iter()
            .find(|s| s.id == shell_mesh.shell_id)
            .is_none_or(|s| s.visible);
        *visibility = if shell_visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

/// Selection / hover / hidden state is driven by the GPU shader via the
/// `face_state` storage buffer in `ViewerMaterialExt` (see
/// `update_face_state_buffer`). This system only keeps the diff bookkeeping
/// (`prev_selection`, `prev_hover`) in sync.
pub(crate) fn apply_selection_highlight(mut state: ResMut<ViewerState>) {
    state.prev_selection = state.selection;
    state.prev_hover = state.hover;
}

/// Refresh the per-face state buffer whenever selection, hover, or
/// face-visibility state changes. The buffer is keyed by global face_id;
/// the shader looks each fragment's face up via the `face_id` vertex
/// attribute.
pub(crate) fn update_face_state_buffer(
    mut state: ResMut<ViewerState>,
    face_state_buffer: Res<FaceStateBuffer>,
    palette: Res<MaterialPalette>,
    mut storage_buffers: ResMut<
        Assets<bevy::render::storage::ShaderStorageBuffer>,
    >,
    mut materials: ResMut<Assets<ViewerMaterial>>,
) {
    let sel_changed = state.selection != state.prev_face_state_selection;
    let hov_changed = state.hover != state.prev_face_state_hover;
    let vis_changed = state.face_state_visibility_dirty;
    if !sel_changed && !hov_changed && !vis_changed {
        return;
    }

    let resolve = |sel: &Option<Selection>,
                   faces: &[FaceRecord],
                   loops: &[LoopRecord]|
     -> HashSet<usize> {
        match sel {
            Some(Selection::Face(fid)) => [*fid].into_iter().collect(),
            Some(Selection::Loop(lid)) => loops
                .iter()
                .find(|l| l.id == *lid)
                .map(|l| [l.face_id].into_iter().collect())
                .unwrap_or_default(),
            Some(Selection::Edge(eid)) => faces
                .iter()
                .find(|f| f.edge_ids.contains(eid))
                .map(|f| [f.id].into_iter().collect())
                .unwrap_or_default(),
            _ => HashSet::new(),
        }
    };
    let sel_faces = resolve(&state.selection, &state.faces, &state.loops);
    let hov_faces = resolve(&state.hover, &state.faces, &state.loops);

    let len = state.faces.len().max(1);
    let face_state: Vec<u32> = (0..len)
        .map(|fid| {
            let Some(face) = state.faces.get(fid) else {
                return 0;
            };
            let shell_visible = state
                .shells
                .iter()
                .find(|s| s.id == face.shell_id)
                .is_none_or(|s| s.visible);
            let face_visible = face.visible && shell_visible;
            let mut bits = 0u32;
            bits |=
                face.annotation.shader_bits() << FACE_STATE_ANNOTATION_SHIFT;
            if !face_visible {
                bits |= FACE_STATE_HIDDEN;
            }
            if sel_faces.contains(&fid) {
                bits |= FACE_STATE_SELECTED;
            }
            if hov_faces.contains(&fid) {
                bits |= FACE_STATE_HOVERED;
            }
            bits
        })
        .collect();

    if let Some(buffer) = storage_buffers.get_mut(&face_state_buffer.0) {
        buffer.set_data(face_state);
        // Touch every palette material so its bind group is rebuilt against
        // the (possibly resized) GPU storage buffer. Without this the bind
        // group keeps a stale reference and the shader reads the old data.
        for handle in
            [&palette.default, &palette.selected, &palette.hovered].into_iter()
        {
            let _ = materials.get_mut(handle);
        }
    }
    let _ = vis_changed;

    state.prev_face_state_selection = state.selection;
    state.prev_face_state_hover = state.hover;
    state.face_state_visibility_dirty = false;
}

pub(crate) fn normalize_scene_and_setup_camera(
    mut state: ResMut<ViewerState>,
    mut camera_query: Query<(&mut Transform, &mut EditorCam), With<MainCamera>>,
    mesh_query: Query<&Transform, (With<ShellMesh>, Without<MainCamera>)>,
) {
    let Some(bounds) = state.pending_bounds else {
        return;
    };

    // Wait until shell meshes are spawned (ECS delay between commands and
    // queries). One entity per shell.
    let mesh_count = mesh_query.iter().count();
    let expected_shells = state.shells.len();
    if mesh_count < expected_shells {
        return;
    }

    // Now we can consume pending_bounds.
    state.pending_bounds = None;

    // Store bounds for bounding box gizmo.
    state.current_bounds = Some(bounds);

    log::info!(
        "DEBUG: About to setup camera. Bounds center=({:.2}, {:.2}, {:.2}), max_dim={:.2}",
        bounds.center.x,
        bounds.center.y,
        bounds.center.z,
        max_dimension(bounds)
    );

    if let Ok((mut transform, mut editor_cam)) = camera_query.single_mut() {
        let camera_distance =
            frame_camera_initial(bounds, &mut transform, &mut editor_cam);

        log::info!(
            "Camera setup: focus=({:.2}, {:.2}, {:.2}), distance={:.2}",
            bounds.center.x,
            bounds.center.y,
            bounds.center.z,
            camera_distance
        );
    } else {
        state.pending_bounds = Some(bounds);
    }
}

/// Update vertex colors when a global color toggle changes. Walks each
/// shell's `vertex_face_index` lookup and writes the COLOR attribute slice
/// — no geometry rebuild, no meshopt — so toggles are O(vertices) and run
/// in milliseconds even on large parts.
pub(crate) fn rebuild_meshes_on_toggle(
    mut state: ResMut<ViewerState>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !state.needs_mesh_rebuild {
        return;
    }
    state.needs_mesh_rebuild = false;

    let show_random = state.show_random_colors;
    let show_step = state.show_step_colors;

    // Snapshot per-shell data needed in the parallel pass.
    struct ShellJob {
        mesh_handle: Handle<Mesh>,
        face_colors: Vec<[f32; 4]>,
        face_ui_rgb: Vec<[f32; 3]>,
        face_ids: Vec<usize>,
        vertex_face_index: Vec<u32>,
    }

    let jobs: Vec<ShellJob> = {
        let Some(scene) = state.scene_data.as_ref() else {
            return;
        };
        scene
            .shells
            .iter()
            .filter_map(|shell| {
                let record = state.shells.iter().find(|s| s.id == shell.id)?;
                let shell_color = shell.color;
                let face_colors_and_rgb: Vec<([f32; 4], [f32; 3])> = shell
                    .faces
                    .iter()
                    .zip(record.face_ids.iter())
                    .map(|(face, &gid)| {
                        let step_color = face.color.or(shell_color);
                        let (ui_rgb, apply) = face_display_color(
                            gid,
                            step_color,
                            show_random,
                            show_step,
                        );
                        let rgba = if apply {
                            [ui_rgb[0], ui_rgb[1], ui_rgb[2], 1.0]
                        } else {
                            NEUTRAL_GRAY
                        };
                        (rgba, ui_rgb)
                    })
                    .collect();
                let (face_colors, face_ui_rgb): (Vec<_>, Vec<_>) =
                    face_colors_and_rgb.into_iter().unzip();
                Some(ShellJob {
                    mesh_handle: record.mesh_handle.clone(),
                    face_colors,
                    face_ui_rgb,
                    face_ids: record.face_ids.clone(),
                    vertex_face_index: record.vertex_face_index.clone(),
                })
            })
            .collect()
    };

    // Build the new color buffers in parallel; commit on the main thread.
    type ShellColorUpdate =
        (Handle<Mesh>, Vec<[f32; 4]>, Vec<(usize, [f32; 3])>);
    let updates: Vec<ShellColorUpdate> = jobs
        .into_par_iter()
        .map(|job| {
            let new_colors: Vec<[f32; 4]> = job
                .vertex_face_index
                .iter()
                .map(|&fi| job.face_colors[fi as usize])
                .collect();
            let face_ui_updates: Vec<(usize, [f32; 3])> = job
                .face_ids
                .iter()
                .zip(job.face_ui_rgb)
                .map(|(&fid, rgb)| (fid, rgb))
                .collect();
            (job.mesh_handle, new_colors, face_ui_updates)
        })
        .collect();

    for (handle, colors, face_ui_updates) in updates {
        if let Some(mesh) = meshes.get_mut(&handle) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
        }
        for (fid, rgb) in face_ui_updates {
            if let Some(face) = state.faces.iter_mut().find(|f| f.id == fid) {
                face.ui_color = rgb;
            }
        }
    }
}

pub(crate) fn color_for_index(idx: usize) -> (Color, [f32; 3]) {
    use bevy::color::Hsva;
    // Use golden ratio for hue spread (in degrees for Hsva).
    let hue = (idx as f32 * 0.618_034 * 360.0) % 360.0;
    // Vary saturation and value to distinguish similar hues.
    // 0.5-0.9.
    let s = 0.5 + 0.4 * ((idx as f32 * 0.317) % 1.0);
    // 0.7-0.95.
    let v = 0.7 + 0.25 * ((idx as f32 * 0.513) % 1.0);
    let hsva = Hsva::new(hue, s, v, 1.0);
    let color = Color::from(hsva);
    let srgba = color.to_srgba();
    (color, [srgba.red, srgba.green, srgba.blue])
}

fn neutral_rgb() -> [f32; 3] {
    [NEUTRAL_GRAY[0], NEUTRAL_GRAY[1], NEUTRAL_GRAY[2]]
}

fn mix_step_and_random_color(
    step_color: [f32; 3],
    random_color: [f32; 3],
) -> [f32; 3] {
    [
        step_color[0] * 0.45 + random_color[0] * 0.55,
        step_color[1] * 0.45 + random_color[1] * 0.55,
        step_color[2] * 0.45 + random_color[2] * 0.55,
    ]
}

pub(crate) fn face_display_color(
    face_id: usize,
    step_color: Option<[f32; 3]>,
    use_random_colors: bool,
    use_step_colors: bool,
) -> ([f32; 3], bool) {
    match (use_random_colors, use_step_colors, step_color) {
        (false, false, _) => (neutral_rgb(), false),
        (false, true, Some(step_color)) => (step_color, true),
        (false, true, None) => (neutral_rgb(), false),
        (true, false, _) => {
            let (_, random_color) = color_for_index(face_id);
            (random_color, true)
        }
        (true, true, Some(step_color)) => {
            let (_, random_color) = color_for_index(face_id);
            (mix_step_and_random_color(step_color, random_color), true)
        }
        (true, true, None) => {
            let (_, random_color) = color_for_index(face_id);
            (random_color, true)
        }
    }
}

/// Gate `MeshPickingPlugin` so it only ray-casts while the left mouse button
/// is involved in a click or drag. Without this, every cursor move during
/// camera orbit/pan re-raycasts against every visible face mesh — for parts
/// with hundreds-thousands of faces that's the dominant frame cost.
///
/// The viewer's camera has no [`MeshPickingCamera`] marker, so flipping
/// `require_markers` to `true` makes the backend short-circuit before any
/// per-entity work. Click selection and clip-plane drag both use
/// [`PointerButton::Primary`], so re-enabling picking on left-button activity
/// is sufficient for both.
pub(crate) fn gate_picking_on_primary_button(
    mouse: Res<ButtonInput<MouseButton>>,
    mut settings: ResMut<bevy::picking::mesh_picking::MeshPickingSettings>,
) {
    let needs_picking = mouse.pressed(MouseButton::Left)
        || mouse.just_pressed(MouseButton::Left)
        || mouse.just_released(MouseButton::Left);
    settings.require_markers = !needs_picking;
}

/// Disable the editor camera when egui wants pointer input or a handle is being
/// dragged.
pub(crate) fn disable_camera_when_egui_wants_input(
    mut contexts: EguiContexts,
    mut camera_query: Query<&mut EditorCam, With<MainCamera>>,
    drag_state: Res<ClipPlaneDragState>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let egui_wants_input =
        ctx.wants_pointer_input() || ctx.is_pointer_over_area();

    if let Ok(mut editor_cam) = camera_query.single_mut() {
        let enabled = !egui_wants_input && !drag_state.dragging;
        editor_cam.enabled_motion = EnabledMotion {
            pan: enabled,
            orbit: enabled,
            zoom: enabled,
        };
        if !enabled {
            editor_cam.current_motion = Default::default();
        }
    }
}

/// Keyboard shortcuts for camera framing.
pub(crate) fn handle_view_shortcuts(
    mut contexts: EguiContexts,
    keyboard: Res<ButtonInput<KeyCode>>,
    state: Res<ViewerState>,
    meshes: Res<Assets<Mesh>>,
    mut camera_query: Query<(&mut Transform, &mut EditorCam), With<MainCamera>>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    if ctx.wants_keyboard_input() {
        return;
    }

    let reset_view = keyboard.just_pressed(KeyCode::KeyR);
    let center_selection = keyboard.just_pressed(KeyCode::KeyC);
    if !reset_view && !center_selection {
        return;
    }

    let Ok((mut transform, mut editor_cam)) = camera_query.single_mut() else {
        return;
    };

    if reset_view && let Some(bounds) = state.current_bounds {
        frame_camera_initial(bounds, &mut transform, &mut editor_cam);
    }

    if center_selection
        && let Some(bounds) = selected_face_bounds(&state, &meshes)
    {
        focus_camera_on_bounds(bounds, &mut transform, &mut editor_cam);
    }
}

fn selected_face_bounds(
    state: &ViewerState,
    meshes: &Assets<Mesh>,
) -> Option<Bounds> {
    let face_id =
        selected_face_id(state.selection, &state.faces, &state.loops)?;
    let face = state.faces.iter().find(|face| face.id == face_id)?;
    let shell = state
        .shells
        .iter()
        .find(|shell| shell.id == face.shell_id)?;
    let mesh = meshes.get(&shell.mesh_handle)?;
    let positions = mesh
        .attribute(Mesh::ATTRIBUTE_POSITION)
        .and_then(|attribute| attribute.as_float3())?;

    bounds_for_face_positions(positions, &shell.vertex_face_index, face_id)
}

fn selected_face_id(
    selection: Option<Selection>,
    faces: &[FaceRecord],
    loops: &[LoopRecord],
) -> Option<usize> {
    match selection {
        Some(Selection::Face(face_id)) => Some(face_id),
        Some(Selection::Loop(loop_id)) => loops
            .iter()
            .find(|boundary_loop| boundary_loop.id == loop_id)
            .map(|boundary_loop| boundary_loop.face_id),
        Some(Selection::Edge(edge_id)) => faces
            .iter()
            .find(|face| face.edge_ids.contains(&edge_id))
            .map(|face| face.id),
        Some(Selection::Shell(_)) | None => None,
    }
}

fn bounds_for_face_positions(
    positions: &[[f32; 3]],
    vertex_face_index: &[u32],
    face_id: usize,
) -> Option<Bounds> {
    let (min, max) = positions
        .iter()
        .zip(vertex_face_index.iter().copied())
        .filter_map(|(position, vertex_face_id)| {
            (vertex_face_id as usize == face_id)
                .then_some(Vec3::from(*position))
        })
        .fold(None::<(Vec3, Vec3)>, |bounds, position| {
            Some(match bounds {
                Some((min, max)) => (min.min(position), max.max(position)),
                None => (position, position),
            })
        })?;
    Some(Bounds {
        center: (min + max) * 0.5,
        min,
        max,
    })
}

fn max_dimension(bounds: Bounds) -> f32 {
    let size = bounds.max - bounds.min;
    size.x.max(size.y).max(size.z)
}

fn frame_distance_for_bounds(bounds: Bounds) -> f32 {
    (max_dimension(bounds) * 1.5).max(1.0e-3)
}

fn initial_camera_offset(distance: f32) -> Vec3 {
    let yaw = FRAC_PI_4;
    let pitch = FRAC_PI_6;
    Vec3::new(
        distance * yaw.cos() * pitch.cos(),
        distance * pitch.sin(),
        distance * yaw.sin() * pitch.cos(),
    )
}

fn update_editor_cam_anchor(editor_cam: &mut EditorCam, distance: f32) {
    editor_cam.last_anchor_depth = -(distance as f64).abs();
    editor_cam.current_motion = Default::default();
}

fn frame_camera_initial(
    bounds: Bounds,
    transform: &mut Transform,
    editor_cam: &mut EditorCam,
) -> f32 {
    let distance = frame_distance_for_bounds(bounds);
    transform.translation = bounds.center + initial_camera_offset(distance);
    *transform = transform.looking_at(bounds.center, Vec3::Y);
    update_editor_cam_anchor(editor_cam, distance);
    distance
}

fn focus_camera_on_bounds(
    bounds: Bounds,
    transform: &mut Transform,
    editor_cam: &mut EditorCam,
) -> f32 {
    let distance = (transform.translation - bounds.center)
        .length()
        .max(frame_distance_for_bounds(bounds));
    let forward = transform.forward().as_vec3();
    transform.translation = bounds.center - forward * distance;
    *transform = transform.looking_at(bounds.center, Vec3::Y);
    update_editor_cam_anchor(editor_cam, distance);
    distance
}

/// One-shot startup: create the shared `StandardMaterial` used by every
/// per-shell polygon-edges line-list entity (unlit black, double-sided).
pub(crate) fn setup_polygon_edges_material(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgba(0.0, 0.0, 0.0, 0.6),
        unlit: true,
        cull_mode: None,
        alpha_mode: AlphaMode::Blend,
        ..Default::default()
    });
    commands.insert_resource(PolygonEdgesMaterial(handle));
}

/// Build a `LineList` mesh whose segments are the unique edges of the given
/// triangle mesh (each edge appears once even when shared by two triangles).
fn build_polygon_edges_mesh(
    positions: Vec<[f32; 3]>,
    tri_indices: &[u32],
) -> Mesh {
    let edges: HashSet<(u32, u32)> = tri_indices
        .par_chunks_exact(3)
        .flat_map_iter(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
        .map(|(a, b)| if a < b { (a, b) } else { (b, a) })
        .collect();
    let line_indices: Vec<u32> =
        edges.into_iter().flat_map(|(a, b)| [a, b]).collect();

    let mut mesh =
        Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(line_indices));
    mesh
}

/// Toggle the per-shell polygon-edges entities' visibility based on
/// `state.show_polygon_edges` and the owning shell's own visibility. Skips
/// the write when the target value already matches — every frame's
/// `*vis = …` would otherwise trip `Changed<Visibility>` for hundreds of
/// entities, which Bevy then re-runs visibility propagation + frustum
/// culling for. That's the dominant cost in the steady-state frame budget.
pub(crate) fn apply_polygon_edges_visibility(
    state: Res<ViewerState>,
    mut query: Query<(&PolygonEdgesMesh, &mut Visibility)>,
) {
    let show = state.show_polygon_edges
        && state.shading_mode != ShadingMode::Wireframe;
    for (edges, mut vis) in query.iter_mut() {
        let shell_visible = state
            .shells
            .iter()
            .find(|s| s.id == edges.shell_id)
            .is_none_or(|s| s.visible);
        let target = if show && shell_visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != target {
            *vis = target;
        }
    }
}

/// Shared `StandardMaterial` used by every per-shell isoparams line-list
/// entity. Slightly lighter and bluer than polygon edges so the two
/// overlays read as different at a glance.
pub(crate) fn setup_isoparams_material(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgba(0.15, 0.45, 0.85, 0.7),
        unlit: true,
        cull_mode: None,
        alpha_mode: AlphaMode::Blend,
        ..Default::default()
    });
    commands.insert_resource(IsoparamsMaterial(handle));
}

/// Build a `LineList` mesh from a list of polylines (each polyline is a
/// world-space sequence of points). Points get normalised into the scene's
/// coordinate frame to match the shell mesh.
fn build_isoparams_mesh(
    polylines: &[Vec<[f64; 3]>],
    scene_center: Vec3,
    scale: f32,
) -> Mesh {
    // Per-polyline vertex offsets (sequential prefix sum) so we can
    // generate the line-list indices in parallel.
    let mut polyline_offsets: Vec<u32> =
        Vec::with_capacity(polylines.len() + 1);
    let mut running = 0u32;
    polylines.iter().for_each(|p| {
        polyline_offsets.push(running);
        running += p.len() as u32;
    });
    polyline_offsets.push(running);

    let positions: Vec<[f32; 3]> = polylines
        .par_iter()
        .flat_map_iter(|polyline| {
            polyline.iter().map(move |p| {
                let world = Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32);
                let n = (world - scene_center) * scale;
                [n.x, n.y, n.z]
            })
        })
        .collect();

    // For each polyline of N points, emit (N-1)*2 indices forming the line
    // segments p0-p1, p1-p2, ..., p(N-2)-p(N-1).
    let indices: Vec<u32> = polylines
        .par_iter()
        .zip(polyline_offsets.par_iter().copied())
        .flat_map_iter(|(polyline, offset)| {
            let len = polyline.len() as u32;
            (0..len.saturating_sub(1))
                .flat_map(move |i| [offset + i, offset + i + 1])
        })
        .collect();

    let mut mesh =
        Mesh::new(PrimitiveTopology::LineList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Toggle the per-shell isoparams entities' visibility based on
/// `state.show_isoparams` and the owning shell's own visibility. Same
/// no-write-when-unchanged guard as `apply_polygon_edges_visibility`.
pub(crate) fn apply_isoparams_visibility(
    state: Res<ViewerState>,
    mut query: Query<(&IsoparamsMesh, &mut Visibility)>,
) {
    let show = state.show_isoparams;
    for (iso, mut vis) in query.iter_mut() {
        let shell_visible = state
            .shells
            .iter()
            .find(|s| s.id == iso.shell_id)
            .is_none_or(|s| s.visible);
        let target = if show && shell_visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != target {
            *vis = target;
        }
    }
}

/// Configure gizmo rendering with depth bias so lines draw on top of meshes.
pub(crate) fn configure_gizmos(mut config_store: ResMut<GizmoConfigStore>) {
    let (config, _) = config_store.config_mut::<DefaultGizmoConfigGroup>();
    config.depth_bias = -0.001;
}

/// Draw bounding box and wireframe gizmos when enabled.
pub(crate) fn draw_gizmos(state: Res<ViewerState>, mut gizmos: Gizmos) {
    // Polygon edges are now drawn by per-shell `LineList` `Mesh3d` entities
    // (toggled via `apply_polygon_edges_visibility`); no per-frame gizmo
    // work here.

    // Draw STEP curve edges as blue polylines (highlighted if selected).
    if (state.show_wireframe || state.shading_mode == ShadingMode::Wireframe)
        && let Some(scene) = &state.scene_data
    {
        let edge_color = Color::srgba(0.2, 0.6, 1.0, 0.9);
        let highlight_color = Color::srgba(1.0, 0.85, 0.0, 1.0);
        let center = state.scene_center;
        let scale = state.scene_scale;
        let mut edge_offset = 0usize;

        // Precompute which edge IDs are highlighted by the current selection.
        let highlighted_edges: HashSet<usize> = match &state.selection {
            Some(Selection::Edge(eid)) => [*eid].into_iter().collect(),
            Some(Selection::Loop(lid)) => state
                .loops
                .iter()
                .find(|l| l.id == *lid)
                .map(|l| l.edge_ids.iter().copied().collect())
                .unwrap_or_default(),
            Some(Selection::Face(fid)) => state
                .faces
                .iter()
                .find(|f| f.id == *fid)
                .map(|f| f.edge_ids.iter().copied().collect())
                .unwrap_or_default(),
            _ => HashSet::new(),
        };

        for shell in &scene.shells {
            // Check if shell is visible.
            let shell_visible = state
                .shells
                .iter()
                .find(|s| s.id == shell.id)
                .is_none_or(|s| s.visible);

            if shell_visible {
                for curve_edge in &shell.curve_edges {
                    let global_edge_id = edge_offset + curve_edge.id;
                    let edge_visible = state
                        .edges
                        .get(global_edge_id)
                        .is_none_or(|e| e.visible);

                    if edge_visible {
                        let color =
                            if highlighted_edges.contains(&global_edge_id) {
                                highlight_color
                            } else {
                                edge_color
                            };
                        for window in curve_edge.points.windows(2) {
                            let p0_raw = Vec3::new(
                                window[0][0] as f32,
                                window[0][1] as f32,
                                window[0][2] as f32,
                            );
                            let p1_raw = Vec3::new(
                                window[1][0] as f32,
                                window[1][1] as f32,
                                window[1][2] as f32,
                            );
                            let p0 = (p0_raw - center) * scale;
                            let p1 = (p1_raw - center) * scale;
                            gizmos.line(p0, p1, color);
                        }
                    }
                }
            }
            edge_offset += shell.curve_edges.len();
        }
    }

    // Draw bounding box.
    if state.show_bounding_box
        && let Some(bounds) = state.current_bounds
    {
        let min = bounds.min;
        let max = bounds.max;
        // Green.
        let color = Color::srgb(0.0, 1.0, 0.0);

        // 12 edges of the bounding box.
        // Bottom face.
        gizmos.line(
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(max.x, min.y, min.z),
            color,
        );
        gizmos.line(
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(max.x, min.y, max.z),
            color,
        );
        gizmos.line(
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(min.x, min.y, max.z),
            color,
        );
        gizmos.line(
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(min.x, min.y, min.z),
            color,
        );
        // Top face.
        gizmos.line(
            Vec3::new(min.x, max.y, min.z),
            Vec3::new(max.x, max.y, min.z),
            color,
        );
        gizmos.line(
            Vec3::new(max.x, max.y, min.z),
            Vec3::new(max.x, max.y, max.z),
            color,
        );
        gizmos.line(
            Vec3::new(max.x, max.y, max.z),
            Vec3::new(min.x, max.y, max.z),
            color,
        );
        gizmos.line(
            Vec3::new(min.x, max.y, max.z),
            Vec3::new(min.x, max.y, min.z),
            color,
        );
        // Vertical edges.
        gizmos.line(
            Vec3::new(min.x, min.y, min.z),
            Vec3::new(min.x, max.y, min.z),
            color,
        );
        gizmos.line(
            Vec3::new(max.x, min.y, min.z),
            Vec3::new(max.x, max.y, min.z),
            color,
        );
        gizmos.line(
            Vec3::new(max.x, min.y, max.z),
            Vec3::new(max.x, max.y, max.z),
            color,
        );
        gizmos.line(
            Vec3::new(min.x, min.y, max.z),
            Vec3::new(min.x, max.y, max.z),
            color,
        );
    }
}

/// Per-face loop-trim retessellation is currently disabled because faces
/// don't own their own ECS meshes. Drains the request flag so the toggle
/// in the hierarchy panel doesn't fire forever; restoring the actual
/// retessellation will require rebuilding the affected shell's merged mesh.
pub(crate) fn retessellate_face(mut state: ResMut<ViewerState>) {
    let _ = state.retessellate_face.take();
}

/// Apply shading mode changes to materials and trigger mesh rebuilds.
pub(crate) fn apply_shading_mode(
    mut state: ResMut<ViewerState>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
    palette: Res<MaterialPalette>,
    matcap_res: Option<Res<MatcapTexture>>,
) {
    if !state.shading_mode_changed {
        return;
    }
    state.shading_mode_changed = false;

    let mode = state.shading_mode;
    state.previous_shading_mode = mode;

    // Resolve the per-mode material properties once, then push to the three
    // shared palette handles so every face picks them up via shared material
    // batching.
    let cull_back = Some(bevy::render::render_resource::Face::Back);
    let (alpha_mode, cull_mode, base_color, shading_flags, want_matcap) =
        match mode {
            ShadingMode::Shaded => {
                (AlphaMode::Opaque, cull_back, Color::WHITE, 0u32, false)
            }
            ShadingMode::Flat => (
                AlphaMode::Opaque,
                cull_back,
                Color::WHITE,
                SHADING_FLAG_FLAT,
                false,
            ),
            ShadingMode::XRay => (
                AlphaMode::Blend,
                None,
                Color::srgba(0.7, 0.7, 0.7, 0.3),
                0,
                false,
            ),
            ShadingMode::Wireframe => (
                AlphaMode::Blend,
                None,
                Color::srgba(0.0, 0.0, 0.0, 0.02),
                0,
                false,
            ),
            ShadingMode::Matcap => (
                AlphaMode::Opaque,
                cull_back,
                Color::WHITE,
                SHADING_FLAG_MATCAP,
                true,
            ),
        };

    let matcap_handle = if want_matcap {
        matcap_res.as_ref().map(|r| r.0.clone())
    } else {
        None
    };

    for handle in
        [&palette.default, &palette.selected, &palette.hovered].into_iter()
    {
        if let Some(mat) = materials.get_mut(handle) {
            mat.base.alpha_mode = alpha_mode;
            mat.base.cull_mode = cull_mode;
            mat.base.base_color = base_color;
            mat.extension.shading_flags = shading_flags;
            mat.extension.matcap_texture = matcap_handle.clone();
        }
    }
}

/// Rebuild mesh normals when switching between flat and smooth shading.
/// Flat-vs-smooth normal rebuild is currently disabled — flat shading uses
/// the smooth-normal mesh until per-shell flat-normal merging is added.
/// Drains the request flag to avoid re-firing.
pub(crate) fn rebuild_normals(mut state: ResMut<ViewerState>) {
    state.needs_normal_rebuild = false;
}

/// Update clip-plane uniforms on every `ViewerMaterial` asset when dirty.
pub(crate) fn update_clip_plane_uniforms(
    mut state: ResMut<ViewerState>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
) {
    if !state.clip_planes_dirty {
        return;
    }
    state.clip_planes_dirty = false;

    // Need bounding box to map normalised position to world coords.
    let bounds = match state.current_bounds {
        Some(b) => b,
        None => return,
    };

    // Axis unit vectors for X, Y, Z.
    const AXES: [Vec3; 3] = [Vec3::X, Vec3::Y, Vec3::Z];
    let bounds_min = [bounds.min.x, bounds.min.y, bounds.min.z];
    let bounds_max = [bounds.max.x, bounds.max.y, bounds.max.z];

    let mut planes = [Vec4::ZERO; 3];
    let mut active_bits: u32 = 0;

    for (i, (plane, cp)) in
        planes.iter_mut().zip(state.clip_planes.iter()).enumerate()
    {
        if !cp.enabled {
            continue;
        }

        // Map position (0..1) to bounding-box range on axis `i`.
        let pos =
            bounds_min[i] + cp.position_f32() * (bounds_max[i] - bounds_min[i]);

        // Normal: unit vector along axis, negated when flipped.
        let normal = if cp.flip { -AXES[i] } else { AXES[i] };

        // d = -dot(normal, point_on_plane). The point lies at `pos` on this
        // axis (other components zero), so d = -normal[i] * pos.
        let d = -normal[i] * pos;
        *plane = Vec4::new(normal.x, normal.y, normal.z, d);
        active_bits |= 1 << i;
    }

    let clip_active = UVec4::new(active_bits, 0, 0, 0);

    // Push to every material asset.
    for (_id, mat) in materials.iter_mut() {
        mat.extension.clip_plane_0 = planes[0];
        mat.extension.clip_plane_1 = planes[1];
        mat.extension.clip_plane_2 = planes[2];
        mat.extension.clip_active = clip_active;
    }
}

/// Global click observer: clicks on a shell mesh map back to the face
/// underneath the cursor by walking the merged-mesh triangles to find which
/// one contains the hit point, then reading face-local index from
/// `ShellRecord::vertex_face_index`. Sets the global face selection so the
/// hierarchy panel scrolls/highlights it.
///
/// CPU triangle scan is fine here because clicks are infrequent and a
/// shell's triangle count is bounded (~hundreds-thousands). When the
/// offscreen GPU id-pass lands this becomes a single image read.
pub(crate) fn on_mesh_click(
    click: On<Pointer<Click>>,
    shell_query: Query<&ShellMesh>,
    mut state: ResMut<ViewerState>,
    mut guard: ResMut<ViewportClickGuard>,
    mesh_assets: Res<Assets<Mesh>>,
) {
    if click.button != PointerButton::Primary {
        return;
    }
    guard.mesh_consumed = true;

    let Ok(shell_mesh) = shell_query.get(click.entity) else {
        log::debug!(
            "on_mesh_click: clicked entity {:?} is not a ShellMesh",
            click.entity
        );
        return;
    };
    let Some(world_pos) = click.hit.position else {
        log::warn!("on_mesh_click: hit.position missing on shell click");
        return;
    };

    let Some(shell_record) =
        state.shells.iter().find(|s| s.id == shell_mesh.shell_id)
    else {
        return;
    };
    let Some(mesh) = mesh_assets.get(&shell_record.mesh_handle) else {
        return;
    };
    let Some(positions) = mesh
        .attribute(Mesh::ATTRIBUTE_POSITION)
        .and_then(|a| a.as_float3())
    else {
        return;
    };
    let Some(mesh_indices) = mesh.indices() else {
        return;
    };
    let indices: Vec<u32> = mesh_indices.iter().map(|i| i as u32).collect();

    let face_id = find_face_id_at(
        world_pos,
        positions,
        &indices,
        &shell_record.vertex_face_index,
    );
    let Some(face_id) = face_id else {
        return;
    };
    state.selection = Some(Selection::Face(face_id as usize));
    state.selection_from_viewport = true;
}

/// Walk the triangles of an indexed mesh and return the global face_id of
/// the triangle that contains `world_pos`. Uses a cheap barycentric +
/// plane-distance test; first match wins (good enough since the picking
/// backend already gives us a point on the front-most surface).
fn find_face_id_at(
    world_pos: Vec3,
    positions: &[[f32; 3]],
    indices: &[u32],
    vertex_face_index: &[u32],
) -> Option<u32> {
    const BARY_TOLERANCE: f32 = 1e-3;
    const PLANE_TOLERANCE: f32 = 1e-2;

    indices.chunks_exact(3).find_map(|tri| {
        let i0 = tri[0] as usize;
        let i1 = tri[1] as usize;
        let i2 = tri[2] as usize;
        let v0 = Vec3::from(positions[i0]);
        let v1 = Vec3::from(positions[i1]);
        let v2 = Vec3::from(positions[i2]);

        let edge0 = v1 - v0;
        let edge1 = v2 - v0;
        let plane_normal_unnorm = edge0.cross(edge1);
        let plane_len = plane_normal_unnorm.length();
        if plane_len < 1e-10 {
            return None;
        }
        let plane_normal = plane_normal_unnorm / plane_len;
        let plane_dist = (world_pos - v0).dot(plane_normal).abs();
        if plane_dist > PLANE_TOLERANCE {
            return None;
        }

        let bary = barycentric(world_pos, v0, v1, v2);
        (bary[0] >= -BARY_TOLERANCE
            && bary[1] >= -BARY_TOLERANCE
            && bary[2] >= -BARY_TOLERANCE)
            .then(|| vertex_face_index[i0])
    })
}

fn barycentric(p: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> [f32; 3] {
    let v0v1 = v1 - v0;
    let v0v2 = v2 - v0;
    let v0p = p - v0;
    let d00 = v0v1.dot(v0v1);
    let d01 = v0v1.dot(v0v2);
    let d11 = v0v2.dot(v0v2);
    let d20 = v0p.dot(v0v1);
    let d21 = v0p.dot(v0v2);
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-12 {
        return [-1.0, -1.0, -1.0];
    }
    let inv = 1.0 / denom;
    let v = (d11 * d20 - d01 * d21) * inv;
    let w = (d00 * d21 - d01 * d20) * inv;
    let u = 1.0 - v - w;
    [u, v, w]
}

/// Click on empty viewport space (no face mesh hit) clears the selection.
pub(crate) fn clear_selection_on_empty_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut contexts: EguiContexts,
    drag_state: Res<ClipPlaneDragState>,
    mut guard: ResMut<ViewportClickGuard>,
    mut state: ResMut<ViewerState>,
) {
    const CLICK_DRAG_THRESHOLD_SQ: f32 = 16.0;

    let cursor_pos = windows.single().ok().and_then(|w| w.cursor_position());

    if mouse.just_pressed(MouseButton::Left) {
        guard.press_pos = cursor_pos;
        guard.mesh_consumed = false;
    }

    if mouse.just_released(MouseButton::Left) {
        let press = guard.press_pos.take();
        let consumed = std::mem::take(&mut guard.mesh_consumed);

        if consumed || drag_state.dragging {
            return;
        }

        let Ok(ctx) = contexts.ctx_mut() else {
            return;
        };
        if ctx.wants_pointer_input() || ctx.is_pointer_over_area() {
            return;
        }

        let (Some(p0), Some(p1)) = (press, cursor_pos) else {
            return;
        };
        if (p1 - p0).length_squared() > CLICK_DRAG_THRESHOLD_SQ {
            return;
        }

        if state.selection.is_some() {
            state.selection = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Clip-plane 3D handles: spawn / despawn / reposition translucent quads
// ---------------------------------------------------------------------------

/// Axis colours for clip-plane handles (translucent).
const CLIP_PLANE_COLORS: [Color; 3] = [
    Color::linear_rgba(1.0, 0.2, 0.2, 0.15), // X — red
    Color::linear_rgba(0.2, 1.0, 0.2, 0.15), // Y — green
    Color::linear_rgba(0.2, 0.2, 1.0, 0.15), // Z — blue
];

/// Margin factor – the handle quad extends a little beyond the bounding box so
/// that it remains visible even when the clip position is at the extremes.
const HANDLE_MARGIN: f32 = 1.05;

/// System that spawns, despawns and repositions clip-plane handle quads.
pub(crate) fn manage_clip_plane_visuals(
    mut commands: Commands,
    state: Res<ViewerState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut handle_q: Query<(Entity, &ClipPlaneHandle, &mut Transform)>,
) {
    let Some(bounds) = state.current_bounds else {
        // No scene loaded — despawn any lingering handles.
        for (entity, _, _) in handle_q.iter() {
            commands.entity(entity).despawn();
        }
        return;
    };

    let bounds_min = [bounds.min.x, bounds.min.y, bounds.min.z];
    let bounds_max = [bounds.max.x, bounds.max.y, bounds.max.z];
    let bounds_size = [
        bounds.max.x - bounds.min.x,
        bounds.max.y - bounds.min.y,
        bounds.max.z - bounds.min.z,
    ];

    for axis in 0..3 {
        let cp = &state.clip_planes[axis];

        // Find existing entity for this axis.
        let existing = handle_q.iter_mut().find(|(_, h, _)| h.axis == axis);

        if !cp.enabled {
            // Despawn if present.
            if let Some((entity, _, _)) = existing {
                commands.entity(entity).despawn();
            }
            continue;
        }

        // World position along clip axis.
        let pos = bounds_min[axis]
            + cp.position_f32() * (bounds_max[axis] - bounds_min[axis]);

        if let Some((_, _, mut transform)) = existing {
            // Update position only — the transform orientation & scale stay.
            match axis {
                0 => transform.translation.x = pos,
                1 => transform.translation.y = pos,
                _ => transform.translation.z = pos,
            }
        } else {
            // Spawn a new handle quad.
            // The quad size covers the two non-clip axes of the bbox.
            let (size_a, size_b) = match axis {
                0 => (bounds_size[1], bounds_size[2]), // YZ quad
                1 => (bounds_size[0], bounds_size[2]), // XZ quad
                _ => (bounds_size[0], bounds_size[1]), // XY quad
            };

            let mesh_handle = meshes
                .add(Plane3d::new(Vec3::Y, Vec2::splat(0.5)).mesh().build());

            let mat_handle = std_materials.add(StandardMaterial {
                base_color: CLIP_PLANE_COLORS[axis],
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                cull_mode: None,
                ..Default::default()
            });

            // Build transform: translate to `pos` on the clip axis, rotate so
            // the quad faces along the clip axis, scale to bbox extents.
            let translation = match axis {
                0 => Vec3::new(
                    pos,
                    (bounds_min[1] + bounds_max[1]) * 0.5,
                    (bounds_min[2] + bounds_max[2]) * 0.5,
                ),
                1 => Vec3::new(
                    (bounds_min[0] + bounds_max[0]) * 0.5,
                    pos,
                    (bounds_min[2] + bounds_max[2]) * 0.5,
                ),
                _ => Vec3::new(
                    (bounds_min[0] + bounds_max[0]) * 0.5,
                    (bounds_min[1] + bounds_max[1]) * 0.5,
                    pos,
                ),
            };

            // Plane3d default normal is Y-up, producing an XZ quad.
            // X-handle needs YZ quad: rotate 90° around Z.
            // Y-handle needs XZ quad: identity rotation (default).
            // Z-handle needs XY quad: rotate 90° around X.
            let rotation = match axis {
                0 => Quat::from_rotation_z(FRAC_PI_2),
                1 => Quat::IDENTITY,
                _ => Quat::from_rotation_x(FRAC_PI_2),
            };

            // Scale: the base mesh is 1×1 (half_size 0.5 on each side).
            // We need it to cover size_a × size_b.
            let scale = match axis {
                0 => Vec3::new(
                    size_b * HANDLE_MARGIN,
                    1.0,
                    size_a * HANDLE_MARGIN,
                ),
                1 => Vec3::new(
                    size_a * HANDLE_MARGIN,
                    1.0,
                    size_b * HANDLE_MARGIN,
                ),
                _ => Vec3::new(
                    size_a * HANDLE_MARGIN,
                    1.0,
                    size_b * HANDLE_MARGIN,
                ),
            };

            commands.spawn((
                ClipPlaneHandle { axis },
                Mesh3d(mesh_handle),
                MeshMaterial3d(mat_handle),
                Transform {
                    translation,
                    rotation,
                    scale,
                },
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Clip-plane drag interaction (observers)
// ---------------------------------------------------------------------------

/// Observer: drag-start on a clip-plane handle — disable camera orbit.
pub(crate) fn on_clip_plane_drag_start(
    event: On<Pointer<DragStart>>,
    handle_q: Query<&ClipPlaneHandle>,
    mut drag_state: ResMut<ClipPlaneDragState>,
) {
    if event.button != PointerButton::Primary {
        return;
    }
    if handle_q.get(event.entity).is_ok() {
        drag_state.dragging = true;
    }
}

/// Observer: drag on a clip-plane handle — reposition via ray-axis projection.
pub(crate) fn on_clip_plane_drag(
    event: On<Pointer<Drag>>,
    handle_q: Query<&ClipPlaneHandle>,
    camera_q: Query<(&Camera, &GlobalTransform), With<MainCamera>>,
    mut state: ResMut<ViewerState>,
) {
    if event.button != PointerButton::Primary {
        return;
    }
    let Ok(handle) = handle_q.get(event.entity) else {
        return;
    };
    let Some(bounds) = state.current_bounds else {
        return;
    };
    let Ok((camera, cam_gt)) = camera_q.single() else {
        return;
    };

    // Current pointer position in viewport pixels.
    let pointer_pos = event.pointer_location.position;

    // Cast ray from camera through pointer.
    let Ok(ray) = camera.viewport_to_world(cam_gt, pointer_pos) else {
        return;
    };

    let axis = handle.axis;
    let bounds_min = [bounds.min.x, bounds.min.y, bounds.min.z];
    let bounds_max = [bounds.max.x, bounds.max.y, bounds.max.z];
    let extent = bounds_max[axis] - bounds_min[axis];
    if extent.abs() < 1e-8 {
        return;
    }

    // Find the point on the ray closest to the clip axis line.
    // The clip axis line passes through the bbox center along unit axis `axis`.
    let axis_dir = match axis {
        0 => Vec3::X,
        1 => Vec3::Y,
        _ => Vec3::Z,
    };
    let axis_origin = (bounds.min + bounds.max) * 0.5;

    // Closest approach between two lines:
    //   Line A: P = ray.origin + t * ray.direction
    //   Line B: Q = axis_origin + s * axis_dir
    // We want s that gives the closest point on axis_dir to Line A.
    let d = *ray.direction; // Vec3
    let w = ray.origin - axis_origin;
    let a = d.dot(d);
    let b = d.dot(axis_dir);
    let c = axis_dir.dot(axis_dir);
    let d_val = d.dot(w);
    let e = axis_dir.dot(w);
    let denom = a * c - b * b;
    if denom.abs() < 1e-10 {
        // Lines are parallel — can't determine position.
        return;
    }
    let s = (a * e - b * d_val) / denom;
    let closest_on_axis = axis_origin + s * axis_dir;
    let world_pos = match axis {
        0 => closest_on_axis.x,
        1 => closest_on_axis.y,
        _ => closest_on_axis.z,
    };

    // Map world position back to 0..1 normalised range.
    let t = ((world_pos - bounds_min[axis]) / extent).clamp(0.0, 1.0);
    let new_position = (t * 1000.0).round() as u16;

    if state.clip_planes[axis].position != new_position {
        state.clip_planes[axis].position = new_position;
        state.clip_planes_dirty = true;
    }
}

/// Observer: drag-end on a clip-plane handle — re-enable camera orbit.
pub(crate) fn on_clip_plane_drag_end(
    event: On<Pointer<DragEnd>>,
    handle_q: Query<&ClipPlaneHandle>,
    mut drag_state: ResMut<ClipPlaneDragState>,
) {
    if event.button != PointerButton::Primary {
        return;
    }
    if handle_q.get(event.entity).is_ok() {
        drag_state.dragging = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rgb_near(lhs: [f32; 3], rhs: [f32; 3]) {
        lhs.into_iter()
            .zip(rhs)
            .for_each(|(lhs, rhs)| assert!((lhs - rhs).abs() < 1.0e-6));
    }

    #[test]
    fn face_display_color_is_gray_without_random_or_step_colors() {
        let (rgb, apply_colors) =
            face_display_color(7, Some([1.0, 0.8, 0.1]), false, false);

        assert_rgb_near(
            rgb,
            [NEUTRAL_GRAY[0], NEUTRAL_GRAY[1], NEUTRAL_GRAY[2]],
        );
        assert!(!apply_colors);
    }

    #[test]
    fn face_display_color_uses_step_color_when_step_colors_are_on() {
        let step_color = [1.0, 0.0, 0.0];
        let (rgb, apply_colors) =
            face_display_color(7, Some(step_color), false, true);

        assert_rgb_near(rgb, step_color);
        assert!(apply_colors);
    }

    #[test]
    fn face_display_color_randomizes_faces_without_step_color() {
        let (_, expected) = color_for_index(7);
        let (rgb, apply_colors) = face_display_color(7, None, true, false);

        assert_rgb_near(rgb, expected);
        assert!(apply_colors);
    }

    #[test]
    fn face_display_color_mixes_step_color_with_random_face_color() {
        let step_color = [1.0, 0.8, 0.1];
        let (face_7, apply_7) =
            face_display_color(7, Some(step_color), true, true);
        let (face_8, apply_8) =
            face_display_color(8, Some(step_color), true, true);

        assert!(apply_7);
        assert!(apply_8);
        assert_ne!(face_7, step_color);
        assert_ne!(face_7, face_8);
    }

    #[test]
    fn bounds_for_face_positions_filters_vertices_by_face_id() {
        let positions = [
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 3.0, 0.0],
            [-10.0, -10.0, -10.0],
        ];
        let vertex_face_index = [5, 5, 5, 9];

        let bounds =
            bounds_for_face_positions(&positions, &vertex_face_index, 5)
                .expect("face vertices should produce bounds");

        assert_eq!(bounds.min, Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(bounds.max, Vec3::new(2.0, 3.0, 0.0));
        assert_eq!(bounds.center, Vec3::new(1.0, 1.5, 0.0));
    }

    #[test]
    fn frame_distance_uses_largest_bounds_dimension() {
        let bounds = Bounds {
            center: Vec3::ZERO,
            min: Vec3::new(-1.0, -2.0, -3.0),
            max: Vec3::new(1.0, 2.0, 3.0),
        };

        assert!((frame_distance_for_bounds(bounds) - 9.0).abs() < 1.0e-6);
    }

    #[test]
    fn initial_camera_offset_matches_startup_view_direction() {
        let offset = initial_camera_offset(2.0);
        let expected = Vec3::new(
            2.0 * FRAC_PI_4.cos() * FRAC_PI_6.cos(),
            2.0 * FRAC_PI_6.sin(),
            2.0 * FRAC_PI_4.sin() * FRAC_PI_6.cos(),
        );

        assert!((offset - expected).length() < 1.0e-6);
    }
}
