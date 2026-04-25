use std::{env, path::PathBuf, sync::mpsc::Receiver};

use anyhow::Context;
use monster_step_viewer::{
    LoadMessage, StepBounds, StepFace, StepMetadata, StepScene, StepShell,
    load_step_file, load_step_file_streaming,
};

fn point_max_box_excess(point: [f64; 3], min: [f64; 3], max: [f64; 3]) -> f64 {
    [
        min[0] - point[0],
        point[0] - max[0],
        min[1] - point[1],
        point[1] - max[1],
        min[2] - point[2],
        point[2] - max[2],
    ]
    .into_iter()
    .fold(0.0, f64::max)
}

fn point_bounds<I>(points: I) -> Option<([f64; 3], [f64; 3])>
where
    I: IntoIterator<Item = [f64; 3]>,
{
    points.into_iter().fold(None, |acc, point| {
        Some(match acc {
            Some((mut min, mut max)) => {
                (0..3).for_each(|axis| {
                    min[axis] = min[axis].min(point[axis]);
                    max[axis] = max[axis].max(point[axis]);
                });
                (min, max)
            }
            None => (point, point),
        })
    })
}

fn print_bounds(label: &str, min: [f64; 3], max: [f64; 3]) {
    let excess = [min, max]
        .into_iter()
        .map(|point| {
            point_max_box_excess(point, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0])
        })
        .fold(0.0, f64::max);
    println!(
        "{label} bbox=({:.6},{:.6},{:.6})..({:.6},{:.6},{:.6}) unit_excess={:.9}",
        min[0], min[1], min[2], max[0], max[1], max[2], excess,
    );
}

fn print_step_bounds(label: &str, bounds: StepBounds) {
    print_bounds(label, bounds.min, bounds.max);
}

fn print_face(face: &StepFace) {
    let positions = face.mesh.positions();
    let bounds =
        point_bounds(positions.iter().map(|point| [point.x, point.y, point.z]));
    if let Some((min, max)) = bounds {
        let excess = positions
            .iter()
            .map(|point| {
                point_max_box_excess(
                    [point.x, point.y, point.z],
                    [0.0, 0.0, 0.0],
                    [1.0, 1.0, 1.0],
                )
            })
            .fold(0.0, f64::max);
        println!(
            "face={} tris={} quads={} positions={} loops={} bbox=({:.6},{:.6},{:.6})..({:.6},{:.6},{:.6}) unit_excess={:.9}",
            face.id,
            face.mesh.tri_faces().len(),
            face.mesh.quad_faces().len(),
            positions.len(),
            face.boundary_loops.len(),
            min[0],
            min[1],
            min[2],
            max[0],
            max[1],
            max[2],
            excess,
        );
    }
}

fn print_shell(prefix: &str, shell_index: usize, shell: &StepShell) {
    let total_tris = shell
        .faces
        .iter()
        .map(|face| face.mesh.tri_faces().len())
        .sum::<usize>();
    println!(
        "{prefix} shell={shell_index} faces={} failed_faces={} tris={} edges={} curve_edges={}",
        shell.faces.len(),
        shell.failed_faces,
        total_tris,
        shell.edges.len(),
        shell.curve_edges.len(),
    );
    shell.faces.iter().for_each(print_face);

    let wire_bounds = point_bounds(
        shell.edges.iter().flat_map(|(start, end)| [*start, *end]),
    );
    if let Some((min, max)) = wire_bounds {
        print_bounds("wire_edges", min, max);
    }

    let curve_bounds = point_bounds(
        shell
            .curve_edges
            .iter()
            .flat_map(|edge| edge.points.iter().copied()),
    );
    if let Some((min, max)) = curve_bounds {
        print_bounds("curve_edges", min, max);
    }
}

fn collect_streamed_scene(
    receiver: Receiver<LoadMessage>,
) -> anyhow::Result<StepScene> {
    let mut metadata = StepMetadata::default();
    let mut shells = Vec::new();

    loop {
        match receiver.recv()? {
            LoadMessage::Phase(phase) => {
                println!("stream phase={phase:?}");
            }
            LoadMessage::Bounds(bounds) => {
                print_step_bounds("stream scene_bounds", bounds);
            }
            LoadMessage::Metadata(next_metadata) => {
                println!(
                    "stream metadata entities={}",
                    next_metadata.entity_count
                );
                metadata = next_metadata;
            }
            LoadMessage::TotalShells(total) => {
                println!("stream total_shells={total}");
            }
            LoadMessage::Progress {
                phase,
                current,
                total,
            } => {
                println!("stream progress phase={phase:?} {current}/{total}");
            }
            LoadMessage::Shell(shell) => {
                let shell_index = shells.len();
                print_shell("stream", shell_index, &shell);
                shells.push(shell);
            }
            LoadMessage::Done => {
                break;
            }
            LoadMessage::Error(message) => {
                anyhow::bail!(message);
            }
        }
    }

    Ok(StepScene { metadata, shells })
}

fn print_scene(prefix: &str, scene: &StepScene) {
    println!(
        "{prefix} scene shells={} entities={}",
        scene.shells.len(),
        scene.metadata.entity_count,
    );
    scene
        .shells
        .iter()
        .enumerate()
        .for_each(|(shell_index, shell)| {
            print_shell(prefix, shell_index, shell);
        });
}

fn main() -> anyhow::Result<()> {
    let path = env::args_os().nth(1).map(PathBuf::from).context(
        "usage: cargo run --example inspect_loaded_step -- <file.step>",
    )?;
    let scene = load_step_file(&path)?;
    print_scene("direct", &scene);

    let streamed_scene =
        collect_streamed_scene(load_step_file_streaming(path, 0.001))?;
    print_scene("stream collected", &streamed_scene);

    Ok(())
}
