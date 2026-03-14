use bevy::{
    asset::RenderAssetUsages,
    image::ImageSampler,
    pbr::{ExtendedMaterial, MaterialExtension},
    prelude::*,
    render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat},
    shader::ShaderRef,
};

/// Type alias for the viewer's extended material.
pub(crate) type ViewerMaterial = ExtendedMaterial<StandardMaterial, ViewerMaterialExt>;

/// Fragment shader path (relative to `assets/`).
const SHADER_PATH: &str = "shaders/viewer_material.wgsl";

/// Resource holding the procedurally generated matcap texture handle.
#[derive(Resource)]
pub(crate) struct MatcapTexture(pub Handle<Image>);

/// Material extension carrying clip-plane and shading uniforms.
///
/// Binding slot 100 avoids conflicts with `StandardMaterial` bindings (0-99).
/// The uniform struct must be 16-byte aligned, so we pad `shading_flags` to
/// a full `UVec4` worth of space.
///
/// Binding slots 101/102 hold the matcap texture and sampler.
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
    /// Bit 0 = matcap mode.
    #[uniform(100)]
    pub shading_flags: u32,
    #[uniform(100)]
    pub _pad1: u32,
    #[uniform(100)]
    pub _pad2: u32,
    #[uniform(100)]
    pub _pad3: u32,

    /// Matcap texture — `None` uses a fallback 1x1 white texture.
    #[texture(101)]
    #[sampler(102)]
    pub matcap_texture: Option<Handle<Image>>,
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
            matcap_texture: None,
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

/// Generate a 256x256 matcap image with a neutral gray clay/studio-lit look.
fn generate_matcap_image() -> Image {
    const SIZE: usize = 256;
    let mut data = Vec::with_capacity(SIZE * SIZE * 4);

    // Light direction: slightly right, up, towards viewer.
    let light = Vec3::new(0.3, 0.5, 0.8).normalize();

    for v in 0..SIZE {
        for u in 0..SIZE {
            let nx = (u as f32 - 128.0) / 128.0;
            let ny = (128.0 - v as f32) / 128.0;
            let r2 = nx * nx + ny * ny;

            let nz = (1.0 - r2.min(1.0)).sqrt();
            let normal = Vec3::new(nx, ny, nz).normalize();

            let diffuse = normal.dot(light).max(0.0) * 0.7 + 0.3;

            // Specular: reflect(-light, normal) dot view(0,0,1).
            let reflect = 2.0 * normal.dot(light) * normal - light;
            let spec_dot = reflect.z.max(0.0); // dot with (0,0,1)
            let specular = spec_dot.powf(32.0) * 0.4;

            let brightness = (diffuse + specular).min(1.0);

            // Slight warm tint: R > G > B.
            let r = (brightness * 200.0).min(255.0) as u8;
            let g = (brightness * 195.0).min(255.0) as u8;
            let b = (brightness * 190.0).min(255.0) as u8;

            // Outside the sphere radius, darken smoothly.
            if r2 > 1.0 {
                data.extend_from_slice(&[30, 30, 30, 255]);
            } else {
                data.extend_from_slice(&[r, g, b, 255]);
            }
        }
    }

    Image::new(
        Extent3d {
            width: SIZE as u32,
            height: SIZE as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

/// Startup system that creates the matcap texture and inserts it as a resource.
pub(crate) fn setup_matcap_texture(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
) {
    let mut image = generate_matcap_image();
    image.sampler = ImageSampler::linear();
    let handle = images.add(image);
    commands.insert_resource(MatcapTexture(handle));
}
