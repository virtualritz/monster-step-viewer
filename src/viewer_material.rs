use bevy::{
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::render_resource::AsBindGroup,
    shader::ShaderRef,
};

/// Type alias for the viewer's extended material.
pub(crate) type ViewerMaterial = ExtendedMaterial<StandardMaterial, ViewerMaterialExt>;

/// Fragment shader path (relative to `assets/`).
const SHADER_PATH: &str = "shaders/viewer_material.wgsl";

/// Material extension carrying clip-plane and shading uniforms.
///
/// Binding slot 100 avoids conflicts with `StandardMaterial` bindings (0-99).
/// The uniform struct must be 16-byte aligned, so we pad `shading_flags` to
/// a full `UVec4` worth of space.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub(crate) struct ViewerMaterialExt {
    /// Clip plane 0: `(normal.xyz, distance)`.
    #[uniform(100)]
    pub clip_plane_0: Vec4,
    /// Clip plane 1.
    #[uniform(100)]
    pub clip_plane_1: Vec4,
    /// Clip plane 2.
    #[uniform(100)]
    pub clip_plane_2: Vec4,
    /// Bitmask in `.x` — bit 0/1/2 enable planes 0/1/2.
    #[uniform(100)]
    pub clip_active: UVec4,
    /// Bit 0 = matcap mode (reserved).
    #[uniform(100)]
    pub shading_flags: u32,
    #[uniform(100)]
    pub _pad1: u32,
    #[uniform(100)]
    pub _pad2: u32,
    #[uniform(100)]
    pub _pad3: u32,
}

impl Default for ViewerMaterialExt {
    fn default() -> Self {
        Self {
            clip_plane_0: Vec4::ZERO,
            clip_plane_1: Vec4::ZERO,
            clip_plane_2: Vec4::ZERO,
            clip_active: UVec4::ZERO,
            shading_flags: 0,
            _pad1: 0,
            _pad2: 0,
            _pad3: 0,
        }
    }
}

impl MaterialExtension for ViewerMaterialExt {
    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }
}
