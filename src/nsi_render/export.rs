use std::{
    fmt::{self, Display, Write},
    fs, io,
    path::Path,
};

use glam::Mat4;

use monster_step_viewer::{CompressedShellData, StepScene};

use super::{
    brep::{self, NsiBrepSurfaceData, NsiBrepTrimData},
    mat4_to_nsi,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct NsiFileExportOptions {
    pub model_matrix: Mat4,
    pub view_matrix: Mat4,
    pub fov_y_degrees: f32,
    pub resolution: [u32; 2],
}

pub(crate) fn export_scene_to_nsi_file(
    path: &Path,
    scene: &StepScene,
    options: NsiFileExportOptions,
) -> io::Result<usize> {
    let mut output = String::new();
    let surface_count = write_scene(&mut output, path, scene, options)
        .map_err(|_| io::Error::other("failed to format NSI scene"))?;
    fs::write(path, output)?;
    Ok(surface_count)
}

fn write_scene(
    output: &mut String,
    path: &Path,
    scene: &StepScene,
    options: NsiFileExportOptions,
) -> Result<usize, fmt::Error> {
    write_scene_header(output, path, options)?;
    scene
        .shells
        .iter()
        .filter_map(|shell| {
            shell.original_shell.as_ref().map(|data| (shell.id, data))
        })
        .try_fold(0usize, |surface_count, (shell_id, shell_data)| {
            write_shell(output, shell_id, shell_data, options.model_matrix)
                .map(|shell_surface_count| surface_count + shell_surface_count)
        })
}

fn write_scene_header(
    output: &mut String,
    path: &Path,
    options: NsiFileExportOptions,
) -> fmt::Result {
    let image_filename = output_image_filename(path);
    let camera_matrix = mat4_to_nsi(options.view_matrix.inverse());

    writeln!(output, "# Monster STEP Viewer NSI export.")?;
    writeln!(
        output,
        "# Uses weighted homogeneous `Pw` and per-loop `trimcurves.sense`."
    )?;
    writeln!(output)?;
    writeln!(output, "Create \"mstpv_camera_xform\" \"transform\"")?;
    write_set_attribute(output, "mstpv_camera_xform")?;
    write_f64_matrix_attr(output, "transformationmatrix", &camera_matrix)?;
    writeln!(
        output,
        "Connect \"mstpv_camera_xform\" \"\" \".root\" \"objects\""
    )?;
    writeln!(output)?;
    writeln!(output, "Create \"mstpv_camera\" \"perspectivecamera\"")?;
    write_set_attribute(output, "mstpv_camera")?;
    write_f32_attr(output, "fov", options.fov_y_degrees)?;
    writeln!(
        output,
        "Connect \"mstpv_camera\" \"\" \"mstpv_camera_xform\" \"objects\""
    )?;
    writeln!(output)?;
    writeln!(output, "Create \"mstpv_screen\" \"screen\"")?;
    write_set_attribute(output, "mstpv_screen")?;
    write_u32_slice_attr(output, "resolution", &options.resolution)?;
    write_i32_attr(output, "oversampling", 16)?;
    writeln!(
        output,
        "Connect \"mstpv_screen\" \"\" \"mstpv_camera\" \"screens\""
    )?;
    writeln!(output)?;
    writeln!(output, "Create \"mstpv_beauty\" \"outputlayer\"")?;
    write_set_attribute(output, "mstpv_beauty")?;
    write_string_attr(output, "variablename", "Ci")?;
    write_i32_attr(output, "withalpha", 1)?;
    write_string_attr(output, "scalarformat", "uint8")?;
    write_string_attr(output, "colorprofile", "srgb")?;
    writeln!(
        output,
        "Connect \"mstpv_beauty\" \"\" \"mstpv_screen\" \"outputlayers\""
    )?;
    writeln!(output)?;
    writeln!(output, "Create \"mstpv_driver\" \"outputdriver\"")?;
    write_set_attribute(output, "mstpv_driver")?;
    write_string_attr(output, "drivername", "tiff")?;
    write_string_attr(output, "imagefilename", &image_filename)?;
    writeln!(
        output,
        "Connect \"mstpv_driver\" \"\" \"mstpv_beauty\" \"outputdrivers\""
    )?;
    writeln!(output)?;
    writeln!(output, "Create \"mstpv_shader\" \"shader\"")?;
    write_set_attribute(output, "mstpv_shader")?;
    write_string_attr(output, "shaderfilename", "${DELIGHT}/osl/dlPrincipled")?;
    write_color_attr(output, "i_color", [0.8, 0.8, 0.8])?;
    write_f32_attr(output, "roughness", 0.3)?;
    writeln!(output)?;
    writeln!(output, "Create \"mstpv_environment\" \"environment\"")?;
    writeln!(
        output,
        "Connect \"mstpv_environment\" \"\" \".root\" \"objects\""
    )?;
    writeln!(output, "Create \"mstpv_env_attrib\" \"attributes\"")?;
    write_set_attribute(output, "mstpv_env_attrib")?;
    write_i32_attr(output, "visibility.camera", 0)?;
    writeln!(
        output,
        "Connect \"mstpv_env_attrib\" \"\" \"mstpv_environment\" \"geometryattributes\""
    )?;
    writeln!(output, "Create \"mstpv_env_shader\" \"shader\"")?;
    write_set_attribute(output, "mstpv_env_shader")?;
    write_string_attr(
        output,
        "shaderfilename",
        "${DELIGHT}/osl/environmentLight",
    )?;
    write_f32_attr(output, "intensity", 1.0)?;
    writeln!(
        output,
        "Connect \"mstpv_env_shader\" \"\" \"mstpv_env_attrib\" \"surfaceshader\""
    )?;
    writeln!(output)
}

fn write_shell(
    output: &mut String,
    shell_id: usize,
    shell_data: &CompressedShellData,
    model_matrix: Mat4,
) -> Result<usize, fmt::Error> {
    let surfaces = brep::shell_data_to_nsi_surfaces(shell_data);
    if surfaces.is_empty() {
        Ok(0)
    } else {
        let transform_handle = format!("mstpv_brep_xform_shell_{shell_id}");
        let nsi_matrix = mat4_to_nsi(model_matrix);
        writeln!(output, "Create \"{transform_handle}\" \"transform\"")?;
        write_set_attribute(output, &transform_handle)?;
        write_f64_matrix_attr(output, "transformationmatrix", &nsi_matrix)?;
        writeln!(
            output,
            "Connect \"{transform_handle}\" \"\" \".root\" \"objects\""
        )?;
        surfaces.iter().try_for_each(|surface| {
            write_surface_node(output, &transform_handle, shell_id, surface)
        })?;
        Ok(surfaces.len())
    }
}

fn write_surface_node(
    output: &mut String,
    transform_handle: &str,
    shell_id: usize,
    surface: &NsiBrepSurfaceData,
) -> fmt::Result {
    let surface_handle =
        format!("mstpv_brep_surface_shell_{shell_id}_{}", surface.face_index);
    let attrib_handle =
        format!("mstpv_brep_attrib_shell_{shell_id}_{}", surface.face_index);
    writeln!(output)?;
    writeln!(output, "Create \"{surface_handle}\" \"nurbs\"")?;
    write_set_attribute(output, &surface_handle)?;
    write_surface_attributes(output, surface)?;
    if let Some(trims) = &surface.trims {
        write_trim_attributes(output, trims)?;
    }
    writeln!(
        output,
        "Connect \"{surface_handle}\" \"\" \"{transform_handle}\" \"objects\""
    )?;
    writeln!(output, "Create \"{attrib_handle}\" \"attributes\"")?;
    writeln!(
        output,
        "Connect \"mstpv_shader\" \"\" \"{attrib_handle}\" \"surfaceshader\""
    )?;
    writeln!(
        output,
        "Connect \"{attrib_handle}\" \"\" \"{surface_handle}\" \"geometryattributes\""
    )
}

fn write_surface_attributes(
    output: &mut String,
    surface: &NsiBrepSurfaceData,
) -> fmt::Result {
    write_i32_attr(output, "nu", surface.nu)?;
    write_i32_attr(output, "nv", surface.nv)?;
    write_i32_attr(output, "uorder", surface.uorder)?;
    write_i32_attr(output, "vorder", surface.vorder)?;
    write_f32_slice_attr(output, "uknot", &surface.uknot)?;
    write_f32_slice_attr(output, "vknot", &surface.vknot)?;
    write_f32_attr(output, "umin", surface.umin)?;
    write_f32_attr(output, "umax", surface.umax)?;
    write_f32_attr(output, "vmin", surface.vmin)?;
    write_f32_attr(output, "vmax", surface.vmax)?;
    write_pw_attr(output, &surface.pw)
}

fn write_trim_attributes(
    output: &mut String,
    trims: &NsiBrepTrimData,
) -> fmt::Result {
    write_i32_attr(output, "trimcurves.nloops", trims.nloops)?;
    write_i32_slice_attr(output, "trimcurves.ncurves", &trims.ncurves)?;
    write_i32_slice_attr(output, "trimcurves.n", &trims.n)?;
    write_i32_slice_attr(output, "trimcurves.order", &trims.order)?;
    write_f32_slice_attr(output, "trimcurves.knot", &trims.knot)?;
    write_f32_slice_attr(output, "trimcurves.min", &trims.min)?;
    write_f32_slice_attr(output, "trimcurves.max", &trims.max)?;
    write_f32_slice_attr(output, "trimcurves.u", &trims.u)?;
    write_f32_slice_attr(output, "trimcurves.v", &trims.v)?;
    write_f32_slice_attr(output, "trimcurves.w", &trims.w)?;
    write_i32_slice_attr(output, "trimcurves.sense", &trims.sense)
}

fn write_set_attribute(output: &mut String, handle: &str) -> fmt::Result {
    writeln!(output, "SetAttribute \"{}\"", escape_nsi_string(handle))
}

fn write_i32_attr(output: &mut String, name: &str, value: i32) -> fmt::Result {
    writeln!(output, "  \"{name}\" \"int\" 1 {value}")
}

fn write_f32_attr(output: &mut String, name: &str, value: f32) -> fmt::Result {
    writeln!(output, "  \"{name}\" \"float\" 1 {value}")
}

fn write_string_attr(
    output: &mut String,
    name: &str,
    value: &str,
) -> fmt::Result {
    writeln!(
        output,
        "  \"{name}\" \"string\" 1 \"{}\"",
        escape_nsi_string(value)
    )
}

fn write_color_attr(
    output: &mut String,
    name: &str,
    values: [f32; 3],
) -> fmt::Result {
    write!(output, "  \"{name}\" \"color\" 1 [")?;
    write_values(output, &values)?;
    writeln!(output, " ]")
}

fn write_u32_slice_attr(
    output: &mut String,
    name: &str,
    values: &[u32],
) -> fmt::Result {
    write_slice_attr(output, name, "int", values)
}

fn write_i32_slice_attr(
    output: &mut String,
    name: &str,
    values: &[i32],
) -> fmt::Result {
    write_slice_attr(output, name, "int", values)
}

fn write_f32_slice_attr(
    output: &mut String,
    name: &str,
    values: &[f32],
) -> fmt::Result {
    write_slice_attr(output, name, "float", values)
}

fn write_f64_matrix_attr(
    output: &mut String,
    name: &str,
    values: &[f64; 16],
) -> fmt::Result {
    write_slice_attr(output, name, "doublematrix", values)
}

fn write_slice_attr<T: Display>(
    output: &mut String,
    name: &str,
    ty: &str,
    values: &[T],
) -> fmt::Result {
    write!(output, "  \"{name}\" \"{ty}\" {} [", values.len())?;
    write_values(output, values)?;
    writeln!(output, " ]")
}

fn write_pw_attr(output: &mut String, points: &[[f32; 4]]) -> fmt::Result {
    write!(output, "  \"Pw\" \"float\" {} [", points.len() * 4)?;
    points
        .iter()
        .flat_map(|point| point.iter())
        .try_for_each(|value| write!(output, " {value}"))?;
    writeln!(output, " ]")
}

fn write_values<T: Display>(output: &mut String, values: &[T]) -> fmt::Result {
    values
        .iter()
        .try_for_each(|value| write!(output, " {value}"))
}

fn output_image_filename(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("mstpv_nsi_export");
    format!("{stem}.tif")
}

fn escape_nsi_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_trims() -> NsiBrepTrimData {
        NsiBrepTrimData {
            nloops: 2,
            ncurves: vec![1, 1],
            n: vec![2, 2],
            order: vec![2, 2],
            knot: vec![0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0],
            min: vec![0.0, 0.0],
            max: vec![1.0, 1.0],
            u: vec![0.0, 1.0, 0.25, 0.75],
            v: vec![0.0, 0.0, 0.25, 0.25],
            w: vec![1.0, 1.0, 1.0, 1.0],
            sense: vec![0, 1],
        }
    }

    fn test_surface() -> NsiBrepSurfaceData {
        NsiBrepSurfaceData {
            face_index: 3,
            nu: 2,
            nv: 2,
            uorder: 2,
            vorder: 2,
            uknot: vec![0.0, 0.0, 1.0, 1.0],
            vknot: vec![0.0, 0.0, 1.0, 1.0],
            umin: 0.0,
            umax: 1.0,
            vmin: 0.0,
            vmax: 1.0,
            pw: vec![
                [0.0, 0.0, 0.0, 1.0],
                [2.0, 0.0, 0.0, 2.0],
                [0.0, 1.0, 0.0, 1.0],
                [1.0, 1.0, 0.0, 1.0],
            ],
            trims: None,
            sampled_trim_fallback_count: 0,
        }
    }

    #[test]
    fn trim_attributes_write_per_loop_sense_array() {
        let mut output = String::new();

        write_trim_attributes(&mut output, &test_trims())
            .expect("trim serialization should succeed");

        assert!(output.contains("\"trimcurves.sense\" \"int\" 2 [ 0 1 ]"));
    }

    #[test]
    fn surface_attributes_write_weighted_homogeneous_pw() {
        let mut output = String::new();

        write_surface_attributes(&mut output, &test_surface())
            .expect("surface serialization should succeed");

        assert!(output.contains("\"Pw\" \"float\" 16 ["));
        assert!(output.contains("2 0 0 2"));
    }

    #[test]
    fn boxy_fixture_export_writes_scene_camera_pw_and_loop_sense_arrays() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("step-files/boxy_with_surfacetex.stp");
        let scene = monster_step_viewer::load_step_file(&path)
            .expect("STEP fixture should load");
        let options = NsiFileExportOptions {
            model_matrix: Mat4::IDENTITY,
            view_matrix: Mat4::IDENTITY,
            fov_y_degrees: 45.0,
            resolution: [640, 480],
        };
        let mut output = String::new();

        let surface_count = write_scene(
            &mut output,
            Path::new("boxy_with_surfacetex.nsi"),
            &scene,
            options,
        )
        .expect("NSI serialization should succeed");

        assert!(surface_count > 0);
        assert!(
            output.contains("Create \"mstpv_camera\" \"perspectivecamera\"")
        );
        assert!(output.contains("\"Pw\" \"float\""));
        assert!(output.contains("\"trimcurves.sense\" \"int\" 6 ["));
    }
}
