use bevy::{
    asset::RenderAssetUsages,
    camera::{RenderTarget, visibility::RenderLayers},
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages},
};
use bevy_egui::{EguiTextureHandle, EguiUserTextures};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use crate::scene::{bevy_mesh_from_polygon_normalized, color_for_index, compute_bounds};
use crate::state::{
    BrowserState, DirectoryEntry, MATERIAL_METALLIC, MATERIAL_ROUGHNESS, MAX_RENDER_SLOTS,
    PREVIEW_SIZE, PREVIEW_TESSELLATION_FACTOR, PreviewCamera, PreviewData, PreviewEntry,
    PreviewLight, PreviewMesh, PreviewStatus, RenderSlot,
};

/// Expand the tree from `root` down to `target`, loading children lazily along the way.
/// Returns true if the target was found and expanded to.
pub(crate) fn expand_tree_to_path(
    tree: &mut Vec<DirectoryEntry>,
    root: &Path,
    target: &Path,
) -> bool {
    let Ok(relative) = target.strip_prefix(root) else {
        return false;
    };
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return true;
    }
    let mut current_level = tree.as_mut_slice();
    for component in &components {
        let name = component.as_os_str().to_string_lossy();
        let Some(entry) = current_level.iter_mut().find(|e| e.name == name.as_ref()) else {
            return false;
        };
        entry.expanded = true;
        if entry.children.is_none() {
            entry.children = Some(scan_subdirs(&entry.path));
        }
        current_level = entry.children.as_mut().unwrap().as_mut_slice();
    }
    true
}

/// Scan a directory for subdirectories, returning sorted entries.
pub(crate) fn scan_subdirs(path: &Path) -> Vec<DirectoryEntry> {
    let Ok(read_dir) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut entries: Vec<DirectoryEntry> = read_dir
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
        .filter(|e| !e.file_name().to_str().is_some_and(|n| n.starts_with('.')))
        .map(|e| DirectoryEntry {
            path: e.path(),
            name: e.file_name().to_string_lossy().to_string(),
            expanded: false,
            children: None,
        })
        .collect();
    entries.sort_by_key(|a| a.name.to_lowercase());
    entries
}

/// Scan a directory for STEP files, returning sorted preview entries.
pub(crate) fn scan_step_files(dir: &Path) -> Vec<PreviewEntry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<PreviewEntry> = read_dir
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|ft| ft.is_file()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            name.ends_with(".step") || name.ends_with(".stp")
        })
        .map(|e| PreviewEntry {
            path: e.path(),
            filename: e.file_name().to_string_lossy().to_string(),
            status: PreviewStatus::Pending,
        })
        .collect();
    entries.sort_by_key(|a| a.filename.to_lowercase());
    entries
}

/// Start background loading of preview tessellations.
pub(crate) fn start_preview_loads(state: &mut BrowserState) {
    // Cancel any existing loads.
    state.cancel_flag.store(true, Ordering::Relaxed);
    let cancel_flag = Arc::new(AtomicBool::new(false));
    state.cancel_flag = cancel_flag.clone();

    let pending: Vec<(usize, std::path::PathBuf)> = state
        .previews
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.status, PreviewStatus::Pending))
        .map(|(i, p)| (i, p.path.clone()))
        .collect();

    if pending.is_empty() {
        return;
    }

    // Mark all as loading.
    for &(i, _) in &pending {
        state.previews[i].status = PreviewStatus::Loading;
    }

    let (tx, rx) = mpsc::channel();
    state.preview_receiver = Some(parking_lot::Mutex::new(rx));

    // Spawn background thread that processes files sequentially (to avoid overloading).
    std::thread::spawn(move || {
        for (idx, path) in pending {
            if cancel_flag.load(Ordering::Relaxed) {
                break;
            }
            let result = load_preview(&path, &cancel_flag);
            if tx.send((idx, result)).is_err() {
                break;
            }
        }
    });
}

/// Load a single STEP file at preview quality.
fn load_preview(path: &Path, cancel: &AtomicBool) -> Result<PreviewData, String> {
    let receiver = monster_step_viewer::load_step_file_streaming(
        path.to_path_buf(),
        PREVIEW_TESSELLATION_FACTOR,
    );

    let mut shells = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        match receiver.recv() {
            Ok(monster_step_viewer::LoadMessage::Shell(shell)) => {
                shells.push(shell);
            }
            Ok(monster_step_viewer::LoadMessage::Done) => break,
            Ok(monster_step_viewer::LoadMessage::Error(e)) => return Err(e),
            Ok(_) => {} // Metadata, Progress, TotalShells — skip.
            Err(_) => return Err("loader disconnected".to_string()),
        }
    }

    // Compute bounds for normalization.
    let scene = monster_step_viewer::StepScene {
        metadata: Default::default(),
        shells,
    };
    let bounds = compute_bounds(&scene).ok_or("empty geometry")?;
    let size = bounds.max - bounds.min;
    let max_dim = size.x.max(size.y).max(size.z);
    let scale = if max_dim > 0.0 { 1.0 / max_dim } else { 1.0 };

    Ok(PreviewData {
        shells: scene.shells,
        bounds_center: bounds.center,
        bounds_scale: scale,
    })
}

/// Poll the preview receiver and update statuses.
pub(crate) fn poll_preview_loads(state: &mut BrowserState) {
    let Some(receiver) = &state.preview_receiver else {
        return;
    };
    let receiver = receiver.lock();
    while let Ok((idx, result)) = receiver.try_recv() {
        if idx < state.previews.len() {
            state.previews[idx].status = match result {
                Ok(data) => PreviewStatus::Ready(data),
                Err(e) => PreviewStatus::Failed(e),
            };
        }
    }
}

/// Create render target images and slots.
pub(crate) fn setup_render_slots(
    images: &mut Assets<Image>,
    egui_textures: &mut EguiUserTextures,
) -> Vec<RenderSlot> {
    (0..MAX_RENDER_SLOTS)
        .map(|_| {
            let size = Extent3d {
                width: PREVIEW_SIZE,
                height: PREVIEW_SIZE,
                depth_or_array_layers: 1,
            };
            let mut image = Image::new_fill(
                size,
                TextureDimension::D2,
                &[30, 30, 30, 255],
                TextureFormat::Bgra8UnormSrgb,
                RenderAssetUsages::default(),
            );
            image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT;
            let handle = images.add(image);
            let egui_texture_id =
                egui_textures.add_image(EguiTextureHandle::Strong(handle.clone()));
            RenderSlot {
                image: handle,
                egui_texture_id: Some(egui_texture_id),
                preview_index: None,
                yaw: 0.0,
            }
        })
        .collect()
}

/// Spawn preview scene entities (meshes + camera + light) for a given slot.
fn spawn_preview_scene(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    slot_idx: usize,
    image_handle: Handle<Image>,
    yaw: f32,
    preview_data: &PreviewData,
) {
    let layer = RenderLayers::layer(slot_idx + 1);
    let center = preview_data.bounds_center;
    let scale = preview_data.bounds_scale;

    // Spawn meshes for all faces in all shells.
    for (shell_idx, shell) in preview_data.shells.iter().enumerate() {
        let step_color = shell.color;
        for (face_idx, face) in shell.faces.iter().enumerate() {
            let global_idx = shell_idx * 100 + face_idx;
            let ui_rgb = step_color.unwrap_or_else(|| {
                let (_, rgb) = color_for_index(global_idx);
                rgb
            });
            let (mesh, _) =
                bevy_mesh_from_polygon_normalized(&face.mesh, ui_rgb, true, center, scale);
            let mesh_handle = meshes.add(mesh);
            let material = materials.add(StandardMaterial {
                base_color: Color::WHITE,
                perceptual_roughness: MATERIAL_ROUGHNESS,
                metallic: MATERIAL_METALLIC,
                ..Default::default()
            });
            commands.spawn((
                PreviewMesh { slot: slot_idx },
                Mesh3d(mesh_handle),
                MeshMaterial3d(material),
                Transform::default(),
                Visibility::Visible,
                layer.clone(),
            ));
        }
    }

    // Spawn camera rendering to the slot's image.
    let camera_distance = 1.5;
    let pitch = std::f32::consts::FRAC_PI_6;
    let offset = Vec3::new(
        camera_distance * yaw.cos() * pitch.cos(),
        camera_distance * pitch.sin(),
        camera_distance * yaw.sin() * pitch.cos(),
    );
    commands.spawn((
        PreviewCamera { slot: slot_idx },
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(Color::srgba(0.12, 0.12, 0.12, 1.0)),
            order: (slot_idx as isize) + 10,
            ..Default::default()
        },
        RenderTarget::from(image_handle),
        Transform::from_translation(offset).looking_at(Vec3::ZERO, Vec3::Y),
        layer.clone(),
    ));

    // Spawn directional light for this slot.
    commands.spawn((
        PreviewLight { slot: slot_idx },
        DirectionalLight {
            illuminance: 8000.0,
            shadows_enabled: false,
            ..Default::default()
        },
        Transform::from_rotation(Quat::from_euler(
            EulerRot::YXZ,
            std::f32::consts::PI * 0.25,
            std::f32::consts::PI * -0.3,
            0.0,
        )),
        layer,
    ));
}

/// Despawn all entities belonging to a render slot.
pub(crate) fn despawn_slot(
    commands: &mut Commands,
    slot_idx: usize,
    world_query: &[(Entity, usize)],
) {
    for &(entity, s) in world_query {
        if s == slot_idx {
            commands.entity(entity).despawn();
        }
    }
}

/// Update turntable rotation for all active preview cameras.
pub(crate) fn update_turntable_system(
    mut browser: ResMut<BrowserState>,
    mut cameras: Query<(&PreviewCamera, &mut Transform)>,
    time: Res<Time>,
) {
    let rotation_speed = 0.5; // radians per second.
    let dt = time.delta_secs();

    for slot in &mut browser.render_slots {
        if slot.preview_index.is_some() {
            slot.yaw += rotation_speed * dt;
        }
    }

    for (preview_cam, mut transform) in cameras.iter_mut() {
        if let Some(slot) = browser.render_slots.get(preview_cam.slot) {
            let camera_distance = 1.5;
            let pitch = std::f32::consts::FRAC_PI_6;
            let yaw = slot.yaw;
            let offset = Vec3::new(
                camera_distance * yaw.cos() * pitch.cos(),
                camera_distance * pitch.sin(),
                camera_distance * yaw.sin() * pitch.cos(),
            );
            *transform = Transform::from_translation(offset).looking_at(Vec3::ZERO, Vec3::Y);
        }
    }
}

/// Manage which previews get render slots based on visibility in the scroll area.
pub(crate) fn manage_render_slots_system(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut browser: ResMut<BrowserState>,
    preview_meshes: Query<(Entity, &PreviewMesh)>,
    preview_cameras: Query<(Entity, &PreviewCamera)>,
    preview_lights: Query<(Entity, &PreviewLight)>,
) {
    let cols = browser.grid_cols.max(1);
    let thumb_size = browser.thumb_size;
    if thumb_size <= 0.0 {
        return;
    }

    let scroll_offset = browser.scroll_offset;
    let visible_rows = browser.visible_rows.max(1);

    // Calculate which preview indices are visible.
    let first_visible_row = (scroll_offset / (thumb_size + 8.0)).floor() as usize;
    let first_visible_idx = first_visible_row * cols;
    let last_visible_idx =
        ((first_visible_row + visible_rows + 1) * cols).min(browser.previews.len());

    let visible_range = first_visible_idx..last_visible_idx;

    // Collect which indices need slots and which slots can be freed.
    let needed: Vec<usize> = visible_range
        .clone()
        .filter(|&i| {
            matches!(
                browser.previews.get(i).map(|p| &p.status),
                Some(PreviewStatus::Ready(_))
            )
        })
        .collect();

    // Build list of all slot-associated entities for despawning.
    let mesh_entities: Vec<(Entity, usize)> =
        preview_meshes.iter().map(|(e, m)| (e, m.slot)).collect();
    let cam_entities: Vec<(Entity, usize)> =
        preview_cameras.iter().map(|(e, c)| (e, c.slot)).collect();
    let light_entities: Vec<(Entity, usize)> =
        preview_lights.iter().map(|(e, l)| (e, l.slot)).collect();

    let mut all_entities: Vec<(Entity, usize)> = Vec::new();
    all_entities.extend(&mesh_entities);
    all_entities.extend(&cam_entities);
    all_entities.extend(&light_entities);

    // Free slots that are no longer in the visible range.
    for slot in &mut browser.render_slots {
        if let Some(idx) = slot.preview_index
            && !needed.contains(&idx)
        {
            slot.preview_index = None;
        }
    }

    // Despawn entities for freed slots.
    for (slot_idx, slot) in browser.render_slots.iter().enumerate() {
        if slot.preview_index.is_none() {
            despawn_slot(&mut commands, slot_idx, &all_entities);
        }
    }

    // Collect assignments to make: (slot_idx, preview_idx).
    let mut assignments: Vec<(usize, usize)> = Vec::new();
    for preview_idx in &needed {
        // Already assigned?
        if browser
            .render_slots
            .iter()
            .any(|s| s.preview_index == Some(*preview_idx))
        {
            continue;
        }
        // Find a free slot not already claimed in this batch.
        let claimed: Vec<usize> = assignments.iter().map(|(s, _)| *s).collect();
        let free_slot = browser
            .render_slots
            .iter()
            .enumerate()
            .position(|(i, s)| s.preview_index.is_none() && !claimed.contains(&i));
        let Some(slot_idx) = free_slot else {
            break;
        };
        if matches!(
            browser.previews.get(*preview_idx).map(|p| &p.status),
            Some(PreviewStatus::Ready(_))
        ) {
            assignments.push((slot_idx, *preview_idx));
        }
    }

    // Execute assignments.
    for (slot_idx, preview_idx) in assignments {
        let image_handle = browser.render_slots[slot_idx].image.clone();
        browser.render_slots[slot_idx].preview_index = Some(preview_idx);
        browser.render_slots[slot_idx].yaw = 0.0;

        if let PreviewStatus::Ready(data) = &browser.previews[preview_idx].status {
            spawn_preview_scene(
                &mut commands,
                &mut meshes,
                &mut materials,
                slot_idx,
                image_handle,
                0.0,
                data,
            );
        }
    }
}
