# Clipping Planes + Shading Modes Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add shader-based clip planes with 3D drag handles, 5 shading modes, and optional CAD boolean solidification for STEP solids.

**Architecture:** All face meshes switch from `StandardMaterial` to `ExtendedMaterial<StandardMaterial, ViewerMaterialExt>`. The extension's fragment shader handles clip plane discard and matcap shading. Shading mode changes modify StandardMaterial properties (alpha, cull mode) and mesh geometry (flat normals). Solidify clip runs monstertruck `and()` on a background thread.

**Tech Stack:** Bevy 0.18 `ExtendedMaterial`/`MaterialExtension`, WGSL shaders, `WireframePlugin`, monstertruck `solid::and()`, `topology::CompressedSolid`, `modeling::builder::cuboid`.

---

## Phase 1: Material Extension Foundation

### Task 1: Add ViewerMaterialExt type and WGSL shader

This task replaces `StandardMaterial` with `ExtendedMaterial<StandardMaterial, ViewerMaterialExt>` on all face meshes. The extension initially just passes through (no clip logic yet) so we can verify the plumbing works.

**Files:**
- Create: `src/viewer_material.rs`
- Create: `assets/shaders/viewer_material.wgsl`
- Modify: `src/main.rs`
- Modify: `src/scene.rs:299-314` (material creation in `spawn_shell_faces_normalized`)
- Modify: `src/scene.rs:566-639` (selection highlight — now uses extended material)
- Modify: `src/state.rs` (FaceRecord material handle type)
- Modify: `Cargo.toml` (no new deps needed, but verify bevy `shader_format_glsl` or similar isn't needed)

**Step 1: Create the WGSL shader**

Create `assets/shaders/viewer_material.wgsl`:

```wgsl
#import bevy_pbr::{
    forward_io::VertexOutput,
    pbr_fragment::pbr_input_from_vertex_output,
}

struct ViewerMaterialUniforms {
    clip_planes: array<vec4<f32>, 3>,
    clip_active: vec4<u32>,
    shading_flags: u32, // bit 0 = matcap mode
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
};

@group(2) @binding(100)
var<uniform> viewer: ViewerMaterialUniforms;

@group(2) @binding(101)
var matcap_texture: texture_2d<f32>;
@group(2) @binding(102)
var matcap_sampler: sampler;

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> @location(0) vec4<f32> {
    // Clip plane discard.
    for (var i = 0u; i < 3u; i++) {
        if viewer.clip_active[i] != 0u {
            let plane = viewer.clip_planes[i];
            if dot(in.world_position.xyz, plane.xyz) + plane.w < 0.0 {
                discard;
            }
        }
    }

    // Matcap shading (when shading_flags bit 0 is set).
    if (viewer.shading_flags & 1u) != 0u {
        let normal = normalize(in.world_normal);
        // View-space normal for matcap UV lookup.
        let view_normal = (view.world_from_view * vec4<f32>(normal, 0.0)).xyz;
        let uv = view_normal.xy * 0.5 + 0.5;
        return textureSample(matcap_texture, matcap_sampler, uv);
    }

    // Default: let PBR pipeline handle it (return nothing — this needs the
    // apply_pbr_lighting approach). Actually, for ExtendedMaterial the base
    // StandardMaterial fragment runs first, then this extension runs.
    // We only override when we want to (matcap) or discard (clip).
    // When not overriding, we need to return the PBR result.

    // For now, signal "use base material" by returning the PBR-computed color.
    // ExtendedMaterial calls the extension fragment AFTER the base, so we
    // actually need to structure this differently — see implementation notes.
    return vec4<f32>(1.0); // placeholder
}
```

> **Implementation note:** The exact WGSL structure depends on how Bevy 0.18's `MaterialExtension` fragment pipeline works. The implementing agent should check `bevy_pbr/src/extended_material.rs` to understand whether the extension fragment replaces or post-processes the base fragment. If it replaces, the shader needs to call `pbr_input_from_vertex_output` and `apply_pbr_lighting` for non-matcap modes. If it post-processes, clip discard works directly and matcap needs to override the output. **Read the Bevy source before writing the final shader.**

**Step 2: Create `src/viewer_material.rs`**

```rust
use bevy::{
    asset::Asset,
    pbr::MaterialExtension,
    prelude::*,
    render::render_resource::{AsBindGroup, ShaderRef},
};

#[derive(Asset, Clone, AsBindGroup, TypePath)]
pub(crate) struct ViewerMaterialExt {
    #[uniform(100)]
    pub clip_planes: [Vec4; 3],
    #[uniform(100)] // same binding — packed as a struct
    pub clip_active: UVec4,
    #[uniform(100)]
    pub shading_flags: u32,
    #[texture(101)]
    #[sampler(102)]
    pub matcap_texture: Option<Handle<Image>>,
}

impl Default for ViewerMaterialExt {
    fn default() -> Self {
        Self {
            clip_planes: [Vec4::ZERO; 3],
            clip_active: UVec4::ZERO,
            shading_flags: 0,
            matcap_texture: None,
        }
    }
}

impl MaterialExtension for ViewerMaterialExt {
    fn fragment_shader() -> ShaderRef {
        "shaders/viewer_material.wgsl".into()
    }
}

/// Type alias for the combined material.
pub(crate) type ViewerMaterial = ExtendedMaterial<StandardMaterial, ViewerMaterialExt>;
```

> **Implementation note:** The `#[uniform(100)]` bindings may need to be a single struct or separate bindings depending on how `AsBindGroup` handles multiple fields at the same binding. Check Bevy docs. May need `#[uniform(100)]` for a single `ViewerUniforms` struct wrapping all fields.

**Step 3: Update `src/main.rs`**

Add the material plugin:

```rust
mod viewer_material;

// In App::new() chain, after .add_plugins(EguiPlugin::default()):
.add_plugins(MaterialPlugin::<viewer_material::ViewerMaterial>::default())
```

**Step 4: Update material creation in `src/scene.rs:299-314`**

Change `spawn_shell_faces_normalized` to use `ViewerMaterial`:

```rust
// Replace:
//   let material_handle = materials.add(StandardMaterial { ... });
//   MeshMaterial3d(material_handle.clone()),
// With:
let material_handle = ext_materials.add(ViewerMaterial {
    base: StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: MATERIAL_ROUGHNESS,
        metallic: MATERIAL_METALLIC,
        ..Default::default()
    },
    extension: ViewerMaterialExt::default(),
});
commands.spawn((
    FaceMesh { face_id: global_face_id },
    Mesh3d(mesh_handle.clone()),
    MeshMaterial3d(material_handle.clone()),
    Transform::default(),
    Visibility::Visible,
));
```

The function signature needs a new parameter: `ext_materials: &mut ResMut<Assets<ViewerMaterial>>`.

**Step 5: Update `FaceRecord` in `src/state.rs:150`**

Change the material handle type:

```rust
// Replace:
//   pub material_handle: Handle<StandardMaterial>,
// With:
pub material_handle: Handle<crate::viewer_material::ViewerMaterial>,
```

**Step 6: Update `apply_selection_highlight` in `src/scene.rs:566-639`**

Change from `Assets<StandardMaterial>` to `Assets<ViewerMaterial>`:

```rust
// Replace:
//   mut materials: ResMut<Assets<StandardMaterial>>,
// With:
mut materials: ResMut<Assets<crate::viewer_material::ViewerMaterial>>,

// And access the base material:
//   mat.emissive = ...
// Becomes:
//   mat.base.emissive = ...
```

**Step 7: Update `rebuild_meshes_on_toggle` in `src/scene.rs:716`**

No material changes needed here — it only modifies mesh vertex colors, not materials. But verify it still works.

**Step 8: Update browser `spawn_preview_scene` in `src/browser.rs:304`**

Preview scenes can keep using plain `StandardMaterial` — they don't need clip planes. No change needed here.

**Step 9: Verify**

```bash
cargo check && cargo clippy -- -D warnings
cargo run --release -- <test.step>
```

Expect: model renders identically to before (extension is a no-op passthrough).

**Step 10: Commit**

```bash
git add src/viewer_material.rs assets/shaders/viewer_material.wgsl src/main.rs src/scene.rs src/state.rs
git commit -m "feat: replace StandardMaterial with ExtendedMaterial for viewer meshes"
```

---

## Phase 2: Shader Clip Planes

### Task 2: Add clip plane state and UI toggle buttons

**Files:**
- Modify: `src/state.rs` — add `ClipPlaneState`, fields on `ViewerState`
- Modify: `src/icons.rs` — add clip plane icons
- Modify: `src/ui.rs:963-977` — add X/Y/Z clip toggle buttons after existing toolbar buttons
- Modify: `src/persistence.rs` — persist clip plane enabled state

**Step 1: Add types to `src/state.rs`**

After the `Selection` enum (line 38):

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ClipPlaneState {
    /// Whether this clip plane is active.
    pub enabled: bool,
    /// Position along the axis (normalized 0..1 within bounding box).
    pub position: f32,
    /// Which side to keep: true = keep positive side, false = keep negative.
    pub flip: bool,
}
```

Add to `ViewerState` (after `prev_hover` field, line 98):

```rust
/// Clip planes (X, Y, Z axes).
pub clip_planes: [ClipPlaneState; 3],
/// Flag: clip plane state changed, need to update material uniforms.
pub clip_planes_dirty: bool,
/// Shading mode.
pub shading_mode: ShadingMode,
/// Flag: shading mode changed, need to update materials/meshes.
pub shading_mode_changed: bool,
```

Add `ShadingMode` enum:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ShadingMode {
    #[default]
    Shaded,
    Flat,
    Matcap,
    XRay,
    Wireframe,
}
```

Update `ViewerState::default()` to include new fields with defaults.

**Step 2: Add icons to `src/icons.rs`**

```rust
/// Clip plane X (content_cut rotated — or use a simple letter).
pub(crate) const ICON_CLIP: &str = "\u{e14e}"; // content_cut
```

**Step 3: Add clip toggle buttons to toolbar in `src/ui.rs`**

After the curve edges button (line 977), add:

```rust
ui.separator();

// Clip plane toggles: X, Y, Z.
for (axis_idx, (label, color)) in [
    ("X", egui::Color32::from_rgb(220, 80, 80)),
    ("Y", egui::Color32::from_rgb(80, 200, 80)),
    ("Z", egui::Color32::from_rgb(80, 120, 220)),
].iter().enumerate() {
    let active = state.clip_planes[axis_idx].enabled;
    let text = egui::RichText::new(*label).strong();
    let text = if active { text.color(*color) } else { text };
    let btn = ui.selectable_label(active, text);
    if btn.clicked() {
        state.clip_planes[axis_idx].enabled = !active;
        state.clip_planes_dirty = true;
        state.settings_dirty = true;
    }
    btn.on_hover_text(format!("Clip {} axis", label));
}
```

**Step 4: Add to persistence**

In `PersistentSettings`:

```rust
#[serde(default)]
pub clip_planes: [ClipPlaneState; 3],
#[serde(default)]
pub shading_mode: ShadingMode,
```

Wire up in `auto_save_system` and `main.rs` resource initialization.

**Step 5: Verify**

```bash
cargo check && cargo clippy -- -D warnings
cargo run --release -- <test.step>
```

Expect: X/Y/Z buttons visible in toolbar, toggle on/off (no visual effect yet).

**Step 6: Commit**

```bash
git commit -m "feat: add clip plane state, UI toggle buttons, and persistence"
```

---

### Task 3: Wire clip plane uniforms to material extension

**Files:**
- Modify: `src/scene.rs` — new system `update_clip_plane_uniforms`
- Modify: `src/main.rs` — register the system

**Step 1: Add system to `src/scene.rs`**

```rust
pub(crate) fn update_clip_plane_uniforms(
    mut state: ResMut<ViewerState>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
) {
    if !state.clip_planes_dirty {
        return;
    }
    state.clip_planes_dirty = false;

    let Some(bounds) = state.current_bounds else {
        return;
    };

    // Compute plane equations from clip state.
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    let mut planes = [Vec4::ZERO; 3];
    let mut active = UVec4::ZERO;

    for i in 0..3 {
        let cp = &state.clip_planes[i];
        if cp.enabled {
            let axis = axes[i];
            // Map position 0..1 to bounding box range on this axis.
            let min_val = bounds.min[i];
            let max_val = bounds.max[i];
            let pos = min_val + cp.position * (max_val - min_val);
            let normal = if cp.flip { -axis } else { axis };
            // Plane equation: dot(normal, point) + d = 0
            // Keep the side where dot(normal, point) + d >= 0
            let d = -normal.dot(Vec3::splat(pos) * axis);
            planes[i] = Vec4::new(normal.x, normal.y, normal.z, d);
            active[i] = 1;
        }
    }

    // Update all ViewerMaterial assets.
    for (_id, mat) in materials.iter_mut() {
        mat.extension.clip_planes = planes;
        mat.extension.clip_active = active;
    }
}
```

> **Implementation note:** The plane equation computation needs care. `d = -dot(normal, point_on_plane)`. The point on plane for axis `i` at position `pos` is `pos * axis_unit_vector`. Double-check the math.

**Step 2: Register in `src/main.rs`**

```rust
.add_systems(Update, scene::update_clip_plane_uniforms)
```

**Step 3: Verify the WGSL shader clip discard works**

Ensure the shader's discard logic matches the uniform layout. Enable a clip plane in the UI and verify geometry is clipped.

```bash
cargo run --release -- <test.step>
```

Toggle X clip on → half the model should disappear.

**Step 4: Commit**

```bash
git commit -m "feat: wire clip plane uniforms to shader — geometry clips in real-time"
```

---

### Task 4: Add 3D clip plane drag handles

**Files:**
- Modify: `src/state.rs` — add `ClipPlaneEntity` component
- Modify: `src/scene.rs` — spawn/despawn translucent plane quads, drag system
- Modify: `src/main.rs` — register drag system

**Step 1: Add component to `src/state.rs`**

```rust
#[derive(Component, Debug)]
pub(crate) struct ClipPlaneHandle {
    pub axis: usize, // 0=X, 1=Y, 2=Z
}
```

**Step 2: Spawn/despawn clip plane visuals in `src/scene.rs`**

New system `manage_clip_plane_visuals`:

- When a clip plane is enabled and no entity exists for it: spawn a translucent quad mesh (plane primitive) at the correct position, colored R/G/B for X/Y/Z, with `AlphaMode::Blend` and alpha ~0.2.
- When a clip plane is disabled: despawn its entity.
- When position changes: update the entity's `Transform`.

The quad should be sized to the bounding box (e.g., 2x the bbox extent on the two non-clip axes).

**Step 3: Add drag interaction**

Use Bevy's picking system (already enabled via `MeshPickingPlugin`) to detect drags on the clip plane quad. On drag:

- Project the drag delta onto the clip plane's normal axis.
- Update `state.clip_planes[axis].position` (clamped 0..1).
- Set `state.clip_planes_dirty = true`.

For right-click on the plane handle: toggle `flip` (which side to keep).

**Step 4: Register systems in `src/main.rs`**

```rust
.add_systems(Update, scene::manage_clip_plane_visuals)
.add_systems(Update, scene::drag_clip_plane)
```

**Step 5: Verify**

```bash
cargo run --release -- <test.step>
```

Enable X clip → red translucent plane appears. Drag it along X axis → geometry clips interactively.

**Step 6: Commit**

```bash
git commit -m "feat: add 3D clip plane drag handles with interactive positioning"
```

---

## Phase 3: Shading Modes

### Task 5: Add shading mode dropdown to toolbar

**Files:**
- Modify: `src/ui.rs` — add ComboBox before the quality slider
- Modify: `src/state.rs` — `ShadingMode` already added in Task 2

**Step 1: Add dropdown to `src/ui.rs`**

In the toolbar horizontal layout (line 899), before the quality slider:

```rust
// Shading mode dropdown.
let mode_label = match state.shading_mode {
    ShadingMode::Shaded => "Shaded",
    ShadingMode::Flat => "Flat",
    ShadingMode::Matcap => "Matcap",
    ShadingMode::XRay => "X-Ray",
    ShadingMode::Wireframe => "Wireframe",
};
egui::ComboBox::from_id_salt("shading_mode")
    .selected_text(mode_label)
    .width(90.0)
    .show_ui(ui, |ui| {
        for mode in [
            ShadingMode::Shaded,
            ShadingMode::Flat,
            ShadingMode::Matcap,
            ShadingMode::XRay,
            ShadingMode::Wireframe,
        ] {
            let label = match mode {
                ShadingMode::Shaded => "Shaded",
                ShadingMode::Flat => "Flat",
                ShadingMode::Matcap => "Matcap",
                ShadingMode::XRay => "X-Ray",
                ShadingMode::Wireframe => "Wireframe",
            };
            if ui.selectable_label(state.shading_mode == mode, label).clicked() {
                state.shading_mode = mode;
                state.shading_mode_changed = true;
                state.settings_dirty = true;
            }
        }
    });

ui.separator();
```

**Step 2: Verify**

```bash
cargo check && cargo run --release -- <test.step>
```

Dropdown visible, selecting modes changes state (no visual effect yet).

**Step 3: Commit**

```bash
git commit -m "feat: add shading mode dropdown to viewport toolbar"
```

---

### Task 6: Implement Shaded, Flat, X-Ray, and Wireframe modes

**Files:**
- Modify: `src/scene.rs` — new system `apply_shading_mode`
- Modify: `src/scene.rs:445-536` — add flat normal computation variant to `bevy_mesh_from_polygon_normalized`
- Modify: `src/main.rs` — register system, add `WireframePlugin`
- Modify: `Cargo.toml` — may need bevy `wireframe` feature (check if included in `3d_bevy_render`)

**Step 1: Add `WireframePlugin` to `src/main.rs`**

```rust
use bevy::pbr::wireframe::WireframePlugin;

// In app builder:
.add_plugins(WireframePlugin::default())
```

> **Implementation note:** `WireframePlugin` requires `WgpuFeatures::POLYGON_MODE_LINE`. This should be available on desktop. Check if any feature flag is needed in `Cargo.toml`.

**Step 2: Add flat normal mesh variant**

In `src/scene.rs`, add a function `rebuild_mesh_flat_normals` that takes a `PolygonMesh` and produces a Bevy `Mesh` with per-face normals (compute normal from triangle vertices, assign same normal to all 3 vertices of each triangle). This is the same as `bevy_mesh_from_polygon_normalized` but with computed flat normals instead of using `mesh.normals()`.

**Step 3: Add `apply_shading_mode` system**

```rust
pub(crate) fn apply_shading_mode(
    mut state: ResMut<ViewerState>,
    mut materials: ResMut<Assets<ViewerMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
    face_query: Query<(Entity, &FaceMesh)>,
) {
    if !state.shading_mode_changed {
        return;
    }
    state.shading_mode_changed = false;

    match state.shading_mode {
        ShadingMode::Shaded => {
            // Restore default: opaque, front-face cull, smooth normals.
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Opaque;
                    mat.base.cull_mode = Some(Face::Back);
                    mat.base.base_color = Color::WHITE;
                    mat.extension.shading_flags = 0;
                }
            }
            // Remove Wireframe components.
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
            // Rebuild meshes with smooth normals.
            state.needs_mesh_rebuild = true; // triggers rebuild_meshes_on_toggle
            // Actually need a full mesh rebuild for normals — set a flag.
            state.needs_normal_rebuild = true;
        }
        ShadingMode::Flat => {
            // Same as Shaded but with flat normals.
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Opaque;
                    mat.base.cull_mode = Some(Face::Back);
                    mat.base.base_color = Color::WHITE;
                    mat.extension.shading_flags = 0;
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
            state.needs_normal_rebuild = true;
        }
        ShadingMode::XRay => {
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Blend;
                    mat.base.cull_mode = None; // see both sides
                    mat.base.base_color = Color::srgba(0.7, 0.7, 0.7, 0.3);
                    mat.extension.shading_flags = 0;
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
        }
        ShadingMode::Wireframe => {
            // Add Wireframe component to all face entities.
            // If edges toggle is off: keep backface culling.
            // If edges toggle is on: disable culling for see-through.
            let cull = if state.show_edges { None } else { Some(Face::Back) };
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.cull_mode = cull;
                    mat.base.alpha_mode = AlphaMode::Opaque;
                    mat.base.base_color = Color::WHITE;
                    mat.extension.shading_flags = 0;
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).insert(Wireframe);
            }
        }
        ShadingMode::Matcap => {
            // Set matcap flag in extension, handled in Task 7.
            for face in &state.faces {
                if let Some(mat) = materials.get_mut(&face.material_handle) {
                    mat.base.alpha_mode = AlphaMode::Opaque;
                    mat.base.cull_mode = Some(Face::Back);
                    mat.extension.shading_flags = 1; // matcap bit
                }
            }
            for (entity, _) in face_query.iter() {
                commands.entity(entity).remove::<Wireframe>();
            }
        }
    }
}
```

> **Implementation note:** `needs_normal_rebuild` is a new flag on ViewerState. When true, the mesh rebuild system should recompute normals as flat (for Flat mode) or smooth (for Shaded). This requires storing which mode is active and adjusting the normal computation in `rebuild_meshes_on_toggle` or a new dedicated system.

**Step 4: Handle flat vs smooth normals in mesh rebuild**

Modify `rebuild_meshes_on_toggle` (or add a new system) to check `state.shading_mode == ShadingMode::Flat` and if so, compute flat normals from triangle cross products instead of using `mesh.normals()`.

**Step 5: Register in `src/main.rs`**

```rust
.add_systems(Update, scene::apply_shading_mode)
```

**Step 6: Verify**

```bash
cargo run --release -- <test.step>
```

- Shaded: normal rendering
- Flat: faceted look (visible triangle boundaries)
- X-Ray: transparent, see-through
- Wireframe: line rendering with backface culling

**Step 7: Commit**

```bash
git commit -m "feat: implement Shaded, Flat, X-Ray, and Wireframe shading modes"
```

---

### Task 7: Implement Matcap shading mode

**Files:**
- Create: `assets/matcap.png` — download/embed a neutral clay matcap texture (256x256)
- Modify: `src/viewer_material.rs` — ensure matcap texture binding works
- Modify: `assets/shaders/viewer_material.wgsl` — matcap UV lookup from view-space normal
- Modify: `src/scene.rs` — load matcap texture at startup, set on materials

**Step 1: Embed a matcap texture**

Find or create a neutral gray/clay matcap image (256x256 PNG). The matcap should give a smooth, clean studio-lit appearance. Embed it as a static asset.

**Step 2: Load matcap at startup**

In `setup_scene` or a new startup system, load the matcap image:

```rust
let matcap_handle: Handle<Image> = asset_server.load("matcap.png");
commands.insert_resource(MatcapTexture(matcap_handle));
```

Store as a resource:

```rust
#[derive(Resource)]
pub(crate) struct MatcapTexture(pub Handle<Image>);
```

**Step 3: Set matcap texture on materials when mode is Matcap**

In `apply_shading_mode`, for `ShadingMode::Matcap`:

```rust
mat.extension.matcap_texture = Some(matcap_res.0.clone());
mat.extension.shading_flags = 1;
```

For other modes, clear it:

```rust
mat.extension.matcap_texture = None;
mat.extension.shading_flags = 0;
```

**Step 4: Finalize the WGSL shader matcap logic**

The key is computing view-space normals and using them as UV coordinates into the matcap texture:

```wgsl
if (viewer.shading_flags & 1u) != 0u {
    let world_normal = normalize(in.world_normal);
    // Transform world normal to view space.
    let view_normal = normalize((view.world_from_view * vec4<f32>(world_normal, 0.0)).xyz);
    let uv = vec2<f32>(view_normal.x * 0.5 + 0.5, 1.0 - (view_normal.y * 0.5 + 0.5));
    let matcap_color = textureSample(matcap_texture, matcap_sampler, uv);
    return matcap_color;
}
```

> **Implementation note:** Bevy 0.18 may expose the view matrix differently. Check `bevy_pbr::mesh_view_bindings` for the correct uniform name. It might be `view.view_from_world` instead of `view.world_from_view` — check Bevy 0.18 WGSL bindings.

**Step 5: Verify**

```bash
cargo run --release -- <test.step>
```

Select Matcap from dropdown → model renders with smooth studio-lit appearance.

**Step 6: Commit**

```bash
git commit -m "feat: implement Matcap shading mode with embedded texture"
```

---

## Phase 4: Topology Preservation + Solidify Clip

### Task 8: Add StepTopology enum and preserve CompressedSolid at load time

**Files:**
- Modify: `src/step_loader.rs:96-113` — add `StepTopology` enum, add to `StepShell`
- Modify: `src/lib.rs` — export `StepTopology`
- Modify: `Cargo.toml` — add `"solid"` feature to monstertruck dependency

**Step 1: Add monstertruck solid feature to `Cargo.toml`**

```toml
monstertruck = { version = "0.1", features = ["step", "meshing", "solid", "modeling"] }
```

**Step 2: Add `StepTopology` enum to `src/step_loader.rs`**

After `CompressedShellData` (line 84):

```rust
use monstertruck::topology::compress::CompressedSolid;

type OriginalSolid = CompressedSolid<Point3, Curve3D, Surface>;

/// Whether the original STEP entity was a solid or a shell.
#[derive(Clone, Debug)]
pub enum StepTopology {
    /// From manifold_solid_brep — watertight, suitable for boolean ops.
    Solid(CompressedShellData), // wraps OriginalSolid
    /// From shell_based_surface_model — open surface, no boolean ops.
    Shell(CompressedShellData), // wraps OriginalShell
}
```

> We reuse `CompressedShellData` (which wraps `Arc<dyn Any + Send + Sync>`) to avoid making `StepShell` generic. The downcast distinguishes the two at runtime.

**Step 3: Add `topology` field to `StepShell`**

```rust
pub struct StepShell {
    // ... existing fields ...
    /// Original topology for re-tessellation and boolean ops.
    pub topology: Option<StepTopology>,
}
```

Remove the existing `original_shell` field — `topology` replaces it.

**Step 4: Modify the loader to check `manifold_solid_brep` first**

In `load_step_streaming_inner`, after parsing the table:

```rust
// Check if this shell ID corresponds to a manifold_solid_brep.
// If so, use to_compressed_solid; otherwise use to_compressed_shell.
if let Some(solid_holder) = table.manifold_solid_brep.values().find(|s| {
    // Match shell ID to solid's outer shell
    // This requires checking the solid's shell references
}) {
    let csolid = table.to_compressed_solid(solid_holder)?;
    // Store as StepTopology::Solid(CompressedShellData::new(csolid.clone()))
    // Tessellate via csolid.boundaries[0].robust_triangulation(tol)
}
```

> **Implementation note:** The mapping from shell entity IDs to solid entity IDs needs investigation. The `ManifoldSolidBrepHolder` references shells by entity ID. The implementing agent should check how `table.shell` keys relate to `table.manifold_solid_brep` entries to determine which shells are part of solids. If the relationship is indirect, it may be simpler to iterate `manifold_solid_brep` directly instead of `table.shell`.

**Step 5: Update `retessellate_face` to use `StepTopology`**

The existing `retessellate_face` function uses `CompressedShellData` — update it to extract the shell from either `StepTopology::Solid` or `StepTopology::Shell`.

**Step 6: Export from `src/lib.rs`**

```rust
pub use step_loader::{StepTopology, ...};
```

**Step 7: Verify**

```bash
cargo check && cargo clippy -- -D warnings
```

**Step 8: Commit**

```bash
git commit -m "feat: preserve CompressedSolid topology at STEP load time"
```

---

### Task 9: Implement Solidify Clip system

**Files:**
- Modify: `src/state.rs` — add `solidify_job` field to `ViewerState`
- Modify: `src/scene.rs` — new systems `start_solidify_clip`, `poll_solidify_clip`
- Modify: `src/ui.rs` — add "Solidify" button (grayed out for shells)
- Modify: `src/main.rs` — register systems

**Step 1: Add solidify state to `ViewerState`**

```rust
/// In-flight solidify clip job.
pub solidify_job: Option<SolidifyJob>,
/// Whether the loaded model has solid topology (enables Solidify button).
pub has_solid_topology: bool,
```

```rust
pub struct SolidifyJob {
    pub receiver: parking_lot::Mutex<std::sync::mpsc::Receiver<Result<StepScene, String>>>,
}
```

**Step 2: Add "Solidify" button to `src/ui.rs`**

In the toolbar, after clip plane buttons:

```rust
let any_clip_active = state.clip_planes.iter().any(|cp| cp.enabled);
let can_solidify = state.has_solid_topology && any_clip_active && state.solidify_job.is_none();

let solidify_btn = ui.add_enabled(
    can_solidify,
    egui::Button::new("Solidify"),
);
if solidify_btn.clicked() {
    state.start_solidify = true;
}
if !state.has_solid_topology {
    solidify_btn.on_hover_text("Only available for STEP solids (manifold_solid_brep)");
} else if !any_clip_active {
    solidify_btn.on_hover_text("Enable at least one clip plane first");
} else {
    solidify_btn.on_hover_text("Apply boolean clip to create watertight geometry");
}
```

**Step 3: Implement `start_solidify_clip` system**

```rust
pub(crate) fn start_solidify_clip(
    mut state: ResMut<ViewerState>,
) {
    if !state.start_solidify {
        return;
    }
    state.start_solidify = false;

    // Gather clip planes, topology data, tolerance.
    // Spawn background thread:
    //   1. Extract Solid from CompressedSolid
    //   2. For each active clip plane:
    //      a. Build a cuboid (half-space box) using monstertruck::modeling::builder::cuboid
    //      b. Call monstertruck::solid::and(&current_solid, &halfspace, tol)
    //   3. Tessellate the result
    //   4. Send back via channel

    let (tx, rx) = std::sync::mpsc::channel();
    state.solidify_job = Some(SolidifyJob {
        receiver: parking_lot::Mutex::new(rx),
    });

    // Clone necessary data for the background thread.
    // ...

    std::thread::spawn(move || {
        let result = run_solidify(/* args */);
        let _ = tx.send(result);
    });
}
```

**Step 4: Implement `run_solidify` helper**

```rust
fn run_solidify(
    topology: &StepTopology,
    clip_planes: [(bool, Vec4); 3], // (enabled, plane_equation)
    bounds: Bounds,
    tolerance: f64,
) -> Result<StepScene, String> {
    let StepTopology::Solid(data) = topology else {
        return Err("not a solid".into());
    };

    let csolid: &OriginalSolid = data.downcast_ref()
        .ok_or("type mismatch")?;

    let mut solid = monstertruck::topology::Solid::extract(csolid.clone())
        .map_err(|e| format!("solid extraction failed: {e}"))?;

    for (i, (enabled, plane_eq)) in clip_planes.iter().enumerate() {
        if !*enabled { continue; }

        // Build half-space box: a large cuboid on the "keep" side of the plane.
        // Size it to 2x bounding box so it fully contains the model on one side.
        let bbox_size = /* 2x bounds extent */;
        let halfspace_bbox = /* construct BoundingBox on keep side */;
        let halfspace_solid = monstertruck::modeling::builder::cuboid(halfspace_bbox);

        solid = monstertruck::solid::and(&solid, &halfspace_solid, tolerance)
            .map_err(|e| format!("boolean and failed on axis {i}: {e}"))?;
    }

    // Tessellate the result.
    let compressed = solid.compress();
    let poly_shell = compressed.robust_triangulation(tolerance);

    // Convert to StepScene...
    // (extract faces as StepFace, build StepShell, wrap in StepScene)
}
```

> **Implementation note:** The exact `cuboid` API and `solid.compress()` method need verification. The implementing agent should check `monstertruck::modeling::builder::cuboid` signature and `Solid::compress()` or equivalent. Also, `and()` requires `C: ShapeOpsCurve<S>, S: ShapeOpsSurface` — verify that `Curve3D` and `Surface` implement these traits (they should, since boolean ops are the whole point).

**Step 5: Implement `poll_solidify_clip` system**

Poll the receiver. On success: replace scene_data, despawn old meshes, spawn new meshes (same as the load pipeline in `process_load_requests`).

**Step 6: Register systems in `src/main.rs`**

```rust
.add_systems(Update, scene::start_solidify_clip)
.add_systems(Update, scene::poll_solidify_clip)
```

**Step 7: Verify**

```bash
cargo check && cargo clippy -- -D warnings
cargo run --release -- <test.step>
```

Load a solid STEP file. Enable a clip plane. Click "Solidify". After processing, the model should show clean clipped geometry with cap faces.

**Step 8: Commit**

```bash
git commit -m "feat: implement Solidify Clip with monstertruck boolean AND"
```

---

## Verification Checklist

After all tasks are complete:

1. `cargo check && cargo clippy -- -D warnings` — clean build
2. `cargo run --release -- <test.step>`:
   - [ ] Shading dropdown visible with 5 modes
   - [ ] Shaded mode: default lit rendering
   - [ ] Flat mode: faceted/facet-shaded look
   - [ ] Matcap mode: smooth studio-lit appearance
   - [ ] X-Ray mode: translucent see-through
   - [ ] Wireframe mode: line rendering; see-through when edges toggle is on
   - [ ] X/Y/Z clip buttons in toolbar, colored R/G/B when active
   - [ ] Clip plane shows translucent colored quad in viewport
   - [ ] Dragging clip plane handle clips geometry interactively
   - [ ] Multiple clip planes can be active simultaneously
   - [ ] Solidify button enabled only for STEP solids with active clip planes
   - [ ] Solidify produces clean clipped geometry on background thread
   - [ ] Settings persist across sessions (shading mode, clip plane state)
   - [ ] No regressions: selection highlighting, edge rendering, browser mode all still work
