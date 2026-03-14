#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    mesh_view_bindings::view,
}

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
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

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    // --- Clip plane discard ---
    let world_pos = in.world_position.xyz;
    let active = viewer_ext.clip_active.x;

    if (active & 1u) != 0u {
        let plane = viewer_ext.clip_plane_0;
        if dot(plane.xyz, world_pos) + plane.w > 0.0 {
            discard;
        }
    }
    if (active & 2u) != 0u {
        let plane = viewer_ext.clip_plane_1;
        if dot(plane.xyz, world_pos) + plane.w > 0.0 {
            discard;
        }
    }
    if (active & 4u) != 0u {
        let plane = viewer_ext.clip_plane_2;
        if dot(plane.xyz, world_pos) + plane.w > 0.0 {
            discard;
        }
    }

#ifndef PREPASS_PIPELINE
    // --- Matcap shading path (forward only) ---
    if (viewer_ext.shading_flags & 1u) != 0u {
        // Transform world normal to view space for matcap UV lookup.
        let world_normal = normalize(in.world_normal);
        let view_normal = normalize((view.view_from_world * vec4(world_normal, 0.0)).xyz);
        // Map view-space normal XY to UV: X goes left-to-right, Y goes top-to-bottom.
        let uv = vec2(view_normal.x * 0.5 + 0.5, 1.0 - (view_normal.y * 0.5 + 0.5));
        var out: FragmentOutput;
        out.color = textureSample(matcap_texture, matcap_sampler, uv);
        return out;
    }
#endif

    // --- Standard PBR path ---
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    // Alpha discard.
    pbr_input.material.base_color = alpha_discard(
        pbr_input.material,
        pbr_input.material.base_color,
    );

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif

    return out;
}
