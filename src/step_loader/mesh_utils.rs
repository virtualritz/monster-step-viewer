use std::collections::HashMap;

use monstertruck::meshing::prelude::*;
use rayon::prelude::*;

use super::transform::Transform;

/// Apply a transform to all vertices and normals in a mesh.
pub(crate) fn apply_transform_to_mesh(mesh: &mut PolygonMesh, transform: &Transform) {
    use monstertruck::meshing::prelude::{Faces, Point3, StandardAttributes, Vector3};

    // Transform positions.
    let positions: Vec<_> = mesh
        .positions()
        .par_iter()
        .map(|p| {
            let transformed = transform.transform_point([p.x, p.y, p.z]);
            Point3::new(transformed[0], transformed[1], transformed[2])
        })
        .collect();

    // Transform normals (rotation only).
    let normals: Vec<_> = mesh
        .normals()
        .par_iter()
        .map(|n| {
            let transformed = transform.transform_normal([n.x, n.y, n.z]);
            // Normalize the result.
            let len = (transformed[0] * transformed[0]
                + transformed[1] * transformed[1]
                + transformed[2] * transformed[2])
                .sqrt();
            if len > 1e-10 {
                Vector3::new(
                    transformed[0] / len,
                    transformed[1] / len,
                    transformed[2] / len,
                )
            } else {
                Vector3::new(0.0, 0.0, 1.0)
            }
        })
        .collect();

    // Create new mesh with transformed data.
    let uv_coords: Vec<_> = mesh.uv_coords().to_vec();
    let tri_faces: Vec<_> = mesh.tri_faces().to_vec();

    *mesh = PolygonMesh::new(
        StandardAttributes {
            positions,
            uv_coords,
            normals,
        },
        Faces::from_tri_and_quad_faces(tri_faces, vec![]),
    );
}

/// Extract boundary edges from a tessellated polygon mesh.
/// Returns edges as pairs of 3D points. Boundary edges appear in only one triangle.
pub(crate) fn extract_mesh_edges(
    mesh: &PolygonMesh,
    transform: Option<&Transform>,
) -> Vec<([f64; 3], [f64; 3])> {
    let positions = mesh.positions();
    let tri_faces = mesh.tri_faces();

    // Count how many times each edge appears (using sorted vertex indices as key).
    let mut edge_counts: HashMap<(usize, usize), Vec<(usize, usize)>> = HashMap::new();

    for tri in tri_faces {
        let indices = [tri[0].pos, tri[1].pos, tri[2].pos];
        // Three edges per triangle.
        for i in 0..3 {
            let a = indices[i];
            let b = indices[(i + 1) % 3];
            let key = if a < b { (a, b) } else { (b, a) };
            edge_counts.entry(key).or_default().push((a, b));
        }
    }

    // Boundary edges appear exactly once.
    let boundary: Vec<_> = edge_counts
        .into_iter()
        .filter(|(_, occurrences)| occurrences.len() == 1)
        .map(|(key, _)| key)
        .collect();

    boundary
        .into_par_iter()
        .map(|(a, b)| {
            let pa = positions[a];
            let pb = positions[b];
            let mut coord_a = [pa.x, pa.y, pa.z];
            let mut coord_b = [pb.x, pb.y, pb.z];
            if let Some(xform) = transform {
                coord_a = xform.transform_point(coord_a);
                coord_b = xform.transform_point(coord_b);
            }
            (coord_a, coord_b)
        })
        .collect()
}
