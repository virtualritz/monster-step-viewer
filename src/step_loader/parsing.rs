use std::collections::{HashMap, HashSet};

use super::transform::Transform;

/// Preprocess STEP file to join multi-line entities.
pub(crate) fn preprocess_step_entities(raw: &str) -> String {
    // STEP entities can span multiple lines, ending with ;.
    // Join lines to make parsing easier.
    let mut result = String::with_capacity(raw.len());
    for line in raw.lines() {
        let line = line.trim();
        if !line.is_empty() {
            if !result.is_empty() && !result.ends_with(';') {
                result.push(' ');
            }
            result.push_str(line);
        }
    }
    result
}

/// Parse assembly transforms from raw STEP file content.
/// Returns a map from shell entity ID to world transform.
/// Uses foxtrot's approach: build parent->child graph, detect roots, traverse top-down.
pub(crate) fn parse_assembly_transforms(raw: &str) -> HashMap<u64, Transform> {
    // Parse basic geometric entities.
    let mut cartesian_points: HashMap<u64, [f64; 3]> = HashMap::new();
    let mut directions: HashMap<u64, [f64; 3]> = HashMap::new();
    let mut placement_refs: HashMap<u64, (u64, u64, u64)> = HashMap::new();
    // ITEM_DEFINED_TRANSFORMATION: id -> (from_placement, to_placement).
    let mut item_transforms: HashMap<u64, (u64, u64)> = HashMap::new();
    // REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION: (rep_1, rep_2, transform_id).
    let mut rep_relationships: Vec<(u64, u64, u64)> = Vec::new();
    // MANIFOLD_SOLID_BREP: manifold_id -> shell_id.
    let mut manifold_to_shell: HashMap<u64, u64> = HashMap::new();
    // ADVANCED_BREP_SHAPE_REPRESENTATION: absr_id -> Vec<all refs including manifolds>.
    let mut absr_refs: HashMap<u64, Vec<u64>> = HashMap::new();
    // SHAPE_REPRESENTATION_RELATIONSHIP: (rep_1, rep_2) - links reps.
    let mut shape_rep_relationships: Vec<(u64, u64)> = Vec::new();
    // SHAPE_REPRESENTATION: id -> Vec<item_refs>.
    let mut _shape_reps: HashMap<u64, Vec<u64>> = HashMap::new();

    // Preprocess: join multi-line entities.
    let joined = preprocess_step_entities(raw);

    // First pass: collect all entity definitions.
    for entity in joined.split(';') {
        let entity = entity.trim();
        let Some(rest) = entity.strip_prefix('#') else {
            continue;
        };
        let Some((id_str, rest)) = rest.split_once('=') else {
            continue;
        };
        let Ok(id) = id_str.trim().parse::<u64>() else {
            continue;
        };
        let rest = rest.trim();

        if rest.starts_with("CARTESIAN_POINT") {
            if let Some(coords) = parse_point_coords(rest) {
                cartesian_points.insert(id, coords);
            }
        } else if rest.starts_with("DIRECTION") {
            if let Some(coords) = parse_point_coords(rest) {
                directions.insert(id, coords);
            }
        } else if rest.starts_with("AXIS2_PLACEMENT_3D") {
            let refs = parse_hash_refs(rest);
            if refs.len() >= 3 {
                placement_refs.insert(id, (refs[0], refs[1], refs[2]));
            }
        } else if rest.starts_with("ITEM_DEFINED_TRANSFORMATION") {
            let refs = parse_hash_refs(rest);
            if refs.len() >= 2 {
                item_transforms.insert(id, (refs[0], refs[1]));
            }
        } else if rest.contains("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION") {
            let refs = parse_hash_refs(rest);
            if refs.len() >= 3 {
                rep_relationships.push((refs[0], refs[1], refs[2]));
            }
        } else if rest.starts_with("ADVANCED_BREP_SHAPE_REPRESENTATION") {
            let refs = parse_hash_refs(rest);
            absr_refs.insert(id, refs);
        } else if rest.starts_with("MANIFOLD_SOLID_BREP") {
            let refs = parse_hash_refs(rest);
            if !refs.is_empty() {
                manifold_to_shell.insert(id, refs[0]);
            }
        } else if rest.starts_with("SHAPE_REPRESENTATION_RELATIONSHIP")
            && !rest.contains("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION")
        {
            let refs = parse_hash_refs(rest);
            if refs.len() >= 2 {
                shape_rep_relationships.push((refs[0], refs[1]));
            }
        } else if rest.starts_with("SHAPE_REPRESENTATION")
            && !rest.starts_with("SHAPE_REPRESENTATION_RELATIONSHIP")
        {
            let refs = parse_hash_refs(rest);
            _shape_reps.insert(id, refs);
        }
    }

    // Resolve AXIS2_PLACEMENT_3D.
    let mut resolved_placements: HashMap<u64, Transform> = HashMap::new();
    for (&id, &(loc_id, axis_id, ref_id)) in &placement_refs {
        let location = cartesian_points
            .get(&loc_id)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
        let axis = directions.get(&axis_id).copied().unwrap_or([0.0, 0.0, 1.0]);
        let ref_dir = directions.get(&ref_id).copied().unwrap_or([1.0, 0.0, 0.0]);
        resolved_placements.insert(id, Transform::from_axis2_placement(location, axis, ref_dir));
    }

    log::info!("Assembly parsing:");
    log::info!("  {} cartesian points", cartesian_points.len());
    log::info!("  {} directions", directions.len());
    log::info!("  {} resolved placements", resolved_placements.len());
    log::info!("  {} item_defined_transforms", item_transforms.len());
    log::info!("  {} rep_relationships", rep_relationships.len());
    log::info!("  {} absr_refs entries", absr_refs.len());
    log::info!("  {} manifold_to_shell", manifold_to_shell.len());
    log::info!(
        "  {} shape_rep_relationships",
        shape_rep_relationships.len()
    );

    // ====== FOXTROT-STYLE TOP-DOWN TRANSFORM TRAVERSAL ======.

    // Step 1: Build transform stack (parent -> [(child, transform)]).
    // Try normal direction first.
    let (transform_stack, flipped) =
        build_transform_stack(&rep_relationships, &item_transforms, &resolved_placements);

    log::info!(
        "Transform stack: {} parents, flipped={}",
        transform_stack.len(),
        flipped
    );

    // Step 2: Build shape_rep_relationship map for traversal.
    let mut shape_rep_map: HashMap<u64, Vec<u64>> = HashMap::new();
    for &(r1, r2) in &shape_rep_relationships {
        shape_rep_map.entry(r1).or_default().push(r2);
        shape_rep_map.entry(r2).or_default().push(r1);
    }

    // Step 3: Find roots and traverse top-down.
    let roots = find_transform_roots(&transform_stack);
    log::info!("Transform roots: {}", roots.len());

    // BFS traversal from roots, accumulating transforms.
    let mut rep_transforms: HashMap<u64, Transform> = HashMap::new();
    let mut todo: Vec<(u64, Transform)> = roots
        .into_iter()
        .map(|r| (r, Transform::identity()))
        .collect();

    while let Some((rep_id, mat)) = todo.pop() {
        // Follow shape_rep_relationships (no transform change).
        if let Some(linked) = shape_rep_map.get(&rep_id) {
            for &child in linked {
                if !rep_transforms.contains_key(&child) {
                    todo.push((child, mat));
                }
            }
        }

        // Follow transform_stack (with transform).
        if let Some(children) = transform_stack.get(&rep_id) {
            for &(child, ref child_mat) in children {
                if !rep_transforms.contains_key(&child) {
                    let combined = mat.mul(child_mat);
                    todo.push((child, combined));
                }
            }
        }

        // Store this rep's transform if it's a leaf (ABSR or similar).
        rep_transforms.insert(rep_id, mat);
    }

    log::info!(
        "Computed transforms for {} representations",
        rep_transforms.len()
    );

    // Step 4: Map manifolds to their ABSR's transform.
    let mut manifold_to_absr: HashMap<u64, u64> = HashMap::new();
    for (&absr_id, refs) in &absr_refs {
        for &ref_id in refs {
            if manifold_to_shell.contains_key(&ref_id) {
                manifold_to_absr.insert(ref_id, absr_id);
            }
        }
    }

    // Step 5: Build shell -> world transform.
    let mut shell_transforms: HashMap<u64, Transform> = HashMap::new();
    for (&manifold_id, &shell_id) in &manifold_to_shell {
        if let Some(&absr_id) = manifold_to_absr.get(&manifold_id)
            && let Some(&transform) = rep_transforms.get(&absr_id)
        {
            if transform.cols[3][0].abs() > 0.001
                || transform.cols[3][1].abs() > 0.001
                || transform.cols[3][2].abs() > 0.001
            {
                log::info!(
                    "Shell #{} transform: translation=({:.2}, {:.2}, {:.2})",
                    shell_id,
                    transform.cols[3][0],
                    transform.cols[3][1],
                    transform.cols[3][2]
                );
            }
            shell_transforms.insert(shell_id, transform);
        }
    }

    // If no transforms found via hierarchy, shells get identity.
    if shell_transforms.is_empty() {
        log::info!("No transform hierarchy found, using identity for all shells");
        for &shell_id in manifold_to_shell.values() {
            shell_transforms.insert(shell_id, Transform::identity());
        }
    }

    shell_transforms
}

/// Build transform stack: parent_rep -> [(child_rep, transform)].
/// Returns (stack, was_flipped).
fn build_transform_stack(
    rep_relationships: &[(u64, u64, u64)],
    item_transforms: &HashMap<u64, (u64, u64)>,
    placements: &HashMap<u64, Transform>,
) -> (HashMap<u64, Vec<(u64, Transform)>>, bool) {
    // Try normal direction first (rep_1 is parent, rep_2 is child).
    let stack =
        build_transform_stack_directed(rep_relationships, item_transforms, placements, false);
    let roots = find_transform_roots(&stack);

    // If multiple roots, flip direction (like foxtrot does).
    if roots.len() > 1 {
        log::info!(
            "Multiple roots ({}), flipping transform direction",
            roots.len()
        );
        let flipped_stack =
            build_transform_stack_directed(rep_relationships, item_transforms, placements, true);
        let flipped_roots = find_transform_roots(&flipped_stack);
        if flipped_roots.len() < roots.len() {
            return (flipped_stack, true);
        }
    }

    (stack, false)
}

fn build_transform_stack_directed(
    rep_relationships: &[(u64, u64, u64)],
    item_transforms: &HashMap<u64, (u64, u64)>,
    placements: &HashMap<u64, Transform>,
    flip: bool,
) -> HashMap<u64, Vec<(u64, Transform)>> {
    let mut stack: HashMap<u64, Vec<(u64, Transform)>> = HashMap::new();

    for &(rep_1, rep_2, transform_id) in rep_relationships {
        let (parent, child) = if flip { (rep_1, rep_2) } else { (rep_2, rep_1) };

        // Compute the transform from ITEM_DEFINED_TRANSFORMATION.
        let mut mat = compute_item_transform(transform_id, item_transforms, placements);
        if flip {
            mat = mat.inverse();
        }

        stack.entry(parent).or_default().push((child, mat));
    }

    stack
}

/// Find roots: representations that are parents but never children.
fn find_transform_roots(stack: &HashMap<u64, Vec<(u64, Transform)>>) -> Vec<u64> {
    let children: HashSet<u64> = stack
        .values()
        .flat_map(|v| v.iter().map(|(c, _)| *c))
        .collect();

    stack
        .keys()
        .filter(|k| !children.contains(k))
        .copied()
        .collect()
}

/// Compute transform from ITEM_DEFINED_TRANSFORMATION.
/// Formula: t2 * inverse(t1) where t1=transform_item_1, t2=transform_item_2.
fn compute_item_transform(
    transform_id: u64,
    item_transforms: &HashMap<u64, (u64, u64)>,
    placements: &HashMap<u64, Transform>,
) -> Transform {
    let Some(&(place_1_id, place_2_id)) = item_transforms.get(&transform_id) else {
        return Transform::identity();
    };

    let t1 = placements.get(&place_1_id).copied().unwrap_or_default();
    let t2 = placements.get(&place_2_id).copied().unwrap_or_default();

    // t2 * inverse(t1): converts from local frame (t1) to target frame (t2).
    t2.mul(&t1.inverse())
}

/// Parse #id references from a STEP entity string.
pub(crate) fn parse_hash_refs(s: &str) -> Vec<u64> {
    let mut refs = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' {
            let mut num = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    num.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            if let Ok(id) = num.parse::<u64>() {
                refs.push(id);
            }
        }
    }
    refs
}

/// Parse coordinate values from CARTESIAN_POINT or DIRECTION.
/// Format varies: CARTESIAN_POINT('',(x,y,z)) or CARTESIAN_POINT ( '', ( x, y, z ) ).
fn parse_point_coords(s: &str) -> Option<[f64; 3]> {
    // Find the coordinate tuple - whitespace varies between files.
    // Look for the comma after the name string, then find the opening paren.
    let comma_pos = s.find(',')?;
    let after_comma = &s[comma_pos + 1..];
    let paren_pos = after_comma.find('(')?;
    let inner = &after_comma[paren_pos + 1..];
    let end = inner.find(')')?;
    let coords_str = &inner[..end];

    let parts: Vec<&str> = coords_str.split(',').collect();
    if parts.len() < 3 {
        return None;
    }

    let x = parse_step_float(parts[0])?;
    let y = parse_step_float(parts[1])?;
    let z = parse_step_float(parts[2])?;

    Some([x, y, z])
}

/// Parse a STEP float value (handles E notation like "0.E+000").
pub(crate) fn parse_step_float(s: &str) -> Option<f64> {
    let s = s.trim();
    // Handle STEP's weird notation like "0.E+000" (missing digit after decimal).
    let s = if s.contains(".E") {
        s.replace(".E", ".0E")
    } else {
        s.to_string()
    };
    s.parse().ok()
}

/// Parse colors from raw STEP file content.
/// Returns a map from shell entity ID to RGB color.
pub(crate) fn parse_step_colors(raw: &str) -> HashMap<u64, [f32; 3]> {
    let mut colours: HashMap<u64, [f32; 3]> = HashMap::new();
    // (style_refs, target_id).
    let mut styled_items: Vec<(Vec<u64>, u64)> = Vec::new();
    let mut fill_area_style_colour_to_colour: HashMap<u64, u64> = HashMap::new();
    let mut fill_area_style_to_fasc: HashMap<u64, u64> = HashMap::new();
    // SURFACE_STYLE_FILL_AREA -> FAS.
    let mut ssfa_to_fas: HashMap<u64, u64> = HashMap::new();
    // SURFACE_SIDE_STYLE -> SSFA.
    let mut sss_to_ssfa: HashMap<u64, Vec<u64>> = HashMap::new();
    // SURFACE_STYLE_USAGE -> SSS.
    let mut ssu_to_sss: HashMap<u64, u64> = HashMap::new();
    let mut psa_to_styles: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut manifold_to_shell: HashMap<u64, u64> = HashMap::new();

    let joined = preprocess_step_entities(raw);

    for entity in joined.split(';') {
        let entity = entity.trim();
        let Some(rest) = entity.strip_prefix('#') else {
            continue;
        };
        let Some((id_str, rest)) = rest.split_once('=') else {
            continue;
        };
        let Ok(id) = id_str.trim().parse::<u64>() else {
            continue;
        };
        let rest = rest.trim();

        if rest.starts_with("COLOUR_RGB") {
            // COLOUR_RGB('',r,g,b) - note: no parens around RGB values.
            if let Some(start) = rest.find('(') {
                let inner = &rest[start + 1..];
                if let Some(end) = inner.rfind(')') {
                    // Split by comma, skip the first element (the name string).
                    let params: Vec<&str> = inner[..end].split(',').collect();
                    if params.len() >= 4 {
                        // params[0] is the name (''), params[1..4] are r,g,b.
                        if let (Some(r), Some(g), Some(b)) = (
                            parse_step_float(params[1]),
                            parse_step_float(params[2]),
                            parse_step_float(params[3]),
                        ) {
                            colours.insert(id, [r as f32, g as f32, b as f32]);
                        }
                    }
                }
            }
        } else if rest.starts_with("STYLED_ITEM") || rest.starts_with("OVER_RIDING_STYLED_ITEM") {
            // STYLED_ITEM('name',(#style_refs),#target).
            let refs = parse_hash_refs(rest);
            if refs.len() >= 2 {
                let target_id = refs[refs.len() - 1];
                let style_refs: Vec<u64> = refs[..refs.len() - 1].to_vec();
                styled_items.push((style_refs, target_id));
            }
        } else if rest.starts_with("FILL_AREA_STYLE_COLOUR") {
            // FILL_AREA_STYLE_COLOUR('',#colour_ref).
            let refs = parse_hash_refs(rest);
            if !refs.is_empty() {
                fill_area_style_colour_to_colour.insert(id, refs[0]);
            }
        } else if rest.starts_with("FILL_AREA_STYLE") && !rest.starts_with("FILL_AREA_STYLE_COLOUR")
        {
            // FILL_AREA_STYLE('',(#fasc_ref)).
            let refs = parse_hash_refs(rest);
            if !refs.is_empty() {
                fill_area_style_to_fasc.insert(id, refs[0]);
            }
        } else if rest.starts_with("SURFACE_STYLE_FILL_AREA") {
            // SURFACE_STYLE_FILL_AREA(#fas_ref).
            let refs = parse_hash_refs(rest);
            if !refs.is_empty() {
                ssfa_to_fas.insert(id, refs[0]);
            }
        } else if rest.starts_with("SURFACE_SIDE_STYLE") {
            // SURFACE_SIDE_STYLE('',(#ssfa_refs)).
            let refs = parse_hash_refs(rest);
            sss_to_ssfa.insert(id, refs);
        } else if rest.starts_with("SURFACE_STYLE_USAGE") {
            // SURFACE_STYLE_USAGE(.BOTH.,#sss_ref).
            let refs = parse_hash_refs(rest);
            if !refs.is_empty() {
                ssu_to_sss.insert(id, refs[0]);
            }
        } else if rest.starts_with("PRESENTATION_STYLE_ASSIGNMENT")
            || rest.starts_with("PRESENTATION_STYLE_BY_CONTEXT")
        {
            // PRESENTATION_STYLE_ASSIGNMENT((#style_refs)).
            let refs = parse_hash_refs(rest);
            psa_to_styles.insert(id, refs);
        } else if rest.starts_with("MANIFOLD_SOLID_BREP") {
            // MANIFOLD_SOLID_BREP('',#shell).
            let refs = parse_hash_refs(rest);
            if !refs.is_empty() {
                manifold_to_shell.insert(id, refs[0]);
            }
        }
    }

    // Build color chain: follow references to find RGB values.
    // FASC -> COLOUR_RGB.
    let mut fasc_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&fasc_id, &colour_id) in &fill_area_style_colour_to_colour {
        if let Some(&rgb) = colours.get(&colour_id) {
            fasc_colors.insert(fasc_id, rgb);
        }
    }

    // FAS -> FASC -> COLOUR_RGB.
    let mut fas_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&fas_id, &fasc_id) in &fill_area_style_to_fasc {
        if let Some(&rgb) = fasc_colors.get(&fasc_id) {
            fas_colors.insert(fas_id, rgb);
        }
    }

    // SSFA -> FAS -> ... -> COLOUR_RGB.
    let mut ssfa_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&ssfa_id, &fas_id) in &ssfa_to_fas {
        if let Some(&rgb) = fas_colors.get(&fas_id) {
            ssfa_colors.insert(ssfa_id, rgb);
        }
    }

    // SSS -> SSFA -> ... -> COLOUR_RGB.
    let mut sss_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&sss_id, ssfa_refs) in &sss_to_ssfa {
        for &ssfa_id in ssfa_refs {
            if let Some(&rgb) = ssfa_colors.get(&ssfa_id) {
                sss_colors.insert(sss_id, rgb);
                break;
            }
        }
    }

    // SSU -> SSS -> ... -> COLOUR_RGB.
    let mut ssu_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&ssu_id, &sss_id) in &ssu_to_sss {
        if let Some(&rgb) = sss_colors.get(&sss_id) {
            ssu_colors.insert(ssu_id, rgb);
        }
    }

    // PSA -> SSU -> ... -> COLOUR_RGB.
    let mut psa_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (&psa_id, style_refs) in &psa_to_styles {
        for &style_id in style_refs {
            if let Some(&rgb) = ssu_colors.get(&style_id) {
                psa_colors.insert(psa_id, rgb);
                break;
            }
        }
    }

    // STYLED_ITEM targets -> shell colors.
    let mut shell_colors: HashMap<u64, [f32; 3]> = HashMap::new();
    for (style_refs, target_id) in &styled_items {
        // Find color through style refs.
        let mut found_color: Option<[f32; 3]> = None;
        for &style_id in style_refs {
            if let Some(&rgb) = psa_colors.get(&style_id) {
                found_color = Some(rgb);
                break;
            }
        }

        if let Some(rgb) = found_color {
            // Target might be a manifold_brep, map to shell.
            if let Some(&shell_id) = manifold_to_shell.get(target_id) {
                shell_colors.insert(shell_id, rgb);
            } else {
                // Target might be the shell directly.
                shell_colors.insert(*target_id, rgb);
            }
        }
    }

    log::info!("Color parsing:");
    log::info!("  {} COLOUR_RGB entries", colours.len());
    log::info!("  {} STYLED_ITEM entries", styled_items.len());
    log::info!("  {} shell colors found", shell_colors.len());
    for (&id, &rgb) in &shell_colors {
        log::info!(
            "  Shell #{}: RGB({:.2}, {:.2}, {:.2})",
            id,
            rgb[0],
            rgb[1],
            rgb[2]
        );
    }

    shell_colors
}
