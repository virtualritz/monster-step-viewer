# Clipping Planes + Shading Modes

## Overview

Two interrelated features for the viewer:

1. **Shading modes** — dropdown to switch between 5 rendering styles.
2. **Clipping planes** — up to 3 axis-aligned clip planes with interactive 3D drag handles, shader-based real-time preview, and optional CAD boolean solidification for true STEP solids.

---

## 1. Shading Modes

A dropdown in the toolbar with 5 modes:

| Mode | Implementation |
|------|---------------|
| **Shaded** | Current default. `StandardMaterial`, smooth vertex normals, vertex colors. |
| **Flat** | Same `StandardMaterial` but regenerate meshes with per-face normals (duplicate vertices per triangle, flat normal). Toggle via `needs_mesh_rebuild`. |
| **Matcap** | `ExtendedMaterial<StandardMaterial, MatcapExtension>` with custom fragment shader. Samples a matcap texture using view-space normals as UV. Ship 1-2 matcap textures as embedded assets. Unlit. |
| **X-Ray** | `StandardMaterial` with `alpha_mode: AlphaMode::Blend`, `base_color` alpha ~0.3, `cull_mode: None` (see both sides). |
| **Wireframe** | Bevy's built-in `WireframePlugin` + `Wireframe` component on all face entities. Backface culling on by default. If edges toggle is active, set `cull_mode: None` for see-through wireframe. |

Switching modes updates materials on all `FaceMesh` entities. Flat mode also triggers mesh rebuild for normals. State stored as `ShadingMode` enum in `ViewerState`, persisted.

---

## 2. Shader Clip Planes

### Mechanism

`ExtendedMaterial<StandardMaterial, ClipPlaneExtension>` with a custom fragment shader that discards fragments on the negative side of up to 3 plane equations.

```rust
#[derive(Asset, Clone, AsBindGroup)]
struct ClipPlaneExtension {
    #[uniform(100)]
    clip_planes: [Vec4; 3],  // (nx, ny, nz, d) -- w=0 means disabled
    #[uniform(101)]
    active_mask: UVec4,      // x,y,z = enabled flags
}
```

Fragment shader (`assets/shaders/clip_plane.wgsl`):

```wgsl
for (var i = 0u; i < 3u; i++) {
    if active_mask[i] != 0u {
        if dot(world_position.xyz, clip_planes[i].xyz) + clip_planes[i].w < 0.0 {
            discard;
        }
    }
}
```

### 3D Drag Handles

Each active clip plane renders as:

- A translucent colored quad (R/G/B for X/Y/Z axis) sized to the bounding box, rendered as a separate entity with `AlphaMode::Blend`.
- An arrow gizmo along the plane normal for dragging.
- Dragging uses Bevy's picking to detect drag on the arrow, translates the plane along its normal.
- Plane position clamped to bounding box extents on each axis. Default position: center of bounding box.

### Cap Faces

Not in initial implementation. Show hollow interior (standard for CAD viewers). Can add cap face rendering later as a second pass.

### Toggle UI

Three small axis-labeled buttons (X/Y/Z) in the toolbar, each independently toggleable. Active buttons show their axis color (red/green/blue).

---

## 3. Solidify Clip (CAD Boolean)

### Gating

Only available when the STEP file loaded as `manifold_solid_brep`. The UI shows a "Solidify" button grayed out for shell-based models with a tooltip explaining why.

### Pipeline

1. Construct a large box solid (2x bounding box) on the "keep" side of each active clip plane.
2. `and()` the original `Solid` with each half-space box sequentially.
3. `robust_triangulation(tol)` on the result.
4. Replace displayed meshes.

### Background Execution

Spawns on a background thread with timeout (same pattern as preview loading). Shows a spinner. On timeout, display error toast; original geometry unchanged.

### STEP Loader Changes

Preserve topology kind at load time:

```rust
pub enum StepTopology {
    Solid(CompressedSolid<Point3, Curve3D, Surface>),
    Shell(CompressedShell<Point3, Curve3D, Surface>),
}
```

Stored in `StepShell`. The loader checks `table.manifold_solid_brep` first; falls back to `table.shell_based_surface_model`.

### Solid Extraction Path

For real solids:

1. `Table::to_compressed_solid(solid_holder)` -> `CompressedSolid<Point3, Curve3D, Surface>`
2. Optional robust heal: `robust_split_closed_edges_and_faces(tol)` on the compressed solid
3. `Solid::extract(compressed_solid)` -> `Solid<Point3, Curve3D, Surface>`
4. `and(&model_solid, &halfspace_solid, tol)` -> clipped `Solid`

Shell-based models (`shell_based_surface_model`) cannot use solidify; they lack guaranteed closed volume. Best-effort promotion (wrapping closed shell into `CompressedSolid`) can be added later as an explicit, validated option.

---

## 4. Files to Change

| File | Changes |
|------|---------|
| `src/step_loader.rs` | `StepTopology` enum. Load via `to_compressed_solid` when available. Store in `StepShell`. |
| `src/lib.rs` | Export `StepTopology` and new types. |
| `src/state.rs` | `ShadingMode` enum, `ClipPlane` struct, new fields on `ViewerState`. |
| `src/scene.rs` | Material swapping for shading modes, clip plane entities + drag system, solidify system. |
| `src/ui.rs` | Shading dropdown, clip plane axis buttons, solidify button. |
| `src/persistence.rs` | Persist `ShadingMode`. |
| `src/icons.rs` | Icons for clip plane axes. |
| `src/main.rs` | Register new systems, `WireframePlugin`. |
| `assets/shaders/clip_plane.wgsl` | Fragment shader for clip plane discard. |
| `assets/shaders/matcap.wgsl` | Fragment shader for matcap rendering. |
| `assets/matcap_default.png` | Embedded matcap texture. |
