mod mesh_utils;
mod parsing;
pub mod transform;

use anyhow::Context;
use monstertruck::{
    meshing::prelude::*,
    step::load::{
        Table,
        ruststep::{ast::Name, parser::parse, tables::PlaceHolder},
        step_geometry::{Curve3D, Pcurve, Surface},
    },
    topology::compress::{CompressedShell, CompressedTrimmedShell},
};
type OriginalShell = CompressedTrimmedShell<Point3, Curve3D, Surface, Pcurve>;

fn shell_requires_trimmed_meshing<P, C, S, T>(
    shell: &CompressedTrimmedShell<P, C, S, T>,
) -> bool {
    shell
        .faces
        .iter()
        .any(|face| face.boundaries.iter().any(|boundary| !boundary.is_empty()))
}

fn count_failed_face_meshes<P, C>(
    shell: &CompressedShell<P, C, Option<PolygonMesh>>,
) -> usize {
    shell
        .faces
        .iter()
        .filter(|face| face.surface.is_none())
        .count()
}

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

pub use monstertruck::step::load::ruststep::ast::Parameter;
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

/// Whether the original STEP entity was a solid or a shell.
#[derive(Clone, Debug)]
pub enum StepTopology {
    /// From `manifold_solid_brep` — watertight, suitable for boolean ops.
    /// Wraps a `CompressedTrimmedSolid<Point3, Curve3D, Surface, Pcurve>`.
    Solid(CompressedShellData),
    /// From `shell_based_surface_model` or standalone shell — open surface.
    /// Wraps a `CompressedTrimmedShell<Point3, Curve3D, Surface, Pcurve>`.
    Shell(CompressedShellData),
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
    /// Original topology (solid or shell) for boolean operations.
    pub topology: Option<StepTopology>,
    /// Tessellation tolerance used for this shell.
    pub tessellation_tolerance: f64,
    /// Number of faces the mesher could not triangulate.
    pub failed_faces: usize,
}

/// Full scene extracted from a STEP file.
#[derive(Clone, Debug)]
pub struct StepScene {
    pub metadata: StepMetadata,
    pub shells: Vec<StepShell>,
}

/// Coarse phase of the background STEP loader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadPhase {
    /// Reading bytes from disk.
    Reading,
    /// Parsing STEP text into entities and assembly metadata.
    Parsing,
    /// Converting STEP topology and computing scene bounds.
    Preparing,
    /// Tessellating shell meshes.
    Meshing,
}

impl LoadPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reading => "Reading STEP file",
            Self::Parsing => "Parsing STEP file",
            Self::Preparing => "Preparing topology",
            Self::Meshing => "Tessellating shells",
        }
    }
}

/// Axis-aligned bounds in original STEP model coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepBounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
    pub center: [f64; 3],
}

impl StepBounds {
    pub fn from_points<I>(points: I) -> Option<Self>
    where
        I: IntoIterator<Item = [f64; 3]>,
    {
        let mut builder = StepBoundsBuilder::default();
        points.into_iter().for_each(|point| builder.push(point));
        builder.finish()
    }

    pub fn union(self, other: Self) -> Self {
        Self::from_min_max(
            [
                self.min[0].min(other.min[0]),
                self.min[1].min(other.min[1]),
                self.min[2].min(other.min[2]),
            ],
            [
                self.max[0].max(other.max[0]),
                self.max[1].max(other.max[1]),
                self.max[2].max(other.max[2]),
            ],
        )
    }

    pub fn diameter(self) -> f64 {
        let dx = self.max[0] - self.min[0];
        let dy = self.max[1] - self.min[1];
        let dz = self.max[2] - self.min[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    pub fn normalization_scale(self) -> f64 {
        let dx = self.max[0] - self.min[0];
        let dy = self.max[1] - self.min[1];
        let dz = self.max[2] - self.min[2];
        let max_dim = dx.max(dy).max(dz);
        if max_dim > 0.0 { 1.0 / max_dim } else { 1.0 }
    }

    fn from_min_max(min: [f64; 3], max: [f64; 3]) -> Self {
        Self {
            min,
            max,
            center: [
                (min[0] + max[0]) * 0.5,
                (min[1] + max[1]) * 0.5,
                (min[2] + max[2]) * 0.5,
            ],
        }
    }
}

fn bounds_from_step_shells(shells: &[StepShell]) -> Option<StepBounds> {
    StepBounds::from_points(shells.iter().flat_map(|shell| {
        shell.faces.iter().flat_map(|face| {
            face.mesh
                .positions()
                .iter()
                .map(|point| [point.x, point.y, point.z])
        })
    }))
}

#[derive(Debug)]
struct StepBoundsBuilder {
    min: [f64; 3],
    max: [f64; 3],
    has_points: bool,
}

impl Default for StepBoundsBuilder {
    fn default() -> Self {
        Self {
            min: [f64::MAX; 3],
            max: [f64::MIN; 3],
            has_points: false,
        }
    }
}

impl StepBoundsBuilder {
    fn push(&mut self, point: [f64; 3]) {
        if point.iter().all(|coord| coord.is_finite()) {
            self.has_points = true;
            for (axis, coord) in point.iter().enumerate() {
                self.min[axis] = self.min[axis].min(*coord);
                self.max[axis] = self.max[axis].max(*coord);
            }
        }
    }

    fn finish(self) -> Option<StepBounds> {
        self.has_points
            .then(|| StepBounds::from_min_max(self.min, self.max))
    }
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

/// Resolve an entity ID that might be an oriented_shell to the underlying
/// shell entity ID.  If `idx` is in `table.oriented_shell` and that entry's
/// `shell_element` is a `PlaceHolder::Ref(Name::Entity(shell_idx))`, return
/// `shell_idx`.  Otherwise return the original `idx` (it may already be a
/// direct shell reference).
fn resolve_to_shell_id(table: &Table, idx: u64) -> u64 {
    if let Some(oriented) = table.oriented_shell.get(&idx)
        && let PlaceHolder::Ref(Name::Entity(shell_idx)) =
            &oriented.shell_element
    {
        *shell_idx
    } else {
        idx
    }
}

/// Build a mapping from shell entity IDs (in `table.shell`) to their parent
/// solid entity IDs.
///
/// A solid can reference shells via `outer` (always) and `voids` (optional).
/// References may go through an `oriented_shell` indirection, so we resolve
/// those to the underlying shell ID.
fn build_shell_to_solid_map(
    table: &Table,
) -> std::collections::HashMap<u64, u64> {
    let mut shell_to_solid: std::collections::HashMap<u64, u64> =
        std::collections::HashMap::new();
    for (solid_id, solid_holder) in &table.manifold_solid_brep {
        // Extract the outer shell entity ID (may be shell or oriented_shell).
        if let PlaceHolder::Ref(Name::Entity(outer_idx)) = &solid_holder.outer {
            let shell_id = resolve_to_shell_id(table, *outer_idx);
            shell_to_solid.insert(shell_id, *solid_id);
        }
        // Extract void shell entity IDs (always oriented_shell references).
        for void_ref in &solid_holder.voids {
            if let PlaceHolder::Ref(Name::Entity(void_idx)) = void_ref {
                let shell_id = resolve_to_shell_id(table, *void_idx);
                shell_to_solid.insert(shell_id, *solid_id);
            }
        }
    }
    shell_to_solid
}

/// Build the `StepTopology` for a shell, given the shell-to-solid mapping.
///
/// If the shell belongs to a solid, call `to_compressed_solid` and wrap as
/// `StepTopology::Solid`. Otherwise wrap the already-converted
/// `CompressedShell` as `StepTopology::Shell`.
///
/// Multiple shells in the same solid share the same `CompressedSolid` via
/// `Arc` inside `CompressedShellData`.
fn build_topology_for_shell(
    shell_id: &u64,
    compressed: &OriginalShell,
    table: &Table,
    shell_to_solid: &std::collections::HashMap<u64, u64>,
    solid_cache: &Mutex<std::collections::HashMap<u64, CompressedShellData>>,
) -> Option<StepTopology> {
    if let Some(&solid_id) = shell_to_solid.get(shell_id) {
        // Check cache first (multiple shells can belong to the same solid).
        let mut cache = solid_cache.lock();
        let solid_data = if let Some(cached) = cache.get(&solid_id) {
            cached.clone()
        } else {
            // Convert the solid.
            let solid_holder = table.manifold_solid_brep.get(&solid_id)?;
            match table.to_compressed_trimmed_solid(solid_holder) {
                Ok(solid) => {
                    let data = CompressedShellData::new(solid);
                    cache.insert(solid_id, data.clone());
                    data
                }
                Err(e) => {
                    log::warn!(
                        "Failed to convert solid #{} to CompressedSolid: {}",
                        solid_id,
                        e
                    );
                    return None;
                }
            }
        };
        Some(StepTopology::Solid(solid_data))
    } else {
        Some(StepTopology::Shell(CompressedShellData::new(
            compressed.clone(),
        )))
    }
}

fn compute_shell_bounds(
    compressed: &OriginalShell,
    transform: Option<&Transform>,
) -> Option<StepBounds> {
    let mut bounds = StepBoundsBuilder::default();
    let mut push_point = |point: Point3| {
        let mut coord = [point.x, point.y, point.z];
        if let Some(xform) = transform {
            coord = xform.transform_point(coord);
        }
        bounds.push(coord);
    };

    compressed
        .vertices
        .iter()
        .for_each(|point| push_point(*point));
    for edge in &compressed.edges {
        let (start, end) = edge.curve.range_tuple();
        for idx in 0..=4 {
            let t = start + (end - start) * idx as f64 / 4.0;
            push_point(edge.curve.subs(t));
        }
    }
    for face in &compressed.faces {
        let (urange, vrange) = face.surface.try_range_tuple();
        if let (Some((u0, u1)), Some((v0, v1))) = (urange, vrange) {
            push_point(face.surface.subs(u0, v0));
            push_point(face.surface.subs(u1, v0));
            push_point(face.surface.subs(u0, v1));
            push_point(face.surface.subs(u1, v1));
            push_point(face.surface.subs((u0 + u1) * 0.5, (v0 + v1) * 0.5));
        }
    }

    bounds.finish()
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

    // Build shell-to-solid mapping for topology preservation.
    let shell_to_solid = build_shell_to_solid_map(&table);
    let solid_cache: Mutex<
        std::collections::HashMap<u64, CompressedShellData>,
    > = Mutex::new(std::collections::HashMap::new());

    // Convert each shell into a triangulated mesh (in parallel).
    let mut shell_entries: Vec<_> = table.shell.iter().collect();
    shell_entries.sort_by_key(|(id, _)| *id);

    let total = shell_entries.len();
    progress.set(0, total as u16);
    let completed = AtomicUsize::new(0);

    let shells: Result<Vec<StepShell>, anyhow::Error> = shell_entries
        .into_par_iter()
        .enumerate()
        .map(|(local_idx, (shell_id, shell_holder))| {
            let compressed = table
                .to_compressed_trimmed_shell(shell_holder)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to convert STEP shell into topology: {e}"
                    )
                })?;

            // Build topology (Solid or Shell).
            let topology = build_topology_for_shell(
                shell_id,
                &compressed,
                &table,
                &shell_to_solid,
                &solid_cache,
            );

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

            let has_boundaries = shell_requires_trimmed_meshing(&compressed);
            let original_shell = CompressedShellData::new(compressed.clone());
            let poly_shell = compressed.clone().robust_triangulation(tol);
            let failed_faces = count_failed_face_meshes(&poly_shell);
            if failed_faces > 0 {
                log::warn!(
                    "Shell {}: {} face meshes failed (has_boundaries={})",
                    local_idx,
                    failed_faces,
                    has_boundaries
                );
            }

            // Extract tessellated curve edges from the meshed shell edges.
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
                topology,
                tessellation_tolerance: tol,
                failed_faces,
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
    /// Loader phase changed.
    Phase(LoadPhase),
    /// Scene bounds are available before shell render messages.
    Bounds(StepBounds),
    /// Metadata parsed from STEP header.
    Metadata(StepMetadata),
    /// Total number of shells to process.
    TotalShells(usize),
    /// Progress update for a specific loader phase.
    Progress {
        phase: LoadPhase,
        current: usize,
        total: usize,
    },
    /// A completed shell.
    Shell(StepShell),
    /// Loading finished successfully.
    Done,
    /// An error occurred.
    Error(String),
}

struct PreparedShell {
    local_idx: usize,
    compressed: OriginalShell,
    topology: Option<StepTopology>,
    curve_types: Vec<String>,
    color: Option<[f32; 3]>,
    transform: Option<Transform>,
    tolerance: f64,
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
        let _ = tx.send(LoadMessage::Phase(LoadPhase::Reading));
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(LoadMessage::Error(format!(
                    "Failed to read STEP file {}: {e}",
                    path.display()
                )));
                return;
            }
        };
        if let Err(e) = load_step_from_string_inner(raw, &tx, tolerance_factor)
        {
            let _ = tx.send(LoadMessage::Error(e.to_string()));
        }
    });

    rx
}

/// Start loading STEP data from a string in a background thread, streaming
/// results via channel.
pub fn load_step_from_string_streaming(
    data: String,
    tolerance_factor: f64,
) -> Receiver<LoadMessage> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        if let Err(e) = load_step_from_string_inner(data, &tx, tolerance_factor)
        {
            let _ = tx.send(LoadMessage::Error(e.to_string()));
        }
    });

    rx
}

fn load_step_from_string_inner(
    raw: String,
    tx: &Sender<LoadMessage>,
    tolerance_factor: f64,
) -> anyhow::Result<()> {
    let raw = raw.as_str();

    tx.send(LoadMessage::Phase(LoadPhase::Parsing))?;

    // Parse colors from raw STEP content.
    let entity_colors = parse_step_colors(raw);
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
    let assembly_transforms = parse_assembly_transforms(raw);
    log::info!(
        "Parsed {} assembly transforms from STEP file",
        assembly_transforms.len()
    );

    let exchange = parse(raw).context("Failed to parse STEP file")?;
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

    tx.send(LoadMessage::Phase(LoadPhase::Preparing))?;

    // Convert each shell into topology data before meshing so scene bounds are
    // available early.
    let mut shell_entries: Vec<_> = table.shell.iter().collect();
    shell_entries.sort_by_key(|(id, _)| *id);

    let total = shell_entries.len();
    tx.send(LoadMessage::TotalShells(total))?;

    // Build shell-to-solid mapping for topology preservation.
    let shell_to_solid = build_shell_to_solid_map(&table);
    let solid_cache: Arc<
        Mutex<std::collections::HashMap<u64, CompressedShellData>>,
    > = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let prepared_count = Arc::new(AtomicUsize::new(0));

    let prepared_shells: Result<Vec<PreparedShell>, anyhow::Error> =
        shell_entries
            .into_par_iter()
            .enumerate()
            .map(|(local_idx, (shell_id, shell_holder))| {
                // Look up color for this shell's entity ID.
                let color = entity_colors.get(shell_id).copied();
                // Look up assembly transform for this shell.
                let transform = assembly_transforms.get(shell_id).copied();
                log::info!(
                    "Shell {} (entity #{}): color={:?}, transform={:?}",
                    local_idx,
                    shell_id,
                    color,
                    transform
                        .map(|t| [t.cols[3][0], t.cols[3][1], t.cols[3][2]])
                );

                let compressed =
                    table.to_compressed_trimmed_shell(shell_holder).map_err(
                        |e| {
                            anyhow::anyhow!(
                                "Failed to convert STEP shell into topology: {e}"
                            )
                        },
                    )?;

                // Build topology (Solid or Shell).
                let topology = build_topology_for_shell(
                    shell_id,
                    &compressed,
                    &table,
                    &shell_to_solid,
                    &solid_cache,
                );

                // Classify curve types from original geometry before
                // tessellation.
                let curve_types: Vec<String> = compressed
                    .edges
                    .iter()
                    .map(|e| classify_curve_type(&e.curve))
                    .collect();

                let bounds = compute_shell_bounds(&compressed, transform.as_ref());
                let bbox_diameter =
                    bounds.map(StepBounds::diameter).unwrap_or_default();
                let mut tol =
                    f64::max(bbox_diameter * tolerance_factor, TOLERANCE);
                if !tol.is_finite() {
                    tol = 0.01;
                }
                log::info!(
                    "Tessellation: bbox_diameter={:.4}, factor={:.6}, tol={:.6}",
                    bbox_diameter,
                    tolerance_factor,
                    tol
                );

                let done =
                    prepared_count.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = tx.send(LoadMessage::Progress {
                    phase: LoadPhase::Preparing,
                    current: done,
                    total,
                });

                Ok(PreparedShell {
                    local_idx,
                    compressed,
                    topology,
                    curve_types,
                    color,
                    transform,
                    tolerance: tol,
                })
            })
            .collect();

    let mut prepared_shells = prepared_shells?;
    prepared_shells.sort_by_key(|shell| shell.local_idx);

    if prepared_shells.is_empty() {
        anyhow::bail!("No shells found in STEP file");
    }

    tx.send(LoadMessage::Phase(LoadPhase::Meshing))?;

    let completed = Arc::new(AtomicUsize::new(0));

    let mut shells: Vec<StepShell> = prepared_shells
        .into_par_iter()
        .map(|prepared| {
            let PreparedShell {
                local_idx,
                compressed,
                topology,
                curve_types,
                color,
                transform,
                tolerance: tol,
            } = prepared;

            let has_boundaries = shell_requires_trimmed_meshing(&compressed);
            let original_shell = CompressedShellData::new(compressed.clone());
            let poly_shell = compressed.clone().robust_triangulation(tol);
            let failed_faces = count_failed_face_meshes(&poly_shell);
            if failed_faces > 0 {
                log::warn!(
                    "Shell {}: {} face meshes failed (has_boundaries={})",
                    local_idx,
                    failed_faces,
                    has_boundaries
                );
            }

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

            let total_tris: usize =
                faces.iter().map(|f| f.mesh.tri_faces().len()).sum();
            log::info!(
                "Shell {}: {} faces, {} triangles (tol={:.6})",
                local_idx,
                faces.len(),
                total_tris,
                tol
            );

            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = tx.send(LoadMessage::Progress {
                phase: LoadPhase::Meshing,
                current: done,
                total,
            });

            StepShell {
                id: local_idx,
                name: format!("Shell {}", local_idx + 1),
                faces,
                color,
                transform,
                edges: all_edges,
                curve_edges,
                original_shell: Some(original_shell),
                topology,
                tessellation_tolerance: tol,
                failed_faces,
            }
        })
        .collect();

    shells.sort_by_key(|shell| shell.id);

    if let Some(bounds) = bounds_from_step_shells(&shells) {
        tx.send(LoadMessage::Bounds(bounds))?;
    }

    shells
        .into_iter()
        .try_for_each(|shell| tx.send(LoadMessage::Shell(shell)))?;

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
    let face = modified.faces.get_mut(face_idx)?;
    let original_boundaries = face.boundaries.clone();
    face.boundaries = active_boundary_indices
        .iter()
        .filter_map(|&idx| original_boundaries.get(idx).cloned())
        .collect();

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
        Curve3D::IntersectionCurve(_) => "IntersectionCurve",
        Curve3D::SurfaceCurve(_) => "SurfaceCurve",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use monstertruck::topology::compress::{
        CompressedEdge, CompressedEdgeUse, CompressedFace,
        CompressedTrimmedFace, CompressedTrimmedShell,
    };

    #[test]
    fn boundary_without_exact_pcurve_still_requires_trimmed_meshing() {
        let shell = CompressedTrimmedShell {
            vertices: vec![0usize, 1usize],
            edges: vec![CompressedEdge {
                vertices: (0, 1),
                curve: 5usize,
            }],
            faces: vec![CompressedTrimmedFace {
                boundaries: vec![vec![CompressedEdgeUse {
                    index: 0,
                    orientation: true,
                    trim_curve: None::<usize>,
                }]],
                orientation: true,
                surface: (),
            }],
        };

        assert!(shell_requires_trimmed_meshing(&shell));
    }

    #[test]
    fn empty_boundaries_do_not_require_trimmed_meshing() {
        let shell = CompressedTrimmedShell::<usize, usize, (), usize> {
            vertices: Vec::new(),
            edges: Vec::new(),
            faces: vec![CompressedTrimmedFace {
                boundaries: Vec::new(),
                orientation: true,
                surface: (),
            }],
        };

        assert!(!shell_requires_trimmed_meshing(&shell));
    }

    #[test]
    fn failed_face_meshes_are_counted() {
        let shell = CompressedShell {
            vertices: Vec::<usize>::new(),
            edges: Vec::<CompressedEdge<usize>>::new(),
            faces: vec![
                CompressedFace {
                    boundaries: Vec::new(),
                    orientation: true,
                    surface: Some(PolygonMesh::default()),
                },
                CompressedFace {
                    boundaries: Vec::new(),
                    orientation: true,
                    surface: None,
                },
            ],
            vertex_stable_ids: None,
            edge_stable_ids: None,
            face_stable_ids: None,
        };

        assert_eq!(count_failed_face_meshes(&shell), 1);
    }

    #[test]
    fn load_phase_labels_distinguish_parsing_from_meshing() {
        assert_eq!(LoadPhase::Parsing.label(), "Parsing STEP file");
        assert_eq!(LoadPhase::Meshing.label(), "Tessellating shells");
    }

    #[test]
    fn step_bounds_reports_center_and_scale() {
        let bounds =
            StepBounds::from_points([[1.0, 2.0, 3.0], [5.0, 4.0, 7.0]])
                .expect("bounds should be created from points");

        assert_eq!(bounds.center, [3.0, 3.0, 5.0]);
        assert_eq!(bounds.normalization_scale(), 0.25);
    }

    #[test]
    fn step_shell_bounds_use_tessellated_mesh_positions() {
        let mesh = PolygonMesh::new(
            StandardAttributes {
                positions: vec![
                    Point3::new(0.0, 0.0, 0.0),
                    Point3::new(1.0, 0.0, 0.0),
                    Point3::new(0.0, 1.0, 1.0),
                ],
                normals: vec![Vector3::new(0.0, 0.0, 1.0)],
                ..Default::default()
            },
            Faces::from_iter([[
                (0, None, Some(0)),
                (1, None, Some(0)),
                (2, None, Some(0)),
            ]]),
        );
        let shell = StepShell {
            id: 0,
            name: "test".to_string(),
            faces: vec![StepFace {
                id: 0,
                name: "face".to_string(),
                mesh,
                boundary_loops: Vec::new(),
            }],
            color: None,
            transform: None,
            edges: Vec::new(),
            curve_edges: vec![StepEdge {
                id: 0,
                curve_type: "outside".to_string(),
                points: vec![[0.0, 0.0, -5.0], [0.0, 0.0, 5.0]],
            }],
            original_shell: None,
            topology: None,
            tessellation_tolerance: 0.0,
            failed_faces: 0,
        };

        let bounds = bounds_from_step_shells([shell].as_slice())
            .expect("bounds should come from face meshes");

        assert_eq!(bounds.min, [0.0, 0.0, 0.0]);
        assert_eq!(bounds.max, [1.0, 1.0, 1.0]);
    }
}
