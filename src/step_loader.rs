mod mesh_utils;
mod parsing;
pub mod transform;

use anyhow::Context;
use monstertruck::meshing::prelude::*;
use monstertruck::step::r#in::Table;
use parking_lot::Mutex;
use rayon::prelude::*;
pub use ruststep::ast::Parameter;
use ruststep::parser::parse;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

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

/// A single STEP face (surface) with its tessellated mesh.
#[derive(Clone, Debug)]
pub struct StepFace {
    pub id: usize,
    pub name: String,
    pub mesh: PolygonMesh,
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
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read STEP file {}", path.display()))?;

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
            let compressed = table
                .to_compressed_shell(shell_holder)
                .map_err(|e| anyhow::anyhow!("Failed to convert STEP shell into topology: {e}"))?;

            // Compute tolerance from geometric extents without a coarse triangulation pass.
            let mut bounds: BoundingBox<Point3> = compressed.vertices.iter().collect();
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
                    bounds.push(face.surface.subs((u0 + u1) * 0.5, (v0 + v1) * 0.5));
                }
            }
            let diameter = bounds.diameter();
            let mut tol = f64::max(diameter * 0.001, TOLERANCE);
            if !tol.is_finite() {
                tol = 0.01;
            }

            let poly_shell = compressed.robust_triangulation(tol);

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

                        StepFace {
                            id: face_idx,
                            name: format!("Face {}", face_idx + 1),
                            mesh,
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
                // Non-streaming loader doesn't parse colors.
                color: None,
                // Non-streaming loader doesn't parse assembly transforms.
                transform: None,
                edges: all_edges,
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

/// Start loading a STEP file in a background thread, streaming results via channel.
/// `tolerance_factor` controls tessellation density (smaller = more triangles, default 0.005).
pub fn load_step_file_streaming(path: PathBuf, tolerance_factor: f64) -> Receiver<LoadMessage> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        if let Err(e) = load_step_streaming_inner(&path, &tx, tolerance_factor) {
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
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read STEP file {}", path.display()))?;

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

    // Process shells in parallel, sending each as it completes (true streaming).
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    shell_entries
        .into_par_iter()
        .enumerate()
        .for_each(|(local_idx, (shell_id, shell_holder))| {
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
                    *error.lock() =
                        Some(format!("Failed to convert STEP shell into topology: {e}"));
                    return;
                }
            };

            // Compute tolerance from geometric extents without a coarse triangulation pass.
            let mut bounds: BoundingBox<Point3> = compressed.vertices.iter().collect();
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
                    bounds.push(face.surface.subs((u0 + u1) * 0.5, (v0 + v1) * 0.5));
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

            let poly_shell = compressed.robust_triangulation(tol);

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

                        // Extract boundary edges from this face's mesh (before transform is applied to mesh).
                        // Pass transform to extract_mesh_edges so edges are in world coords.
                        let face_edges = extract_mesh_edges(&mesh, transform.as_ref());
                        all_edges.extend(face_edges);

                        // Apply assembly transform to mesh vertices and normals.
                        if let Some(xform) = transform {
                            apply_transform_to_mesh(&mut mesh, &xform);
                        }

                        StepFace {
                            id: face_idx,
                            name: format!("Face {}", face_idx + 1),
                            mesh,
                        }
                    })
                })
                .collect();

            // Count total triangles for debugging.
            let total_tris: usize = faces.iter().map(|f| f.mesh.tri_faces().len()).sum();
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
            };

            // Send shell immediately (true streaming).
            let _ = tx.send(LoadMessage::Shell(shell));

            // Update and report progress.
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = tx.send(LoadMessage::Progress(done, total));
        });

    // Check for errors.
    if let Some(err) = error.lock().take() {
        return Err(anyhow::anyhow!(err));
    }

    tx.send(LoadMessage::Done)?;
    Ok(())
}
