use bevy::{
    asset::RenderAssetUsages,
    image::ImageSampler,
    mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef},
    pbr::{
        ExtendedMaterial, MaterialExtension, MaterialExtensionKey,
        MaterialExtensionPipeline,
    },
    prelude::*,
    render::{
        render_resource::{
            AsBindGroup, Extent3d, RenderPipelineDescriptor,
            SpecializedMeshPipelineError, TextureDimension, TextureFormat,
            VertexFormat,
        },
        storage::ShaderStorageBuffer,
    },
    shader::ShaderRef,
};

/// Per-vertex global face id (read by the custom vertex shader and passed
/// flat to the fragment shader for per-face state lookup).
pub(crate) const ATTRIBUTE_FACE_ID: MeshVertexAttribute =
    MeshVertexAttribute::new("ViewerFaceId", 0xa1f0_0001, VertexFormat::Uint32);

/// Bit flags packed into each `face_state` entry.
pub(crate) const FACE_STATE_SELECTED: u32 = 1 << 0;
pub(crate) const FACE_STATE_HOVERED: u32 = 1 << 1;
pub(crate) const FACE_STATE_HIDDEN: u32 = 1 << 2;
pub(crate) const FACE_STATE_ANNOTATION_SHIFT: u32 = 3;

/// Use matcap shading in the custom fragment shader.
pub(crate) const SHADING_FLAG_MATCAP: u32 = 1 << 0;
/// Derive a per-fragment geometric normal for flat display mode.
pub(crate) const SHADING_FLAG_FLAT: u32 = 1 << 1;

/// Type alias for the viewer's extended material.
pub(crate) type ViewerMaterial =
    ExtendedMaterial<StandardMaterial, ViewerMaterialExt>;

/// Fragment shader path (relative to `assets/`).
const SHADER_PATH: &str = "shaders/viewer_material.wgsl";

/// Resource holding the procedurally generated matcap texture handle.
#[derive(Resource)]
pub(crate) struct MatcapTexture(pub Handle<Image>);

/// Three shared `ViewerMaterial` handles cover every face mesh in the scene:
/// the default appearance plus selection and hover variants whose only
/// difference is `base.emissive`. Sharing materials across faces collapses
/// the draw-call count to roughly one per (mesh, palette-slot) pair so Bevy
/// can batch instead of submitting one draw per face.
#[derive(Resource, Clone)]
pub(crate) struct MaterialPalette {
    pub default: Handle<ViewerMaterial>,
    pub selected: Handle<ViewerMaterial>,
    pub hovered: Handle<ViewerMaterial>,
}

/// Selection emissive (warm orange) baked into the `selected` palette entry.
pub(crate) const SELECTION_EMISSIVE: LinearRgba =
    LinearRgba::new(0.6, 0.45, 0.0, 1.0);
/// Hover emissive — same hue, dimmer.
pub(crate) const HOVER_EMISSIVE: LinearRgba =
    LinearRgba::new(0.2, 0.15, 0.0, 1.0);

fn base_material() -> ViewerMaterial {
    use bevy::pbr::ExtendedMaterial;
    ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.4,
            metallic: 0.0,
            ..Default::default()
        },
        extension: ViewerMaterialExt::default(),
    }
}

/// Create the three shared face materials and the per-face state buffer.
pub(crate) fn setup_material_palette(
    mut commands: Commands,
    mut materials: ResMut<Assets<ViewerMaterial>>,
    mut storage_buffers: ResMut<Assets<ShaderStorageBuffer>>,
) {
    // One zero-initialised u32 keeps the buffer non-empty (WGSL storage
    // buffers can't be zero-sized). Resized by `update_face_state_buffer`.
    let mut initial_buffer = ShaderStorageBuffer::default();
    initial_buffer.set_data(vec![0u32]);
    let face_state_handle = storage_buffers.add(initial_buffer);

    let mk = |emissive: Option<LinearRgba>| {
        let mut mat = base_material();
        if let Some(e) = emissive {
            mat.base.emissive = e;
        }
        mat.extension.face_state = face_state_handle.clone();
        mat
    };

    let default = materials.add(mk(None));
    let selected = materials.add(mk(Some(SELECTION_EMISSIVE)));
    let hovered = materials.add(mk(Some(HOVER_EMISSIVE)));

    commands.insert_resource(MaterialPalette {
        default,
        selected,
        hovered,
    });
    commands.insert_resource(FaceStateBuffer(face_state_handle));
}

/// Material extension carrying clip-plane and shading uniforms.
///
/// Binding slot 100 avoids conflicts with `StandardMaterial` bindings (0-99).
/// The uniform struct must be 16-byte aligned, so we pad `shading_flags` to
/// a full `vec4<u32>` uniform slot.
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
    /// Bit 0 = matcap mode, bit 1 = flat normal mode.
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

    /// Per-global-face-id state bits (see `FACE_STATE_*` constants). Indexed
    /// in the fragment shader by the per-vertex `face_id` attribute. The
    /// underlying buffer is a separate asset; mutating its data triggers a
    /// re-upload to GPU.
    #[storage(103, read_only)]
    pub face_state: Handle<ShaderStorageBuffer>,
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
            face_state: Handle::default(),
        }
    }
}

/// Resource holding the shared `ShaderStorageBuffer` handle that backs
/// every palette material's `face_state`. The update system mutates this
/// asset; all bound materials see the change automatically.
#[derive(Resource, Clone)]
pub(crate) struct FaceStateBuffer(pub Handle<ShaderStorageBuffer>);

impl MaterialExtension for ViewerMaterialExt {
    fn vertex_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    fn deferred_fragment_shader() -> ShaderRef {
        SHADER_PATH.into()
    }

    /// Skip the depth/normal prepass — we don't use SSAO/TAA/etc. and the
    /// stock prepass vertex shader expects more attributes than our custom
    /// vertex layout provides. Re-enable when prepass is wired up properly.
    fn enable_prepass() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        use bevy::pbr::MeshPipelineKey;

        // Only override the vertex layout for the forward pipeline. Prepass /
        // deferred / shadow pipelines use Bevy's stock vertex shader, which
        // expects the default attribute layout — overriding here would mean
        // those pipelines see attributes their shader doesn't declare.
        let prepass_bits = MeshPipelineKey::DEPTH_PREPASS
            | MeshPipelineKey::NORMAL_PREPASS
            | MeshPipelineKey::MOTION_VECTOR_PREPASS
            | MeshPipelineKey::DEFERRED_PREPASS;
        if key.mesh_key.intersects(prepass_bits) {
            return Ok(());
        }

        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(5),
            ATTRIBUTE_FACE_ID.at_shader_location(8),
        ])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
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
