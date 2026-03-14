#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
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
// shading_flags: bit 0 = matcap mode (reserved for later use).
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
