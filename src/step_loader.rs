mod mesh_utils;
mod parsing;
pub mod transform;

use anyhow::Context;
use monstertruck::{
    meshing::prelude::*,
    step::load::{
        Table,
        step_geometry::{Curve3D, Surface},
    },
    topology::compress::CompressedShell,
};
type OriginalShell = CompressedShell<Point3, Curve3D, Surface>;
pub use monstertruck::step::load::ruststep::ast::Parameter;
use monstertruck::step::load::ruststep::parser::parse;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
};

pub use transform::Transform;

use mesh_utils::{apply_transform_to_mesh, extract_mesh_edges};
use parsing::{parse_assembly_transforms, parse_step_colors};

/// A named header entry from the STEP file.
#[derive(Clone, Debug)]
pub struct HeaderEntry {
    pub name: String,
    pub parameter: Parameter,
}

/// Metadata pulled from a STEP file header.
#[derive(Clone, Debug, Default)]
pub struct StepMetadata {
    pub headers: Vec<HeaderEntry>,
    pub entity_count: usize,
}

/// A single tessellated edge curve from the STEP model.
#[derive(Clone, Debug)]
pub struct StepEdge {
    pub id: usize,
    pub curve_type: String,
    pub points: Vec<[f64; 3]>,
}

/// A boundary loop of a face (outer boundary or hole).
#[derive(Clone, Debug)]
pub struct StepBoundaryLoop {
    pub edge_indices: Vec<usize>,
    pub is_outer: bool,
}

/// Wraps an original CompressedShell for potential re-tessellation.
#[derive(Clone)]
pub struct CompressedShellData {
    inner: Arc<dyn std::any::Any + Send + Sync>,
}

impl CompressedShellData {
    pub fn new<T: std::any::Any + Send + Sync + 'static>(data: T) -> Self {
        Self {
            inner: Arc::new(data),
        }
    }

    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        self.inner.downcast_ref()
    }
}

impl std::fmt::Debug for CompressedShellData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompressedShellData").finish()
    }
}

/// A single STEP face (surface) with its tessellated mesh.
#[derive(Clone, Debug)]
pub struct StepFace {
    pub id: usize,
    pub name: String,
    pub mesh: PolygonMesh,
    pub boundary_loops: Vec<StepBoundaryLoop>,
}

/// A STEP shell containing multiple faces.
#[derive(Clone, Debug)]
pub struct StepShell {
    pub id: usize,
    pub name: String,
    pub faces: Vec<StepFace>,
    /// RGB color from STEP file (if any).
    pub color: Option<[f32; 3]>,
    /// Assembly transform (world transform for this shell).
    pub transform: Option<Transform>,
    /// Tessellated boundary edges (each edge is a pair of 3D points).
    pub edges: Vec<([f64; 3], [f64; 3])>,
    /// Tessellated STEP curve edges (polylines from curve tessellation).
    pub curve_edges: Vec<StepEdge>,
    /// Original compressed shell for potential re-tessellation.
    pub original_shell: Option<CompressedShellData>,
    /// Tessellation tolerance used for this shell.
    pub tessellation_tolerance: f64,
}

/// Full scene extracted from a STEP file.
#[derive(Clone, Debug)]
pub struct StepScene {
    pub metadata: StepMetadata,
    pub shells: Vec<StepShell>,
}

/// Progress state for loading - stores (current, total) as packed u32s.
#[derive(Clone, Debug, Default)]
pub struct LoadProgress {
    /// Packed as (current << 16) | total.
    packed: Arc<AtomicU32>,
}

impl LoadProgress {
    pub fn new() -> Self {
        Self {
            packed: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn set(&self, current: u16, total: u16) {
        let packed = ((current as u32) << 16) | (total as u32);
        self.packed.store(packed, Ordering::Relaxed);
    }

    pub fn get(&self) -> (u16, u16) {
        let packed = self.packed.load(Ordering::Relaxed);
        ((packed >> 16) as u16, (packed & 0xFFFF) as u16)
    }

    pub fn fraction(&self) -> f32 {
        let (current, total) = self.get();
        if total == 0 {
            0.0
        } else {
            current as f32 / total as f32
        }
    }
}

/// Load and tessellate a STEP file into polygon meshes with progress reporting.
pub fn load_step_file_with_progress(
    path: &Path,
    progress: &LoadProgress,
) -> anyhow::Result<StepScene> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!("Failed to read STEP file {}", path.display())
    })?;

    let exchange = parse(&raw).context("Failed to parse STEP file")?;
    let table = Table::from_data_section(
        exchange
            .data
            .first()
            .context("STEP file has no data sections")?,
    );

    // Extract metadata.
    let metadata = StepMetadata {
        headers: exchange
            .header
            .iter()
            .map(|r| HeaderEntry {
                name: r.name.clone(),
                parameter: r.parameter.clone(),
            })
            .collect(),
        entity_count: exchange
            .data
            .iter()
            .map(|section| section.entities.len())
            .sum(),
    };

    // Convert each shell into a triangulated mesh (in parallel).
    let mut shell_entries: Vec<_> = table.shell.iter().collect();
    shell_entries.sort_by_key(|(id, _)| *id);

    let total = shell_entries.len();
    progress.set(0, total as u16);
    let completed = AtomicUsize::new(0);

    let shells: Result<Vec<StepShell>, anyhow::Error> = shell_entries
        .into_par_iter()
        .enumerate()
        .map(|(local_idx, (_id, shell_holder))| {
            let compressed =
                table.to_compressed_shell(shell_holder).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to convert STEP shell into topology: {e}"
                    )
                })?;

            // Classify curve types from original geometry (before
            // tessellation).
            let curve_types: Vec<String> = compressed
                .edges
                .iter()
                .map(|e| classify_curve_type(&e.curve))
                .collect();

            // Compute tolerance from geometric extents without a coarse
            // triangulation pass.
            let mut bounds: BoundingBox<Point3> =
                compressed.vertices.iter().collect();
            for edge in &compressed.edges {
                let (start, end) = edge.curve.range_tuple();
                // Sample a few points per edge to capture curved extents.
                for idx in 0..=4 {
                    let t = start + (end - start) * idx as f64 / 4.0;
                    bounds.push(edge.curve.subs(t));
                }
            }
            for face in &compressed.faces {
                let (urange, vrange) = face.surface.try_range_tuple();
                if let (Some((u0, u1)), Some((v0, v1))) = (urange, vrange) {
                    bounds.push(face.surface.subs(u0, v0));
                    bounds.push(face.surface.subs(u1, v0));
                    bounds.push(face.surface.subs(u0, v1));
                    bounds.push(face.surface.subs(u1, v1));
                    bounds.push(
                        face.surface.subs((u0 + u1) * 0.5, (v0 + v1) * 0.5),
                    );
                }
            }
            let diameter = bounds.diameter();
            let mut tol = f64::max(diameter * 0.001, TOLERANCE);
            if !tol.is_finite() {
                tol = 0.01;
            }

            let original_shell = CompressedShellData::new(compressed.clone());
            let poly_shell = compressed.robust_triangulation(tol);

            // Extract tessellated curve edges.
            let curve_edges: Vec<StepEdge> = poly_shell
                .edges
                .iter()
                .enumerate()
                .map(|(i, edge)| {
                    let points =
                        edge.curve.iter().map(|p| [p.x, p.y, p.z]).collect();
                    StepEdge {
                        id: i,
                        curve_type: curve_types
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| "Unknown".to_string()),
                        points,
                    }
                })
                .collect();

            // Extract individual faces and boundary edges from each face mesh.
            let mut all_edges: Vec<([f64; 3], [f64; 3])> = Vec::new();
            let faces: Vec<StepFace> = poly_shell
                .faces
                .iter()
                .enumerate()
                .filter_map(|(face_idx, face)| {
                    face.surface.as_ref().map(|surface| {
                        let mesh = match face.orientation {
                            true => surface.clone(),
                            false => surface.inverse(),
                        };
                        // Extract boundary edges from this face's mesh.
                        let face_edges = extract_mesh_edges(&mesh, None);
                        all_edges.extend(face_edges);

                        // Extract boundary loop topology.
                        let boundary_loops: Vec<StepBoundaryLoop> = face
                            .boundaries
                            .iter()
                            .enumerate()
                            .map(|(loop_idx, loop_edges)| StepBoundaryLoop {
                                edge_indices: loop_edges
                                    .iter()
                                    .map(|ei| ei.index)
                                    .collect(),
                                is_outer: loop_idx == 0,
                            })
                            .collect();

                        StepFace {
                            id: face_idx,
                            name: format!("Face {}", face_idx + 1),
                            mesh,
                            boundary_loops,
                        }
                    })
                })
                .collect();

            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            progress.set(done as u16, total as u16);

            Ok(StepShell {
                id: local_idx,
                name: format!("Shell {}", local_idx + 1),
                faces,
                color: None,
                transform: None,
                edges: all_edges,
                curve_edges,
                original_shell: Some(original_shell),
                tessellation_tolerance: tol,
            })
        })
        .collect();

    let mut shells = shells?;
    // Sort by id to maintain consistent ordering after parallel processing.
    shells.sort_by_key(|s| s.id);

    if shells.is_empty() {
        anyhow::bail!("No shells found in STEP file");
    }

    Ok(StepScene { metadata, shells })
}

/// Load and tessellate a STEP file into polygon meshes.
pub fn load_step_file(path: &Path) -> anyhow::Result<StepScene> {
    load_step_file_with_progress(path, &LoadProgress::new())
}

/// Message sent from background loader to main thread.
#[allow(clippy::large_enum_variant)]
pub enum LoadMessage {
    /// Metadata parsed from STEP header.
    Metadata(StepMetadata),
    /// Total number of shells to process.
    TotalShells(usize),
    /// Progress update during tessellation (completed, total).
    Progress(usize, usize),
    /// A completed shell.
    Shell(StepShell),
    /// Loading finished successfully.
    Done,
    /// An error occurred.
    Error(String),
}

/// Start loading a STEP file in a background thread, streaming results via
/// channel. `tolerance_factor` controls tessellation density (smaller = more
/// triangles, default 0.005).
pub fn load_step_file_streaming(
    path: PathBuf,
    tolerance_factor: f64,
) -> Receiver<LoadMessage> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        if let Err(e) = load_step_streaming_inner(&path, &tx, tolerance_factor)
        {
            let _ = tx.send(LoadMessage::Error(e.to_string()));
        }
    });

    rx
}

fn load_step_streaming_inner(
    path: &Path,
    tx: &Sender<LoadMessage>,
    tolerance_factor: f64,
) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!("Failed to read STEP file {}", path.display())
    })?;

    // Parse colors from raw STEP content.
    let entity_colors = parse_step_colors(&raw);
    log::info!(
        "Parsed {} entity colors from STEP file",
        entity_colors.len()
    );
    for (id, rgb) in &entity_colors {
        log::info!(
            "  Entity #{}: RGB({:.2}, {:.2}, {:.2})",
            id,
            rgb[0],
            rgb[1],
            rgb[2]
        );
    }

    // Parse assembly transforms.
    let assembly_transforms = parse_assembly_transforms(&raw);
    log::info!(
        "Parsed {} assembly transforms from STEP file",
        assembly_transforms.len()
    );

    let exchange = parse(&raw).context("Failed to parse STEP file")?;
    let table = Table::from_data_section(
        exchange
            .data
            .first()
            .context("STEP file has no data sections")?,
    );

    // Extract and send metadata.
    let metadata = StepMetadata {
        headers: exchange
            .header
            .iter()
            .map(|r| HeaderEntry {
                name: r.name.clone(),
                parameter: r.parameter.clone(),
            })
            .collect(),
        entity_count: exchange
            .data
            .iter()
            .map(|section| section.entities.len())
            .sum(),
    };
    tx.send(LoadMessage::Metadata(metadata))?;

    // Convert each shell into a triangulated mesh (in parallel).
    let mut shell_entries: Vec<_> = table.shell.iter().collect();
    shell_entries.sort_by_key(|(id, _)| *id);

    let total = shell_entries.len();
    tx.send(LoadMessage::TotalShells(total))?;

    // Track progress with atomic counter.
    let completed = Arc::new(AtomicUsize::new(0));

    // Process shells in parallel, sending each as it completes (true
    // streaming).
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    shell_entries.into_par_iter().enumerate().for_each(
        |(local_idx, (shell_id, shell_holder))| {
            // Skip if we already encountered an error.
            if error.lock().is_some() {
                return;
            }

            // Look up color for this shell's entity ID.
            let color = entity_colors.get(shell_id).copied();
            // Look up assembly transform for this shell.
            let transform = assembly_transforms.get(shell_id).copied();
            log::info!(
                "Shell {} (entity #{}): color={:?}, transform={:?}",
                local_idx,
                shell_id,
                color,
                transform.map(|t| [t.cols[3][0], t.cols[3][1], t.cols[3][2]])
            );

            let compressed = match table.to_compressed_shell(shell_holder) {
                Ok(c) => c,
                Err(e) => {
                    *error.lock() = Some(format!(
                        "Failed to convert STEP shell into topology: {e}"
                    ));
                    return;
                }
            };

            // Classify curve types from original geometry (before
            // tessellation).
            let curve_types: Vec<String> = compressed
                .edges
                .iter()
                .map(|e| classify_curve_type(&e.curve))
                .collect();

            // Compute tolerance from geometric extents without a coarse
            // triangulation pass.
            let mut bounds: BoundingBox<Point3> =
                compressed.vertices.iter().collect();
            for edge in &compressed.edges {
                let (start, end) = edge.curve.range_tuple();
                // Sample a few points per edge to capture curved extents.
                for idx in 0..=4 {
                    let t = start + (end - start) * idx as f64 / 4.0;
                    bounds.push(edge.curve.subs(t));
                }
            }
            for face in &compressed.faces {
                let (urange, vrange) = face.surface.try_range_tuple();
                if let (Some((u0, u1)), Some((v0, v1))) = (urange, vrange) {
                    bounds.push(face.surface.subs(u0, v0));
                    bounds.push(face.surface.subs(u1, v0));
                    bounds.push(face.surface.subs(u0, v1));
                    bounds.push(face.surface.subs(u1, v1));
                    bounds.push(
                        face.surface.subs((u0 + u1) * 0.5, (v0 + v1) * 0.5),
                    );
                }
            }
            let bbox_diameter = bounds.diameter();
            let mut tol = f64::max(bbox_diameter * tolerance_factor, TOLERANCE);
            if !tol.is_finite() {
                tol = 0.01;
            }
            log::info!(
                "Tessellation: bbox_diameter={:.4}, factor={:.6}, tol={:.6}",
                bbox_diameter,
                tolerance_factor,
                tol
            );

            let original_shell = CompressedShellData::new(compressed.clone());
            let poly_shell = compressed.robust_triangulation(tol);

            // Extract tessellated curve edges (with transform applied).
            let curve_edges: Vec<StepEdge> = poly_shell
                .edges
                .iter()
                .enumerate()
                .map(|(i, edge)| {
                    let points = edge
                        .curve
                        .iter()
                        .map(|p| {
                            let mut coord = [p.x, p.y, p.z];
                            if let Some(xform) = transform.as_ref() {
                                coord = xform.transform_point(coord);
                            }
                            coord
                        })
                        .collect();
                    StepEdge {
                        id: i,
                        curve_type: curve_types
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| "Unknown".to_string()),
                        points,
                    }
                })
                .collect();

            // Extract individual faces and apply transform to mesh vertices.
            // Also extract boundary edges from each face mesh.
            let mut all_edges: Vec<([f64; 3], [f64; 3])> = Vec::new();
            let faces: Vec<StepFace> = poly_shell
                .faces
                .iter()
                .enumerate()
                .filter_map(|(face_idx, face)| {
                    face.surface.as_ref().map(|surface| {
                        let mut mesh = match face.orientation {
                            true => surface.clone(),
                            false => surface.inverse(),
                        };

                        // Extract boundary edges from this face's mesh (before
                        // transform is applied to mesh).
                        // Pass transform to extract_mesh_edges so edges are in
                        // world coords.
                        let face_edges =
                            extract_mesh_edges(&mesh, transform.as_ref());
                        all_edges.extend(face_edges);

                        // Apply assembly transform to mesh vertices and
                        // normals.
                        if let Some(xform) = transform {
                            apply_transform_to_mesh(&mut mesh, &xform);
                        }

                        // Extract boundary loop topology.
                        let boundary_loops: Vec<StepBoundaryLoop> = face
                            .boundaries
                            .iter()
                            .enumerate()
                            .map(|(loop_idx, loop_edges)| StepBoundaryLoop {
                                edge_indices: loop_edges
                                    .iter()
                                    .map(|ei| ei.index)
                                    .collect(),
                                is_outer: loop_idx == 0,
                            })
                            .collect();

                        StepFace {
                            id: face_idx,
                            name: format!("Face {}", face_idx + 1),
                            mesh,
                            boundary_loops,
                        }
                    })
                })
                .collect();

            // Count total triangles for debugging.
            let total_tris: usize =
                faces.iter().map(|f| f.mesh.tri_faces().len()).sum();
            log::info!(
                "Shell {}: {} faces, {} triangles (tol={:.6})",
                local_idx,
                faces.len(),
                total_tris,
                tol
            );

            let shell = StepShell {
                id: local_idx,
                name: format!("Shell {}", local_idx + 1),
                faces,
                color,
                transform,
                edges: all_edges,
                curve_edges,
                original_shell: Some(original_shell),
                tessellation_tolerance: tol,
            };

            // Send shell immediately (true streaming).
            let _ = tx.send(LoadMessage::Shell(shell));

            // Update and report progress.
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = tx.send(LoadMessage::Progress(done, total));
        },
    );

    // Check for errors.
    if let Some(err) = error.lock().take() {
        return Err(anyhow::anyhow!(err));
    }

    tx.send(LoadMessage::Done)?;
    Ok(())
}

/// Re-tessellate a single face with modified boundaries.
/// `active_boundaries` contains the loop indices (into the original face's
/// boundaries) that should remain active. If empty, the face is tessellated
/// with no trim boundaries (full surface domain).
pub fn retessellate_face(
    shell_data: &CompressedShellData,
    face_idx: usize,
    active_boundary_indices: &[usize],
    tolerance: f64,
    transform: Option<&crate::step_loader::Transform>,
) -> Option<PolygonMesh> {
    let original: &OriginalShell = shell_data.downcast_ref()?;

    // Clone the shell and modify the target face's boundaries.
    let mut modified = original.clone();
    if let Some(face) = modified.faces.get_mut(face_idx) {
        let original_boundaries = face.boundaries.clone();
        face.boundaries = active_boundary_indices
            .iter()
            .filter_map(|&idx| original_boundaries.get(idx).cloned())
            .collect();
    } else {
        return None;
    }

    // Re-tessellate the entire shell (necessary because edges are shared).
    let poly_shell = modified.robust_triangulation(tolerance);

    // Extract the target face's mesh.
    let poly_face = poly_shell.faces.get(face_idx)?;
    let surface = poly_face.surface.as_ref()?;
    let mut mesh = if poly_face.orientation {
        surface.clone()
    } else {
        surface.inverse()
    };

    // Apply transform if present.
    if let Some(xform) = transform {
        apply_transform_to_mesh(&mut mesh, xform);
    }

    Some(mesh)
}

fn classify_curve_type(curve: &Curve3D) -> String {
    match curve {
        Curve3D::Line(_) => "Line",
        Curve3D::Polyline(_) => "Polyline",
        Curve3D::Conic(_) => "Conic",
        Curve3D::BsplineCurve(_) => "BSpline",
        Curve3D::Pcurve(_) => "Pcurve",
        Curve3D::NurbsCurve(_) => "NURBS",
    }
    .to_string()
}
