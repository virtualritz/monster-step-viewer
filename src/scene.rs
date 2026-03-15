use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::RenderLayers,
    pbr::wireframe::Wireframe,
    picking::pointer::PointerButton,
    prelude::*,
    render::render_resource::PrimitiveTopology,
};
use bevy_egui::{EguiContexts, EguiGlobalSettings, PrimaryEguiContext};
use bevy_panorbit_camera::PanOrbitCamera;
use monster_step_viewer::{
    CompressedShellData, StepBoundaryLoop, StepEdge, StepFace, StepScene,
    StepShell, StepTopology,
};
use monstertruck::meshing::prelude::PolygonMesh;
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;

use crate::state::{
    AMBIENT_BRIGHTNESS, BACK_LIGHT_ILLUMINANCE, Bounds, ClipPlaneDragState,
    ClipPlaneHandle, EdgeRecord, FaceMesh, FaceRecord, KEY_LIGHT_ILLUMINANCE,
    LoadJob, LoopRecord, MATERIAL_METALLIC, MATERIAL_ROUGHNESS, MainCamera,
    NEUTRAL_GRAY, Selection, ShadingMode, ShellRecord, SolidifyJob, ViewerState,
};
use crate::viewer_material::{ViewerMaterial, ViewerMaterialExt};

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
            PanOrbitCamera {
                focus: Vec3::ZERO,
                radius: Some(2.0),
                ..Default::default()
            },
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
                    std::f32::consts::PI * 0.25,
                    std::f32::consts::PI * -0.3,
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
                    std::f32::consts::PI * -0.7,
                    std::f32::consts::PI * 0.15,
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

pub(crate) fn process_load_requests(
    mut commands: Commands,
    mut state: ResMut<ViewerState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
    existing_meshes: Query<Entity, With<FaceMesh>>,
    clip_handles: Query<Entity, With<ClipPlaneHandle>>,
) {
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
            current_shell: 0,
            total_shells: 0,
        });
        info!("Started loading STEP file");
    }

    // Poll the loading job for new messages.
    let Some(job) = state.loading_job.as_mut() else {
        return;
    };

    // Collect all available messages first (to avoid borrow issues).
    let (messages, disconnected): (Vec<_>, bool) = {
        let receiver = job.receiver.lock();
        let mut messages = Vec::new();
        let mut disconnected = false;

        loop {
            match receiver.try_recv() {
                Ok(msg) => messages.push(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        (messages, disconnected)
    };

    // Process collected messages.
    for msg in messages {
        // Re-borrow job mutably for each message.
        let Some(job) = state.loading_job.as_mut() else {
            return;
        };

        match msg {
            monster_step_viewer::LoadMessage::Metadata(meta) => {
                state.metadata = Some(meta);
            }
            monster_step_viewer::LoadMessage::TotalShells(total) => {
                job.total_shells = total;
            }
            monster_step_viewer::LoadMessage::Progress(current, _total) => {
                job.current_shell = current;
            }
            monster_step_viewer::LoadMessage::Shell(shell) => {
                // Store shell in scene_data - don't spawn meshes yet (need
                // bounds first).
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
                let path = job.path.clone();
                state.loaded_path = Some(path);
                state.loading_job = None;

                // Compute bounds for ENTIRE scene.
                let bounds = state.scene_data.as_ref().and_then(compute_bounds);

                if let Some(bounds) = bounds {
                    let size = bounds.max - bounds.min;
                    let max_dim = size.x.max(size.y).max(size.z);
                    let scale = if max_dim > 0.0 { 1.0 / max_dim } else { 1.0 };

                    // Store normalization params for wireframe rendering.
                    state.scene_center = bounds.center;
                    state.scene_scale = scale;

                    info!(
                        "Scene bounds: center=({:.2}, {:.2}, {:.2}), max_dim={:.2}, scale={:.4}",
                        bounds.center.x,
                        bounds.center.y,
                        bounds.center.z,
                        max_dim,
                        scale
                    );

                    // Now spawn all meshes with normalization applied.
                    // Take scene_data temporarily to avoid borrow conflict.
                    if let Some(scene) = state.scene_data.take() {
                        for shell in &scene.shells {
                            spawn_shell_faces_normalized(
                                shell,
                                &mut commands,
                                &mut meshes,
                                &mut materials,
                                &mut state,
                                bounds.center,
                                scale,
                            );
                        }
                        state.scene_data = Some(scene);
                    }
                    state.current_bounds = Some(Bounds {
                        center: Vec3::ZERO,
                        min: (bounds.min - bounds.center) * scale,
                        max: (bounds.max - bounds.center) * scale,
                    });
                }

                // Track the tessellation factor used for this load.
                state.applied_tessellation_factor = state.tessellation_factor;

                // Check whether any shell has solid topology (for Solidify
                // Clip).
                state.has_solid_topology = state
                    .scene_data
                    .as_ref()
                    .is_some_and(|scene| {
                        scene.shells.iter().any(|s| {
                            matches!(
                                s.topology,
                                Some(monster_step_viewer::StepTopology::Solid(_))
                            )
                        })
                    });

                // Apply persisted clip plane / shading state to the newly
                // spawned materials.
                state.clip_planes_dirty = true;
                if state.shading_mode != ShadingMode::default() {
                    state.shading_mode_changed = true;
                }

                info!(
                    "Finished loading {} shells, {} faces (has_solid_topology={})",
                    state.shells.len(),
                    state.faces.len(),
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

/// Spawn faces for a single shell with normalization applied.
pub(crate) fn spawn_shell_faces_normalized(
    shell: &StepShell,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<ViewerMaterial>>,
    state: &mut ResMut<ViewerState>,
    scene_center: Vec3,
    scale: f32,
) {
    let use_random_colors = state.show_random_colors;
    let base_face_id = state.faces.len();
    let face_ids: Vec<usize> = (0..shell.faces.len())
        .map(|idx| base_face_id + idx)
        .collect();

    // Shell color from STEP file (if defined).
    let step_color = shell.color;

    for (idx, face) in shell.faces.iter().enumerate() {
        let global_face_id = base_face_id + idx;

        // For random colors: each face gets its own color based on
        // global_face_id. For STEP colors: all faces in shell use the
        // STEP-defined color. Otherwise: neutral gray (handled in mesh
        // function).
        let ui_rgb = if let Some(color) = step_color {
            color
        } else {
            let (_, rgb) = color_for_index(global_face_id);
            rgb
        };

        let (mesh, tri_count) = bevy_mesh_from_polygon_normalized(
            &face.mesh,
            ui_rgb,
            use_random_colors || step_color.is_some(),
            scene_center,
            scale,
        );
        let mesh_handle = meshes.add(mesh);

        let material_handle = materials.add(ViewerMaterial {
            base: StandardMaterial {
                base_color: Color::WHITE,
                perceptual_roughness: MATERIAL_ROUGHNESS,
                metallic: MATERIAL_METALLIC,
                ..Default::default()
            },
            extension: ViewerMaterialExt::default(),
        });

        commands.spawn((
            FaceMesh {
                face_id: global_face_id,
            },
            Mesh3d(mesh_handle.clone()),
            MeshMaterial3d(material_handle.clone()),
            Transform::default(),
            Visibility::Visible,
        ));

        state.faces.push(FaceRecord {
            id: global_face_id,
            shell_id: shell.id,
            name: face.name.clone(),
            triangles: tri_count,
            visible: true,
            ui_color: ui_rgb,
            mesh_handle,
            material_handle,
            edge_ids: Vec::new(),
            loop_ids: Vec::new(),
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
    let mut referenced_edge_ids = std::collections::HashSet::new();
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

    state.shells.push(ShellRecord {
        id: shell.id,
        name: shell.name.clone(),
        expanded: true,
        visible: true,
        face_ids,
        standalone_edge_ids,
    });
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

pub(crate) fn bevy_mesh_from_polygon_normalized(
    mesh: &PolygonMesh,
    shell_color: [f32; 3],
    use_random_colors: bool,
    scene_center: Vec3,
    scale: f32,
) -> (Mesh, usize) {
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

    // Collect vertices as (pos_idx, nor_idx) tuples.
    let mut vertices: Vec<(usize, Option<usize>)> = Vec::new();

    for tri in mesh.tri_faces() {
        vertices.extend([
            (tri[0].pos, tri[0].nor),
            (tri[1].pos, tri[1].nor),
            (tri[2].pos, tri[2].nor),
        ]);
    }

    for quad in mesh.quad_faces() {
        vertices.extend([
            (quad[0].pos, quad[0].nor),
            (quad[1].pos, quad[1].nor),
            (quad[2].pos, quad[2].nor),
            (quad[0].pos, quad[0].nor),
            (quad[2].pos, quad[2].nor),
            (quad[3].pos, quad[3].nor),
        ]);
    }

    for face in mesh.other_faces() {
        if face.len() < 3 {
            continue;
        }
        let first = (face[0].pos, face[0].nor);
        face.windows(2).skip(1).for_each(|w| {
            vertices.extend([
                first,
                (w[0].pos, w[0].nor),
                (w[1].pos, w[1].nor),
            ]);
        });
    }

    // Expand indexed geometry to flat arrays.
    let expanded: Vec<_> = vertices
        .par_iter()
        .map(|(pos_idx, nor_idx)| {
            let pos = positions[*pos_idx];
            // Fallback normal.
            let nor = nor_idx.map(|ni| normals[ni]).unwrap_or([0.0, 0.0, 1.0]);
            (pos, nor)
        })
        .collect();
    let (flat_positions, flat_normals): (Vec<[f32; 3]>, Vec<[f32; 3]>) =
        expanded.into_iter().unzip();

    // Uniform color per shell: distinct color if random colors enabled, gray
    // otherwise.
    let color = if use_random_colors {
        [shell_color[0], shell_color[1], shell_color[2], 1.0]
    } else {
        NEUTRAL_GRAY
    };
    let colors: Vec<[f32; 4]> = vec![color; flat_positions.len()];

    let mut bevy_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, flat_positions);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, flat_normals);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);

    (bevy_mesh, vertices.len() / 3)
}

pub(crate) fn apply_face_visibility(
    mut state: ResMut<ViewerState>,
    mut query: Query<(&FaceMesh, &mut Visibility)>,
) {
    if !state.visibility_changed {
        return;
    }
    state.visibility_changed = false;

    for (mesh, mut visibility) in query.iter_mut() {
        if let Some(record) = state.faces.iter().find(|f| f.id == mesh.face_id)
        {
            // Face is visible only when both its own toggle and its shell's
            // toggle are on.
            let shell_visible = state
                .shells
                .iter()
                .find(|s| s.id == record.shell_id)
                .is_none_or(|s| s.visible);
            *visibility = if record.visible && shell_visible {
                Visibility::Visible
            } else {
                Visibility::Hidden
            };
        }
    }
}

pub(crate) fn apply_selection_highlight(
    mut state: ResMut<ViewerState>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
) {
    let sel_changed = state.selection != state.prev_selection;
    let hover_changed = state.hover != state.prev_hover;
    if !sel_changed && !hover_changed {
        return;
    }

    let selection_emissive = Color::linear_rgba(0.6, 0.45, 0.0, 1.0);
    let hover_emissive = Color::linear_rgba(0.2, 0.15, 0.0, 1.0);

    // Resolve selection → face IDs.
    let resolve = |sel: &Option<Selection>,
                   faces: &[FaceRecord],
                   loops: &[LoopRecord]|
     -> Vec<usize> {
        match sel {
            Some(Selection::Face(fid)) => vec![*fid],
            Some(Selection::Loop(lid)) => loops
                .iter()
                .find(|l| l.id == *lid)
                .map(|l| vec![l.face_id])
                .unwrap_or_default(),
            Some(Selection::Edge(eid)) => faces
                .iter()
                .find(|f| f.edge_ids.contains(eid))
                .map(|f| vec![f.id])
                .unwrap_or_default(),
            _ => vec![],
        }
    };

    let sel_faces = resolve(&state.selection, &state.faces, &state.loops);
    let prev_sel_faces =
        resolve(&state.prev_selection, &state.faces, &state.loops);
    let hover_faces = resolve(&state.hover, &state.faces, &state.loops);
    let prev_hover_faces =
        resolve(&state.prev_hover, &state.faces, &state.loops);

    // Collect all face IDs that need updating.
    let mut dirty: Vec<usize> = Vec::new();
    for &id in sel_faces
        .iter()
        .chain(&prev_sel_faces)
        .chain(&hover_faces)
        .chain(&prev_hover_faces)
    {
        if !dirty.contains(&id) {
            dirty.push(id);
        }
    }

    for face in state.faces.iter() {
        if !dirty.contains(&face.id) {
            continue;
        }
        let Some(mat) = materials.get_mut(&face.material_handle) else {
            continue;
        };
        // Selection takes priority over hover.
        if sel_faces.contains(&face.id) {
            mat.base.emissive = selection_emissive.into();
        } else if hover_faces.contains(&face.id) {
            mat.base.emissive = hover_emissive.into();
        } else {
            mat.base.emissive = Color::BLACK.into();
        }
    }

    state.prev_selection = state.selection;
    state.prev_hover = state.hover;
}

pub(crate) fn normalize_scene_and_setup_camera(
    mut state: ResMut<ViewerState>,
    mut camera_query: Query<
        (&mut Transform, &mut PanOrbitCamera),
        With<MainCamera>,
    >,
    mesh_query: Query<&Transform, (With<FaceMesh>, Without<MainCamera>)>,
) {
    let Some(bounds) = state.pending_bounds else {
        return;
    };

    // Wait until meshes are actually available in the query (ECS delay).
    let mesh_count = mesh_query.iter().count();
    let expected_faces = state.faces.len();
    if mesh_count < expected_faces {
        // Meshes not ready yet, try again next frame.
        return;
    }

    // Now we can consume pending_bounds.
    state.pending_bounds = None;

    // Calculate scene dimensions.
    let size = bounds.max - bounds.min;
    let max_dim = size.x.max(size.y).max(size.z);

    // Store bounds for bounding box gizmo.
    state.current_bounds = Some(bounds);

    log::info!(
        "DEBUG: About to setup camera. Bounds center=({:.2}, {:.2}, {:.2}), max_dim={:.2}",
        bounds.center.x,
        bounds.center.y,
        bounds.center.z,
        max_dim
    );

    // Set up camera to view the scene from appropriate distance.
    // Use ~1.5x the max dimension for good framing.
    let camera_distance = max_dim * 1.5;
    if let Ok((mut transform, mut pan_orbit)) = camera_query.single_mut() {
        pan_orbit.focus = bounds.center;
        pan_orbit.radius = Some(camera_distance);
        // 45 degrees.
        pan_orbit.yaw = Some(std::f32::consts::FRAC_PI_4);
        // 30 degrees.
        pan_orbit.pitch = Some(std::f32::consts::FRAC_PI_6);
        pan_orbit.force_update = true;
        // Force re-initialization.
        pan_orbit.initialized = false;

        // Set initial transform position.
        let yaw = std::f32::consts::FRAC_PI_4;
        let pitch = std::f32::consts::FRAC_PI_6;
        let offset = Vec3::new(
            camera_distance * yaw.cos() * pitch.cos(),
            camera_distance * pitch.sin(),
            camera_distance * yaw.sin() * pitch.cos(),
        );
        transform.translation = bounds.center + offset;
        *transform = transform.looking_at(bounds.center, Vec3::Y);

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

pub(crate) fn rebuild_meshes_on_toggle(
    mut state: ResMut<ViewerState>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !state.needs_mesh_rebuild {
        return;
    }
    state.needs_mesh_rebuild = false;

    let Some(scene) = &state.scene_data else {
        return;
    };

    let use_random_colors = state.show_random_colors;

    // Update vertex colors in-place on existing meshes (no despawn/respawn).
    // Iterate through all faces in all shells.
    for shell in &scene.shells {
        // STEP-defined colors always show; random colors only when toggle is
        // on.
        let apply_colors = use_random_colors || shell.color.is_some();

        for step_face in &shell.faces {
            // Find the corresponding FaceRecord.
            if let Some(face_record) = state
                .faces
                .iter()
                .find(|f| f.shell_id == shell.id && f.name == step_face.name)
                && let Some(mesh) = meshes.get_mut(&face_record.mesh_handle)
            {
                let colors = recompute_colors_for_mesh(
                    &step_face.mesh,
                    face_record.ui_color,
                    apply_colors,
                );
                mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
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

/// Recompute vertex colors for a mesh without rebuilding geometry.
/// Returns colors in the same vertex order as bevy_mesh_from_polygon.
pub(crate) fn recompute_colors_for_mesh(
    mesh: &PolygonMesh,
    shell_color: [f32; 3],
    use_random_colors: bool,
) -> Vec<[f32; 4]> {
    // Count total vertices.
    let mut vertex_count = 0usize;
    vertex_count += mesh.tri_faces().len() * 3;
    // 2 triangles per quad.
    vertex_count += mesh.quad_faces().len() * 6;
    for face in mesh.other_faces() {
        if face.len() >= 3 {
            vertex_count += (face.len() - 2) * 3;
        }
    }

    // Use shell's distinct color if random colors enabled, otherwise neutral
    // gray.
    let color = if use_random_colors {
        [shell_color[0], shell_color[1], shell_color[2], 1.0]
    } else {
        // Neutral gray.
        NEUTRAL_GRAY
    };

    vec![color; vertex_count]
}

/// Disable PanOrbitCamera when egui wants pointer input (e.g., during panel
/// resize) or when a clip-plane handle is being dragged.
pub(crate) fn disable_camera_when_egui_wants_input(
    mut contexts: EguiContexts,
    mut camera_query: Query<&mut PanOrbitCamera, With<MainCamera>>,
    drag_state: Res<ClipPlaneDragState>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    let egui_wants_input =
        ctx.wants_pointer_input() || ctx.is_pointer_over_area();

    if let Ok(mut pan_orbit) = camera_query.single_mut() {
        pan_orbit.enabled = !egui_wants_input && !drag_state.dragging;
    }
}

/// Draw bounding box and wireframe gizmos when enabled.
pub(crate) fn draw_gizmos(state: Res<ViewerState>, mut gizmos: Gizmos) {
    // Draw wireframe edges (STEP geometry boundary edges stored in scene_data).
    if state.show_wireframe
        && let Some(scene) = &state.scene_data
    {
        let color = Color::srgba(0.0, 0.0, 0.0, 0.4);
        let center = state.scene_center;
        let scale = state.scene_scale;

        for shell in &scene.shells {
            for (p0_arr, p1_arr) in &shell.edges {
                // Apply same normalization as mesh vertices: (pos - center) *
                // scale.
                let p0_raw = Vec3::new(
                    p0_arr[0] as f32,
                    p0_arr[1] as f32,
                    p0_arr[2] as f32,
                );
                let p1_raw = Vec3::new(
                    p1_arr[0] as f32,
                    p1_arr[1] as f32,
                    p1_arr[2] as f32,
                );
                let p0 = (p0_raw - center) * scale;
                let p1 = (p1_raw - center) * scale;
                gizmos.line(p0, p1, color);
            }
        }
    }

    // Draw STEP curve edges as blue polylines (highlighted if selected).
    if state.show_edges
        && let Some(scene) = &state.scene_data
    {
        let edge_color = Color::srgba(0.2, 0.6, 1.0, 0.9);
        let highlight_color = Color::srgba(1.0, 0.85, 0.0, 1.0);
        let center = state.scene_center;
        let scale = state.scene_scale;
        let mut edge_offset = 0usize;

        // Precompute which edge IDs are highlighted by the current selection.
        let highlighted_edges: std::collections::HashSet<usize> =
            match &state.selection {
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
                _ => std::collections::HashSet::new(),
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

/// Re-tessellate a face when loop trimming changes.
pub(crate) fn retessellate_face(
    mut state: ResMut<ViewerState>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(face_id) = state.retessellate_face.take() else {
        return;
    };

    // Find the face record and its shell.
    let Some(face_rec) = state.faces.iter().find(|f| f.id == face_id) else {
        log::warn!("retessellate_face: face {} not found", face_id);
        return;
    };
    let shell_id = face_rec.shell_id;
    let mesh_handle = face_rec.mesh_handle.clone();
    let ui_color = face_rec.ui_color;

    // Find the shell in scene_data.
    let Some(scene) = &state.scene_data else {
        return;
    };
    let Some(shell) = scene.shells.iter().find(|s| s.id == shell_id) else {
        return;
    };
    let Some(shell_data) = &shell.original_shell else {
        log::warn!(
            "retessellate_face: no original shell data for shell {}",
            shell_id
        );
        return;
    };

    // Compute base_face_id for this shell.
    let base_face_id = state
        .shells
        .iter()
        .take_while(|s| s.id != shell_id)
        .flat_map(|s| s.face_ids.iter())
        .count();
    let local_face_idx = face_id - base_face_id;

    let tolerance = shell.tessellation_tolerance;
    let transform = shell.transform.as_ref();

    // Determine actual boundary indices from the original compressed face.
    // Loop records are in order of the original boundaries. We need the indices
    // of loops that have trimming_active = true.
    let face_loops: Vec<&crate::state::LoopRecord> = state
        .loops
        .iter()
        .filter(|l| l.face_id == face_id)
        .collect();
    let active_indices: Vec<usize> = face_loops
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trimming_active)
        .map(|(i, _)| i)
        .collect();

    let new_mesh = monster_step_viewer::retessellate_face(
        shell_data,
        local_face_idx,
        &active_indices,
        tolerance,
        transform,
    );

    let Some(polygon_mesh) = new_mesh else {
        log::warn!("Re-tessellation returned no mesh for face {}", face_id);
        return;
    };

    // Build the Bevy mesh from the new polygon mesh.
    let use_random_colors = state.show_random_colors;
    let (bevy_mesh, tri_count) = bevy_mesh_from_polygon_normalized(
        &polygon_mesh,
        ui_color,
        use_random_colors,
        state.scene_center,
        state.scene_scale,
    );

    // Update the existing mesh asset.
    if let Some(existing) = meshes.get_mut(&mesh_handle) {
        *existing = bevy_mesh;
    }

    // Update triangle count in face record.
    if let Some(face_rec) = state.faces.iter_mut().find(|f| f.id == face_id) {
        face_rec.triangles = tri_count;
    }

    // Also update the StepFace mesh in scene_data for wireframe consistency.
    if let Some(scene) = &mut state.scene_data
        && let Some(shell) = scene.shells.iter_mut().find(|s| s.id == shell_id)
        && let Some(step_face) =
            shell.faces.iter_mut().find(|f| f.id == local_face_idx)
    {
        step_face.mesh = polygon_mesh;
    }
}

/// Apply shading mode changes to materials and trigger mesh rebuilds.
pub(crate) fn apply_shading_mode(
    mut commands: Commands,
    mut state: ResMut<ViewerState>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
    face_query: Query<(Entity, &FaceMesh)>,
    matcap_res: Option<Res<crate::viewer_material::MatcapTexture>>,
) {
    if !state.shading_mode_changed {
        return;
    }
    state.shading_mode_changed = false;

    let mode = state.shading_mode;
    let prev_mode = state.previous_shading_mode;
    state.previous_shading_mode = mode;

    // Determine whether we need a normal rebuild (flat <-> smooth transition).
    let entering_flat = mode == ShadingMode::Flat && prev_mode != ShadingMode::Flat;
    let leaving_flat = mode != ShadingMode::Flat && prev_mode == ShadingMode::Flat;
    if entering_flat || leaving_flat {
        state.needs_normal_rebuild = true;
    }

    match mode {
        ShadingMode::Shaded => {
            // Opaque, back-face culled, white base color, no special flags.
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Opaque;
                    mat.base.cull_mode = Some(bevy::render::render_resource::Face::Back);
                    mat.base.base_color = Color::WHITE;
                    mat.extension.shading_flags = 0;
                    mat.extension.matcap_texture = None;
                }
            }
            // Remove Wireframe component from all face entities.
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
        }
        ShadingMode::Flat => {
            // Same material properties as Shaded; normals rebuilt separately.
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Opaque;
                    mat.base.cull_mode = Some(bevy::render::render_resource::Face::Back);
                    mat.base.base_color = Color::WHITE;
                    mat.extension.shading_flags = 0;
                    mat.extension.matcap_texture = None;
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
        }
        ShadingMode::XRay => {
            // Translucent, double-sided.
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Blend;
                    mat.base.cull_mode = None;
                    mat.base.base_color = Color::srgba(0.7, 0.7, 0.7, 0.3);
                    mat.extension.shading_flags = 0;
                    mat.extension.matcap_texture = None;
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
        }
        ShadingMode::Wireframe => {
            // Add Wireframe component to all face entities.
            // Make mesh nearly invisible so only wireframe lines show.
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Blend;
                    mat.base.base_color = Color::srgba(0.0, 0.0, 0.0, 0.02);
                    mat.base.cull_mode = if state.show_edges {
                        None
                    } else {
                        Some(bevy::render::render_resource::Face::Back)
                    };
                    mat.extension.shading_flags = 0;
                    mat.extension.matcap_texture = None;
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).insert(Wireframe);
            }
        }
        ShadingMode::Matcap => {
            let matcap_handle = matcap_res.map(|r| r.0.clone());
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Opaque;
                    mat.base.cull_mode = Some(bevy::render::render_resource::Face::Back);
                    mat.base.base_color = Color::WHITE;
                    mat.extension.shading_flags = 1;
                    mat.extension.matcap_texture = matcap_handle.clone();
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
        }
    }
}

/// Rebuild mesh normals when switching between flat and smooth shading.
pub(crate) fn rebuild_normals(
    mut state: ResMut<ViewerState>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !state.needs_normal_rebuild {
        return;
    }
    state.needs_normal_rebuild = false;

    let use_flat = state.shading_mode == ShadingMode::Flat;

    let Some(scene) = &state.scene_data else {
        return;
    };

    let center = state.scene_center;
    let scale = state.scene_scale;
    let use_random_colors = state.show_random_colors;

    for shell in &scene.shells {
        let apply_colors = use_random_colors || shell.color.is_some();

        for step_face in &shell.faces {
            let Some(face_record) = state
                .faces
                .iter()
                .find(|f| f.shell_id == shell.id && f.name == step_face.name)
            else {
                continue;
            };
            let Some(mesh) = meshes.get_mut(&face_record.mesh_handle) else {
                continue;
            };

            // Rebuild the entire mesh geometry (positions + normals + colors)
            // to get correct flat or smooth normals.
            let (new_mesh, _tri_count) = if use_flat {
                bevy_mesh_from_polygon_flat_normals(
                    &step_face.mesh,
                    face_record.ui_color,
                    apply_colors,
                    center,
                    scale,
                )
            } else {
                bevy_mesh_from_polygon_normalized(
                    &step_face.mesh,
                    face_record.ui_color,
                    apply_colors,
                    center,
                    scale,
                )
            };

            *mesh = new_mesh;
        }
    }
}

/// Build a Bevy mesh from a `PolygonMesh` using flat (per-face) normals.
/// Each triangle gets a single normal computed from the cross product of its
/// edges, giving a faceted appearance.
fn bevy_mesh_from_polygon_flat_normals(
    mesh: &PolygonMesh,
    shell_color: [f32; 3],
    use_random_colors: bool,
    scene_center: Vec3,
    scale: f32,
) -> (Mesh, usize) {
    let positions: Vec<[f32; 3]> = mesh
        .positions()
        .par_iter()
        .map(|p| {
            let pos = Vec3::new(p.x as f32, p.y as f32, p.z as f32);
            let normalized = (pos - scene_center) * scale;
            [normalized.x, normalized.y, normalized.z]
        })
        .collect();

    // Collect all triangles as (pos_idx0, pos_idx1, pos_idx2).
    let mut triangles: Vec<[usize; 3]> = Vec::new();

    for tri in mesh.tri_faces() {
        triangles.push([tri[0].pos, tri[1].pos, tri[2].pos]);
    }

    for quad in mesh.quad_faces() {
        triangles.push([quad[0].pos, quad[1].pos, quad[2].pos]);
        triangles.push([quad[0].pos, quad[2].pos, quad[3].pos]);
    }

    for face in mesh.other_faces() {
        if face.len() < 3 {
            continue;
        }
        let first = face[0].pos;
        for w in face.windows(2).skip(1) {
            triangles.push([first, w[0].pos, w[1].pos]);
        }
    }

    // Build flat arrays with per-face normals.
    let mut flat_positions = Vec::with_capacity(triangles.len() * 3);
    let mut flat_normals = Vec::with_capacity(triangles.len() * 3);

    for tri_indices in &triangles {
        let v0 = Vec3::from(positions[tri_indices[0]]);
        let v1 = Vec3::from(positions[tri_indices[1]]);
        let v2 = Vec3::from(positions[tri_indices[2]]);

        let edge1 = v1 - v0;
        let edge2 = v2 - v0;
        let normal = edge1.cross(edge2).normalize_or_zero();
        let n = [normal.x, normal.y, normal.z];

        flat_positions.push(positions[tri_indices[0]]);
        flat_positions.push(positions[tri_indices[1]]);
        flat_positions.push(positions[tri_indices[2]]);

        flat_normals.push(n);
        flat_normals.push(n);
        flat_normals.push(n);
    }

    let color = if use_random_colors {
        [shell_color[0], shell_color[1], shell_color[2], 1.0]
    } else {
        NEUTRAL_GRAY
    };
    let colors: Vec<[f32; 4]> = vec![color; flat_positions.len()];

    let mut bevy_mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, flat_positions);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, flat_normals);
    bevy_mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);

    (bevy_mesh, triangles.len())
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

    for (i, (plane, cp)) in planes
        .iter_mut()
        .zip(state.clip_planes.iter())
        .enumerate()
    {
        if !cp.enabled {
            continue;
        }

        // Map position (0..1) to bounding-box range on axis `i`.
        let pos = bounds_min[i]
            + cp.position_f32() * (bounds_max[i] - bounds_min[i]);

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

/// Global observer: clicking a face mesh in the viewport selects it in the
/// hierarchy.
pub(crate) fn on_mesh_click(
    click: On<Pointer<Click>>,
    face_query: Query<&FaceMesh>,
    mut state: ResMut<ViewerState>,
) {
    // Only respond to primary (left) button.
    if click.button != PointerButton::Primary {
        return;
    }
    if let Ok(face_mesh) = face_query.get(click.entity) {
        state.selection = Some(Selection::Face(face_mesh.face_id));
        state.selection_from_viewport = true;
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
        let existing = handle_q
            .iter_mut()
            .find(|(_, h, _)| h.axis == axis);

        if !cp.enabled {
            // Despawn if present.
            if let Some((entity, _, _)) = existing {
                commands.entity(entity).despawn();
            }
            continue;
        }

        // World position along clip axis.
        let pos =
            bounds_min[axis] + cp.position_f32() * (bounds_max[axis] - bounds_min[axis]);

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

            let mesh_handle = meshes.add(
                Plane3d::new(Vec3::Y, Vec2::splat(0.5)).mesh().build(),
            );

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
                0 => Vec3::new(pos, (bounds_min[1] + bounds_max[1]) * 0.5, (bounds_min[2] + bounds_max[2]) * 0.5),
                1 => Vec3::new((bounds_min[0] + bounds_max[0]) * 0.5, pos, (bounds_min[2] + bounds_max[2]) * 0.5),
                _ => Vec3::new((bounds_min[0] + bounds_max[0]) * 0.5, (bounds_min[1] + bounds_max[1]) * 0.5, pos),
            };

            // Plane3d default normal is Y-up, producing an XZ quad.
            // X-handle needs YZ quad: rotate 90° around Z.
            // Y-handle needs XZ quad: identity rotation (default).
            // Z-handle needs XY quad: rotate 90° around X.
            let rotation = match axis {
                0 => Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                1 => Quat::IDENTITY,
                _ => Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            };

            // Scale: the base mesh is 1×1 (half_size 0.5 on each side).
            // We need it to cover size_a × size_b.
            let scale = match axis {
                0 => Vec3::new(size_b * HANDLE_MARGIN, 1.0, size_a * HANDLE_MARGIN),
                1 => Vec3::new(size_a * HANDLE_MARGIN, 1.0, size_b * HANDLE_MARGIN),
                _ => Vec3::new(size_a * HANDLE_MARGIN, 1.0, size_b * HANDLE_MARGIN),
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

// ---------------------------------------------------------------------------
// Solidify Clip: boolean-AND with half-space boxes for each active clip plane
// ---------------------------------------------------------------------------

/// Kick off a background thread that performs the boolean AND operation.
pub(crate) fn start_solidify_clip(mut state: ResMut<ViewerState>) {
    if !state.start_solidify {
        return;
    }
    state.start_solidify = false;

    // Gather the first solid topology from scene_data.
    let scene = match state.scene_data.as_ref() {
        Some(s) => s,
        None => return,
    };

    // Find a shell with solid topology.
    let (solid_data, tol) = {
        let mut found = None;
        for shell in &scene.shells {
            if let Some(StepTopology::Solid(ref data)) = shell.topology {
                found = Some((data.clone(), shell.tessellation_tolerance));
                break;
            }
        }
        match found {
            Some(v) => v,
            None => {
                state.error = Some("No solid topology found".to_string());
                return;
            }
        }
    };

    // Compute bounds from scene mesh data (world-space, pre-normalization).
    let bounds = match compute_bounds(scene) {
        Some(b) => b,
        None => {
            state.error = Some("Cannot compute bounds for solidify".to_string());
            return;
        }
    };

    // Collect active clip planes with their world-space positions.
    let mut active_clips: Vec<(usize, f64, bool)> = Vec::new();
    for (i, cp) in state.clip_planes.iter().enumerate() {
        if !cp.enabled {
            continue;
        }
        let t = cp.position as f64 / 1000.0;
        let axis_min = match i {
            0 => bounds.min.x as f64,
            1 => bounds.min.y as f64,
            _ => bounds.min.z as f64,
        };
        let axis_max = match i {
            0 => bounds.max.x as f64,
            1 => bounds.max.y as f64,
            _ => bounds.max.z as f64,
        };
        let world_pos = axis_min + t * (axis_max - axis_min);
        active_clips.push((i, world_pos, cp.flip));
    }

    if active_clips.is_empty() {
        return;
    }

    // Spawn background thread.
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = solidify_clip_inner(&solid_data, &active_clips, tol);
        let _ = tx.send(result);
    });

    state.solidify_job = Some(SolidifyJob {
        receiver: parking_lot::Mutex::new(rx),
    });
    info!("Started solidify-clip background job");
}

/// Perform boolean AND of solid with half-space boxes (runs on background thread).
fn solidify_clip_inner(
    solid_data: &CompressedShellData,
    active_clips: &[(usize, f64, bool)],
    tol: f64,
) -> Result<StepScene, String> {
    use monstertruck::meshing::prelude::{BoundingBox, Point3, RobustMeshableShape};
    use monstertruck::modeling::Curve as ModelingCurve;
    use monstertruck::modeling::Surface as ModelingSurface;
    use monstertruck::modeling::Invertible;
    use monstertruck::solid::and as solid_and;
    use monstertruck::step::load::step_geometry::{Curve3D, Surface, Pcurve};
    use monstertruck::topology::compress::CompressedTrimmedSolid;
    use monstertruck::topology::Solid;

    type StepSolid = Solid<Point3, Curve3D, Surface>;
    type StepCompressedSolid = CompressedTrimmedSolid<Point3, Curve3D, Surface, Pcurve>;
    type ModelingSolid = Solid<Point3, ModelingCurve, ModelingSurface>;

    // Downcast and extract the solid.
    let csolid: &StepCompressedSolid = solid_data
        .downcast_ref::<StepCompressedSolid>()
        .ok_or_else(|| "Failed to downcast CompressedShellData to CompressedSolid".to_string())?;

    let current_solid: StepSolid = StepSolid::extract(csolid.clone().erase_trims())
        .map_err(|e| format!("Failed to extract solid: {}", e))?;

    let mut current_solid: ModelingSolid = current_solid.try_mapped(
        |p| Some(*p),
        |c: &Curve3D| ModelingCurve::try_from(c).ok(),
        |s: &Surface| ModelingSurface::try_from(s).ok(),
    ).ok_or_else(|| "Failed to convert STEP solid to modeling types".to_string())?;

    // Apply each active clip plane as a boolean AND with a half-space box.
    // The shader discards fragments where dot(normal, pos) + d > 0, which for
    // no-flip (normal = +axis, d = -pos) keeps the NEGATIVE side (x <= pos).
    // The solidify box must match: no-flip keeps (-big .. world_pos).
    let big = 1e6;
    for &(axis, world_pos, flip) in active_clips {
        let (min_pt, max_pt) = match axis {
            0 => {
                // X axis
                if flip {
                    (
                        Point3::new(world_pos, -big, -big),
                        Point3::new(big, big, big),
                    )
                } else {
                    (
                        Point3::new(-big, -big, -big),
                        Point3::new(world_pos, big, big),
                    )
                }
            }
            1 => {
                // Y axis
                if flip {
                    (
                        Point3::new(-big, world_pos, -big),
                        Point3::new(big, big, big),
                    )
                } else {
                    (
                        Point3::new(-big, -big, -big),
                        Point3::new(big, world_pos, big),
                    )
                }
            }
            _ => {
                // Z axis
                if flip {
                    (
                        Point3::new(-big, -big, world_pos),
                        Point3::new(big, big, big),
                    )
                } else {
                    (
                        Point3::new(-big, -big, -big),
                        Point3::new(big, big, world_pos),
                    )
                }
            }
        };

        let bbox = BoundingBox::from_iter([min_pt, max_pt]);
        let halfspace: ModelingSolid = monstertruck::modeling::primitive::cuboid(bbox);

        current_solid = solid_and(&current_solid, &halfspace, tol)
            .map_err(|e| format!("Boolean AND failed on axis {}: {:?}", axis, e))?;
    }

    // Compress the result and re-tessellate.
    let compressed = current_solid.compress();
    let clipped_solid_data = CompressedShellData::new(compressed.clone());

    // Tessellate each boundary shell of the solid.
    let mut all_shells: Vec<StepShell> = Vec::new();
    for (boundary_idx, boundary) in compressed.boundaries.iter().enumerate() {
        let poly_shell = boundary.robust_triangulation(tol);

        // Extract faces from the tessellated shell.
        let all_edges: Vec<([f64; 3], [f64; 3])> = Vec::new();
        let faces: Vec<StepFace> = poly_shell
            .faces
            .iter()
            .enumerate()
            .filter_map(|(face_idx, face)| {
                face.surface.as_ref().map(|surface| {
                    let mesh = match face.orientation {
                        true => surface.clone(),
                        false => surface.inverse(),
                    };

                    // Extract boundary loop topology.
                    let boundary_loops: Vec<StepBoundaryLoop> = face
                        .boundaries
                        .iter()
                        .enumerate()
                        .map(|(loop_idx, loop_edges)| StepBoundaryLoop {
                            edge_indices: loop_edges
                                .iter()
                                .map(|ei| ei.index)
                                .collect(),
                            is_outer: loop_idx == 0,
                        })
                        .collect();

                    StepFace {
                        id: face_idx,
                        name: format!("Face {}", face_idx + 1),
                        mesh,
                        boundary_loops,
                    }
                })
            })
            .collect();

        // Extract curve edges.
        let curve_edges: Vec<StepEdge> = poly_shell
            .edges
            .iter()
            .enumerate()
            .map(|(i, edge)| {
                let points = edge.curve.iter().map(|p| [p.x, p.y, p.z]).collect();
                StepEdge {
                    id: i,
                    curve_type: "Unknown".to_string(),
                    points,
                }
            })
            .collect();

        all_shells.push(StepShell {
            id: boundary_idx,
            name: format!("Shell {} (clipped)", boundary_idx + 1),
            faces,
            color: None,
            transform: None,
            edges: all_edges,
            curve_edges,
            original_shell: None,
            topology: Some(StepTopology::Solid(clipped_solid_data.clone())),
            tessellation_tolerance: tol,
        });
    }

    if all_shells.is_empty() {
        return Err("Boolean clipping produced no shells".to_string());
    }

    Ok(StepScene {
        metadata: monster_step_viewer::StepMetadata::default(),
        shells: all_shells,
    })
}

/// Poll for solidify-clip completion and apply the result.
pub(crate) fn poll_solidify_clip(
    mut state: ResMut<ViewerState>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
    existing_meshes: Query<Entity, With<FaceMesh>>,
    clip_handles: Query<Entity, With<ClipPlaneHandle>>,
) {
    let job = match state.solidify_job.as_ref() {
        Some(j) => j,
        None => return,
    };

    let result = {
        let receiver = job.receiver.lock();
        receiver.try_recv()
    };

    let result = match result {
        Ok(r) => r,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => {
            state.solidify_job = None;
            state.error = Some("Solidify job thread terminated unexpectedly".to_string());
            return;
        }
    };

    // Clear the job.
    state.solidify_job = None;

    match result {
        Ok(new_scene) => {
            info!(
                "Solidify-clip completed: {} shells, {} total faces",
                new_scene.shells.len(),
                new_scene.shells.iter().map(|s| s.faces.len()).sum::<usize>(),
            );

            // Despawn all existing face mesh entities.
            for entity in existing_meshes.iter() {
                commands.entity(entity).despawn();
            }
            // Remove clip-plane handles — they'll be re-created if needed.
            for entity in clip_handles.iter() {
                commands.entity(entity).despawn();
            }

            // Clear records.
            state.shells.clear();
            state.faces.clear();
            state.edges.clear();
            state.loops.clear();
            state.selection = None;
            state.prev_selection = None;

            // Compute bounds for the new scene and spawn meshes.
            let bounds = compute_bounds(&new_scene);
            if let Some(bounds) = bounds {
                let size = bounds.max - bounds.min;
                let max_dim = size.x.max(size.y).max(size.z);
                let scale = if max_dim > 0.0 { 1.0 / max_dim } else { 1.0 };

                state.scene_center = bounds.center;
                state.scene_scale = scale;

                for shell in &new_scene.shells {
                    spawn_shell_faces_normalized(
                        shell,
                        &mut commands,
                        &mut meshes,
                        &mut materials,
                        &mut state,
                        bounds.center,
                        scale,
                    );
                }

                state.current_bounds = Some(Bounds {
                    center: Vec3::ZERO,
                    min: (bounds.min - bounds.center) * scale,
                    max: (bounds.max - bounds.center) * scale,
                });
            }

            // Update scene_data.
            state.scene_data = Some(new_scene);

            // Update solid topology flag.
            state.has_solid_topology = state
                .scene_data
                .as_ref()
                .is_some_and(|scene| {
                    scene.shells.iter().any(|s| {
                        matches!(
                            s.topology,
                            Some(StepTopology::Solid(_))
                        )
                    })
                });

            // Trigger material updates.
            state.clip_planes_dirty = true;
            state.shading_mode_changed = true;
            state.visibility_changed = true;
        }
        Err(err) => {
            error!("Solidify-clip failed: {}", err);
            state.error = Some(format!("Solidify failed: {}", err));
        }
    }
}
