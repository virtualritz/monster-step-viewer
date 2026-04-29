// Custom forward vertex+fragment shader for ViewerMaterial.
//
// Adds a per-vertex `face_id` attribute (location 8, Uint32) which the
// vertex shader passes flat-interpolated to the fragment shader. The
// fragment shader uses face_id to look up per-face state (selected /
// hovered / hidden) from the `face_state` storage buffer at binding 103.
//
// Selection / hover modify the StandardMaterial's emissive before lighting;
// hidden faces discard at fragment level.

#import bevy_pbr::{
    forward_io::{Vertex, VertexOutput, FragmentOutput},
    mesh_functions,
    mesh_view_bindings::view,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    view_transformations::position_world_to_clip,
}

#ifdef PREPASS_PIPELINE
#import bevy_pbr::pbr_deferred_functions::deferred_output;
#else
#import bevy_pbr::pbr_functions::{
    apply_pbr_lighting,
    main_pass_post_lighting_processing,
};
#endif

// Viewer material extension uniforms.
//
// clip_plane_0..2: each vec4 encodes a plane as (normal.xyz, distance).
//   A fragment is clipped (discarded) when dot(normal, world_pos) + distance > 0.
//
// clip_active: bitmask in .x — bit 0 = plane 0, bit 1 = plane 1, bit 2 = plane 2.
//
// shading_flags: bit 0 = matcap mode.
struct ViewerMaterialExt {
    clip_plane_0: vec4<f32>,
    clip_plane_1: vec4<f32>,
    clip_plane_2: vec4<f32>,
    clip_active: vec4<u32>,
    shading_flags: u32,
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100)
var<uniform> viewer_ext: ViewerMaterialExt;

@group(#{MATERIAL_BIND_GROUP}) @binding(101)
var matcap_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102)
var matcap_sampler: sampler;

// Per-face state bits.
// bit 0 = selected, bit 1 = hovered, bit 2 = hidden
@group(#{MATERIAL_BIND_GROUP}) @binding(103)
var<storage, read> face_state: array<u32>;

// Custom vertex input that adds face_id alongside the standard attributes
// pulled from `Vertex`. Locations match the specialize() layout in Rust.
struct ViewerVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(5) color: vec4<f32>,
    @location(8) face_id: u32,
}

// Mirrors bevy_pbr::forward_io::VertexOutput exactly, plus a flat-
// interpolated face_id at @location(8). Keeps locations identical so we
// can hand a constructed `bevy_pbr::forward_io::VertexOutput` to the PBR
// helper functions.
struct ViewerVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(5) color: vec4<f32>,
    @location(6) @interpolate(flat) instance_index: u32,
    @location(8) @interpolate(flat) face_id: u32,
}

@vertex
fn vertex(in: ViewerVertex) -> ViewerVertexOutput {
    var out: ViewerVertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(in.position, 1.0),
    );
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        in.normal,
        in.instance_index,
    );
    out.color = in.color;
    out.instance_index = in.instance_index;
    out.face_id = in.face_id;
    return out;
}

// Build a stock VertexOutput from our extended one so we can call PBR
// helpers that expect it.
fn to_pbr(in: ViewerVertexOutput) -> VertexOutput {
    var out: VertexOutput;
    out.position = in.position;
    out.world_position = in.world_position;
    out.world_normal = in.world_normal;
#ifdef VERTEX_COLORS
    out.color = in.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = in.instance_index;
#endif
    return out;
}

fn face_state_for(face_id: u32) -> u32 {
    let len = arrayLength(&face_state);
    if face_id >= len {
        return 0u;
    }
    return face_state[face_id];
}

@fragment
fn fragment(
    in: ViewerVertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    let state = face_state_for(in.face_id);
    if (state & 4u) != 0u {
        discard;
    }

    let world_pos = in.world_position.xyz;
    let clip_mask = viewer_ext.clip_active.x;
    if (clip_mask & 1u) != 0u {
        let plane = viewer_ext.clip_plane_0;
        if dot(plane.xyz, world_pos) + plane.w > 0.0 {
            discard;
        }
    }
    if (clip_mask & 2u) != 0u {
        let plane = viewer_ext.clip_plane_1;
        if dot(plane.xyz, world_pos) + plane.w > 0.0 {
            discard;
        }
    }
    if (clip_mask & 4u) != 0u {
        let plane = viewer_ext.clip_plane_2;
        if dot(plane.xyz, world_pos) + plane.w > 0.0 {
            discard;
        }
    }

    let pbr_in = to_pbr(in);

#ifndef PREPASS_PIPELINE
    if (viewer_ext.shading_flags & 1u) != 0u {
        // Matcap path.
        let world_normal = normalize(in.world_normal);
        let view_normal = normalize((view.view_from_world * vec4<f32>(world_normal, 0.0)).xyz);
        let uv = vec2<f32>(view_normal.x * 0.5 + 0.5, 1.0 - (view_normal.y * 0.5 + 0.5));
        var out: FragmentOutput;
        out.color = textureSample(matcap_texture, matcap_sampler, uv);
        if (state & 1u) != 0u {
            out.color = out.color + vec4<f32>(0.6, 0.45, 0.0, 0.0);
        } else if (state & 2u) != 0u {
            out.color = out.color + vec4<f32>(0.2, 0.15, 0.0, 0.0);
        }
        return out;
    }
#endif

    // Standard PBR path.
    var pbr_input = pbr_input_from_standard_material(pbr_in, is_front);
    pbr_input.material.base_color = alpha_discard(
        pbr_input.material,
        pbr_input.material.base_color,
    );

    if (state & 1u) != 0u {
        pbr_input.material.emissive = vec4<f32>(1.6, 1.1, 0.0, 0.0);
    } else if (state & 2u) != 0u {
        pbr_input.material.emissive = vec4<f32>(0.6, 0.45, 0.0, 0.0);
    }

#ifdef PREPASS_PIPELINE
    let out = deferred_output(pbr_in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
