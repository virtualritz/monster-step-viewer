//! NSI (3Delight) renderer driver.
//!
//! Drives an interactive in-viewport render: 3Delight writes progressive
//! pixels through its callback driver while we push scene changes through the
//! `Context` and call `render_control(Synchronize)` to refresh them.
//!
//! Lifecycle:
//! * `NsiRenderState::new()` creates the context, all singleton scene nodes
//!   (camera/screen/output, shader, environment), and spawns a render-control
//!   worker thread. No render is started yet.
//! * `start()` posts `Action::Start` (interactive + progressive) once. From
//!   then on the render runs asynchronously.
//! * `update_shell_brep(key, ...)` pushes one shell's BRep to NSI; geometry is
//!   sent once per shell — subsequent calls with the same key replace the
//!   surfaces.
//! * `update_camera(...)` posts new camera attributes and sends `Sync`.
//! * `set_face_visibilities(...)` toggles all visibility attributes for a batch
//!   of face `attributes` nodes and sends one `Sync`. No geometry re-push.
//! * `stop()` halts the render thread; geometry stays alive in the context
//!   unless dropped.

#[cfg(feature = "nsi-render")]
use crate::{HashMap, HashSet};
use std::{
    env,
    num::NonZeroUsize,
    path::PathBuf,
    sync::OnceLock,
    thread::{self, JoinHandle},
};

#[cfg(feature = "nsi-render")]
use crossbeam::channel::{Receiver, Sender};
use glam::Mat4;
#[cfg(feature = "nsi-render")]
use parking_lot::Mutex;
#[cfg(feature = "nsi-render")]
use rayon::prelude::*;
#[cfg(feature = "nsi-render")]
use std::sync::Arc;

#[cfg(feature = "nsi-render")]
use monster_step_viewer::CompressedShellData;

mod brep;
#[cfg(feature = "nsi-export")]
mod export;

#[cfg(feature = "nsi-export")]
pub(crate) use export::{NsiFileExportOptions, export_scene_to_nsi_file};

/// Detect 3Delight installation. Mirrors `nsi-ffi-wrap`'s lookup chain.
#[cfg(feature = "nsi-render")]
pub fn detect_3delight() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    let default = PathBuf::from("/usr/local/3delight/lib/lib3delight.so");
    #[cfg(target_os = "macos")]
    let default = PathBuf::from("/Applications/3Delight/lib/lib3delight.dylib");
    #[cfg(target_os = "windows")]
    let default = PathBuf::from(r"C:\Program Files\3Delight\bin\3Delight.dll");

    if default.exists() {
        return Some(default);
    }

    if let Ok(delight) = env::var("DELIGHT") {
        #[cfg(target_os = "linux")]
        let path = PathBuf::from(&delight).join("lib/lib3delight.so");
        #[cfg(target_os = "macos")]
        let path = PathBuf::from(&delight).join("lib/lib3delight.dylib");
        #[cfg(target_os = "windows")]
        let path = PathBuf::from(&delight).join("bin/3Delight.dll");

        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Commands posted to the render-control worker thread.
#[cfg(feature = "nsi-render")]
#[derive(Debug)]
enum NsiCommand {
    Start,
    Stop,
    Synchronize,
}

/// Stable handles for the singleton scene nodes.
#[cfg(feature = "nsi-render")]
#[derive(Clone, Debug)]
struct NsiHandles {
    camera_xform: String,
    camera: String,
    screen: String,
    output_layer: String,
    output_driver: String,
    shader: String,
    environment: String,
    env_shader: String,
    env_attrib: String,
}

#[cfg(feature = "nsi-render")]
impl Default for NsiHandles {
    fn default() -> Self {
        Self {
            camera_xform: "mstpv_camera_xform".to_string(),
            camera: "mstpv_camera".to_string(),
            screen: "mstpv_screen".to_string(),
            output_layer: "mstpv_beauty".to_string(),
            output_driver: "mstpv_driver".to_string(),
            shader: "mstpv_shader".to_string(),
            environment: "mstpv_env".to_string(),
            env_shader: "mstpv_env_shader".to_string(),
            env_attrib: "mstpv_env_attrib".to_string(),
        }
    }
}

#[cfg(feature = "nsi-render")]
#[derive(Clone)]
struct DynamicBrepSurfaceHandles {
    face_index: usize,
    surface: String,
    attrib: String,
}

#[cfg(feature = "nsi-render")]
#[derive(Clone)]
struct DynamicBrepHandles {
    xform: String,
    surfaces: Vec<DynamicBrepSurfaceHandles>,
}

/// Pixels supplied by 3Delight's callback driver for the egui overlay.
#[cfg(feature = "nsi-render")]
struct OverlayImage {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    dirty: bool,
    first_dirty_row: usize,
    last_dirty_row: usize,
}

/// Persistent NSI render state — context + scene nodes + render-control
/// worker.
#[cfg(feature = "nsi-render")]
pub struct NsiRenderState {
    context: nsi::Context<'static>,
    handles: NsiHandles,
    command_tx: Option<Sender<NsiCommand>>,
    last_view_matrix: Mutex<Option<[f32; 16]>>,
    last_fov: Mutex<f32>,
    last_clip_range: Mutex<Option<[f32; 2]>>,
    brep_handles: Mutex<HashMap<String, DynamicBrepHandles>>,
    render_thread: Option<JoinHandle<()>>,
    is_rendering: Mutex<bool>,
    overlay_image: Arc<Mutex<OverlayImage>>,
}

#[cfg(feature = "nsi-render")]
impl NsiRenderState {
    /// Create the context, build all singleton scene nodes, and spawn the
    /// render-control worker. Geometry is added later via
    /// `update_shell_brep`.
    pub fn new() -> Result<Self, String> {
        let context =
            nsi::Context::new(None).ok_or("Failed to create NSI context")?;
        let handles = NsiHandles::default();
        let overlay_image = Self::new_overlay_image();

        Self::create_scene_nodes(&context, &handles, overlay_image.clone())?;

        let (command_tx, command_rx) = crossbeam::channel::unbounded();
        let ctx_for_thread = context.clone();
        let render_thread = thread::spawn(move || {
            render_thread_main(ctx_for_thread, command_rx);
        });

        Ok(Self {
            context,
            handles,
            command_tx: Some(command_tx),
            last_view_matrix: Mutex::new(None),
            last_fov: Mutex::new(0.0),
            last_clip_range: Mutex::new(None),
            brep_handles: Mutex::new(HashMap::default()),
            render_thread: Some(render_thread),
            is_rendering: Mutex::new(false),
            overlay_image,
        })
    }

    fn new_overlay_image() -> Arc<Mutex<OverlayImage>> {
        Arc::new(Mutex::new(OverlayImage {
            pixels: vec![0; 1024 * 1024 * 4],
            width: 1024,
            height: 1024,
            dirty: false,
            first_dirty_row: usize::MAX,
            last_dirty_row: 0,
        }))
    }

    fn create_scene_nodes(
        ctx: &nsi::Context,
        handles: &NsiHandles,
        overlay_image: Arc<Mutex<OverlayImage>>,
    ) -> Result<(), String> {
        // Camera transform.
        ctx.create(&handles.camera_xform, nsi::TRANSFORM, None);
        ctx.connect(&handles.camera_xform, None, nsi::ROOT, "objects", None);

        // Camera.
        ctx.create(&handles.camera, nsi::PERSPECTIVE_CAMERA, None);
        ctx.connect(
            &handles.camera,
            None,
            &handles.camera_xform,
            "objects",
            None,
        );
        ctx.set_attribute(&handles.camera, &[nsi::f32!("fov", 45.0)]);

        // Screen resolution matches the in-viewport NSI texture.
        ctx.create(&handles.screen, nsi::SCREEN, None);
        ctx.connect(&handles.screen, None, &handles.camera, "screens", None);
        ctx.set_attribute(
            &handles.screen,
            &[
                nsi::i32_slice!("resolution", &[1024, 1024])
                    .array_len(NonZeroUsize::new(2).unwrap()),
                nsi::i32!("oversampling", 16),
            ],
        );

        // Output layer is delivered to an in-process callback as float RGBA.
        ctx.create(&handles.output_layer, nsi::OUTPUT_LAYER, None);
        ctx.set_attribute(
            &handles.output_layer,
            &[
                nsi::string!("variablename", "Ci"),
                nsi::i32!("withalpha", 1),
                nsi::string!("scalarformat", "float"),
                nsi::f64!("filterwidth", 1.0),
            ],
        );
        ctx.connect(
            &handles.output_layer,
            None,
            &handles.screen,
            "outputlayers",
            None,
        );

        // Output driver — 3Delight writes progressive buckets into the shared
        // image, which egui composites over the viewport.
        ctx.create(&handles.output_driver, nsi::OUTPUT_DRIVER, None);
        ctx.connect(
            &handles.output_driver,
            None,
            &handles.output_layer,
            "outputdrivers",
            None,
        );
        let write_callback = nsi::output::WriteCallback::<f32>::new(
            move |_name: &str,
                  _width: usize,
                  _height: usize,
                  x_min: usize,
                  x_max_plus_one: usize,
                  y_min: usize,
                  y_max_plus_one: usize,
                  pixel_format: &nsi::output::PixelFormat,
                  bucket_data: &[f32]| {
                let channels = pixel_format.channels();
                if channels < 3 {
                    return nsi::output::Error::None;
                }
                let mut image = overlay_image.lock();
                let width = image.width;
                let height = image.height;
                let x_end = x_max_plus_one.min(width);
                let y_end = y_max_plus_one.min(height);
                let x_start = x_min.min(width);
                image
                    .pixels
                    .par_chunks_exact_mut(width * 4)
                    .enumerate()
                    .skip(y_min.min(height))
                    .take(y_end.saturating_sub(y_min))
                    .for_each(|(y, row)| {
                        row[x_start * 4..x_end * 4]
                            .as_chunks_mut::<4>()
                            .0
                            .iter_mut()
                            .enumerate()
                            .for_each(|(column, pixel)| {
                                let x = x_start + column;
                                let source = ((y - y_min)
                                    * (x_max_plus_one - x_min)
                                    + (x - x_min))
                                    * channels;
                                if source + channels > bucket_data.len() {
                                    return;
                                }
                                let alpha = if channels >= 4 {
                                    bucket_data[source + 3]
                                } else {
                                    1.0
                                };
                                let to_srgb = |linear: f32| {
                                    let straight = if alpha > 0.0 {
                                        linear / alpha
                                    } else {
                                        0.0
                                    };
                                    (straight.clamp(0.0, 1.0).powf(1.0 / 2.2)
                                        * 255.0)
                                        as u8
                                };
                                pixel.copy_from_slice(&[
                                    to_srgb(bucket_data[source]),
                                    to_srgb(bucket_data[source + 1]),
                                    to_srgb(bucket_data[source + 2]),
                                    (alpha.clamp(0.0, 1.0) * 255.0) as u8,
                                ]);
                            });
                    });
                image.dirty = true;
                image.first_dirty_row = image.first_dirty_row.min(y_min);
                image.last_dirty_row = image.last_dirty_row.max(y_end);
                nsi::output::Error::None
            },
        );
        let open_callback = nsi::output::OpenCallback::new(
            |_name, _width, _height, _format| nsi::output::Error::None,
        );
        let finish_callback = nsi::output::FinishCallback::new(
            |_name, _width, _height, _format| nsi::output::Error::None,
        );
        ctx.set_attribute(
            &handles.output_driver,
            &[
                nsi::string!("drivername", nsi::output::FERRIS_F32),
                nsi::string!("imagefilename", "mstpv_nsi_overlay"),
                nsi::callback!("callback.open", open_callback),
                nsi::callback!("callback.write", write_callback),
                nsi::callback!("callback.finish", finish_callback),
            ],
        );

        // Shared shader (dlPrincipled) used by all BRep surfaces.
        ctx.create(&handles.shader, nsi::SHADER, None);
        ctx.set_attribute(
            &handles.shader,
            &[
                nsi::string!("shaderfilename", "${DELIGHT}/osl/dlPrincipled"),
                nsi::color!("i_color", &[0.8, 0.8, 0.8]),
                nsi::f32!("roughness", 0.3),
            ],
        );

        // Environment light + attribute group. Mirrors akatela's setup:
        // wooden_lounge_1k.tdl env map (copied into our assets/),
        // visibility.camera = 0 so the environment lights the model
        // without showing up as a background.
        ctx.create(&handles.environment, nsi::ENVIRONMENT, None);
        ctx.connect(&handles.environment, None, nsi::ROOT, "objects", None);
        ctx.create(&handles.env_attrib, nsi::ATTRIBUTES, None);
        ctx.connect(
            &handles.env_attrib,
            None,
            &handles.environment,
            "geometryattributes",
            None,
        );
        ctx.set_attribute(
            &handles.env_attrib,
            &[nsi::i32!("visibility.camera", 0)],
        );
        ctx.create(&handles.env_shader, nsi::SHADER, None);
        ctx.connect(
            &handles.env_shader,
            None,
            &handles.env_attrib,
            "surfaceshader",
            None,
        );
        // Resolve the env-map path relative to the crate's manifest
        // directory so the binary works regardless of CWD.
        let env_map_path = format!(
            "{}/assets/wooden_lounge_1k.tdl",
            env!("CARGO_MANIFEST_DIR")
        );
        ctx.set_attribute(
            &handles.env_shader,
            &[
                nsi::string!(
                    "shaderfilename",
                    "${DELIGHT}/osl/environmentLight"
                ),
                nsi::string!("image", env_map_path.as_str()),
                nsi::f32!("intensity", 1.0),
            ],
        );

        Ok(())
    }

    /// Push or refresh one shell's BRep into NSI.
    ///
    /// `key` is a stable identifier. Calling again with the same key updates
    /// existing face nodes in place. Faces no longer present are hidden, not
    /// deleted.
    pub fn update_shell_brep(
        &self,
        key: &str,
        shell_data: &CompressedShellData,
        model_matrix: Mat4,
    ) {
        let surfaces = brep::shell_data_to_nsi_surfaces(shell_data);
        if surfaces.is_empty() {
            self.hide_shell_faces(key);
            return;
        }

        let mut handles = self.brep_handles.lock();
        let entry = handles.entry(key.to_string()).or_insert_with(|| {
            let new_handles = DynamicBrepHandles {
                xform: format!("mstpv_brep_xform_{}", key),
                surfaces: Vec::new(),
            };
            self.context
                .create(&new_handles.xform, nsi::TRANSFORM, None);
            self.context.connect(
                &new_handles.xform,
                None,
                nsi::ROOT,
                "objects",
                None,
            );
            new_handles
        });

        let nsi_matrix = mat4_to_nsi(model_matrix);
        self.context.set_attribute(
            &entry.xform,
            &[nsi::matrix_f64!("transformationmatrix", &nsi_matrix)],
        );

        let active_faces: HashSet<usize> =
            surfaces.iter().map(|surface| surface.face_index).collect();
        entry
            .surfaces
            .iter()
            .filter(|handles| !active_faces.contains(&handles.face_index))
            .for_each(|handles| {
                self.set_attribute_node_visibility(&handles.attrib, false);
            });

        let xform = entry.xform.clone();
        let mut pending_surfaces = Vec::new();
        surfaces.iter().for_each(|surface| {
            let handles = if let Some(handles) = entry
                .surfaces
                .iter()
                .find(|handles| handles.face_index == surface.face_index)
            {
                handles.clone()
            } else {
                let surface_handle = format!(
                    "mstpv_brep_surface_{}_{}",
                    key, surface.face_index
                );
                let attrib_handle =
                    format!("mstpv_brep_attrib_{}_{}", key, surface.face_index);
                self.context.create(&surface_handle, nsi::NURBS, None);
                self.context.connect(
                    &surface_handle,
                    None,
                    &xform,
                    "objects",
                    None,
                );
                self.context.create(&attrib_handle, nsi::ATTRIBUTES, None);
                self.context.connect(
                    &self.handles.shader,
                    None,
                    &attrib_handle,
                    "surfaceshader",
                    None,
                );
                self.context.connect(
                    &attrib_handle,
                    None,
                    &surface_handle,
                    "geometryattributes",
                    None,
                );
                let handles = DynamicBrepSurfaceHandles {
                    face_index: surface.face_index,
                    surface: surface_handle,
                    attrib: attrib_handle,
                };
                entry.surfaces.push(handles.clone());
                handles
            };
            pending_surfaces.push((handles.surface, surface));
        });

        pending_surfaces
            .iter()
            .for_each(|(surface_handle, surface)| {
                self.set_brep_surface_attributes(surface_handle, surface);
            });

        self.send_command(NsiCommand::Synchronize);
    }

    fn send_command(&self, command: NsiCommand) {
        if let Some(tx) = self.command_tx.as_ref() {
            let _ = tx.send(command);
        }
    }

    fn set_brep_surface_attributes(
        &self,
        surface_handle: &str,
        surface: &brep::NsiBrepSurfaceData,
    ) {
        // One-shot dump of the first surface's data so we can sanity-check
        // what we're sending into NSI when something looks wrong in
        // idisplay. Disabled after the first surface to keep logs clean.
        static FIRST_SURFACE_DUMPED: OnceLock<()> = OnceLock::new();
        if FIRST_SURFACE_DUMPED.set(()).is_ok() {
            log::info!(
                "NSI brep first-surface dump: handle={surface_handle} \
                 nu={} nv={} uorder={} vorder={}",
                surface.nu,
                surface.nv,
                surface.uorder,
                surface.vorder,
            );
            log::info!(
                "  uknot ({}): {:?}",
                surface.uknot.len(),
                surface.uknot
            );
            log::info!(
                "  vknot ({}): {:?}",
                surface.vknot.len(),
                surface.vknot
            );
            log::info!(
                "  domain: u=[{}..{}] v=[{}..{}]",
                surface.umin,
                surface.umax,
                surface.vmin,
                surface.vmax
            );
            log::info!(
                "  Pw ({} CVs): first 4 = {:?}",
                surface.pw.len(),
                &surface.pw[..surface.pw.len().min(4)]
            );
            if let Some(trims) = &surface.trims {
                log::info!(
                    "  trims: nloops={} ncurves={:?} n={:?} order={:?} sense={:?} sampled_fallbacks={}",
                    trims.nloops,
                    trims.ncurves,
                    trims.n,
                    trims.order,
                    trims.sense,
                    surface.sampled_trim_fallback_count
                );
                log::info!(
                    "  trim u ({}): {:?}",
                    trims.u.len(),
                    &trims.u[..trims.u.len().min(8)]
                );
                log::info!(
                    "  trim v ({}): {:?}",
                    trims.v.len(),
                    &trims.v[..trims.v.len().min(8)]
                );
                log::info!(
                    "  trim w ({}): {:?}",
                    trims.w.len(),
                    &trims.w[..trims.w.len().min(8)]
                );
            } else {
                log::info!("  trims: <none>");
            }
        }

        self.context.set_attribute(
            surface_handle,
            &[
                nsi::i32!("nu", surface.nu),
                nsi::i32!("nv", surface.nv),
                nsi::i32!("uorder", surface.uorder),
                nsi::i32!("vorder", surface.vorder),
                nsi::f32_slice!("uknot", &surface.uknot),
                nsi::f32_slice!("vknot", &surface.vknot),
                nsi::f32!("umin", surface.umin),
                nsi::f32!("umax", surface.umax),
                nsi::f32!("vmin", surface.vmin),
                nsi::f32!("vmax", surface.vmax),
                nsi::point4_f32_slice!("Pw", &surface.pw),
            ],
        );

        if let Some(trims) = &surface.trims {
            self.context.set_attribute(
                surface_handle,
                &[
                    nsi::i32!("trimcurves.nloops", trims.nloops),
                    nsi::i32_slice!("trimcurves.ncurves", &trims.ncurves),
                    nsi::i32_slice!("trimcurves.n", &trims.n),
                    nsi::i32_slice!("trimcurves.order", &trims.order),
                    nsi::f32_slice!("trimcurves.knot", &trims.knot),
                    nsi::f32_slice!("trimcurves.min", &trims.min),
                    nsi::f32_slice!("trimcurves.max", &trims.max),
                    nsi::f32_slice!("trimcurves.u", &trims.u),
                    nsi::f32_slice!("trimcurves.v", &trims.v),
                    nsi::f32_slice!("trimcurves.w", &trims.w),
                    nsi::i32_slice!("trimcurves.sense", &trims.sense),
                ],
            );
        }
    }

    fn set_attribute_node_visibility(&self, attrib: &str, visible: bool) {
        let value = i32::from(visible);
        self.context
            .set_attribute(attrib, &[nsi::i32!("visibility", value)]);
    }

    /// Toggle all visibility attributes for a batch of face attributes nodes.
    pub fn set_face_visibilities<'a>(
        &self,
        updates: impl IntoIterator<Item = (&'a str, usize, bool)>,
    ) {
        let attribs: Vec<(String, bool)> = {
            let handles = self.brep_handles.lock();
            updates
                .into_iter()
                .filter_map(|(key, face_index, visible)| {
                    handles.get(key).and_then(|entry| {
                        entry
                            .surfaces
                            .iter()
                            .find(|surface| surface.face_index == face_index)
                            .map(|surface| (surface.attrib.clone(), visible))
                    })
                })
                .collect()
        };
        if !attribs.is_empty() {
            attribs.iter().for_each(|(attrib, visible)| {
                self.set_attribute_node_visibility(attrib, *visible);
            });
            self.send_command(NsiCommand::Synchronize);
        }
    }

    /// Hide every face currently tracked for one shell.
    pub fn hide_shell_faces(&self, key: &str) {
        let attribs: Vec<String> = {
            let handles = self.brep_handles.lock();
            handles
                .get(key)
                .map(|entry| {
                    entry
                        .surfaces
                        .iter()
                        .map(|surface| surface.attrib.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        if !attribs.is_empty() {
            attribs.iter().for_each(|attrib| {
                self.set_attribute_node_visibility(attrib, false);
            });
            self.send_command(NsiCommand::Synchronize);
        }
    }

    /// Hide every BRep shell that is not in `keep`.
    pub fn hide_unretained_shells(&self, keep: &HashSet<String>) {
        let stale: Vec<String> = {
            let handles = self.brep_handles.lock();
            handles
                .keys()
                .filter(|key| !keep.contains(*key))
                .cloned()
                .collect()
        };
        stale.iter().for_each(|key| self.hide_shell_faces(key));
    }

    /// Push the current camera state. Skips the round-trip when nothing
    /// actually changed.
    pub fn update_camera(
        &self,
        view_matrix: Mat4,
        fov_y_degrees: f32,
        near_clip: f32,
        far_clip: f32,
    ) {
        let current_matrix = view_matrix.to_cols_array();
        let current_clip = [near_clip, far_clip];

        let matrix_changed =
            *self.last_view_matrix.lock() != Some(current_matrix);
        let fov_changed = (fov_y_degrees - *self.last_fov.lock()).abs() > 0.001;
        let clip_changed = *self.last_clip_range.lock() != Some(current_clip);

        if !matrix_changed && !fov_changed && !clip_changed {
            return;
        }

        *self.last_view_matrix.lock() = Some(current_matrix);
        *self.last_fov.lock() = fov_y_degrees;
        *self.last_clip_range.lock() = Some(current_clip);

        let cam_to_world = view_matrix.inverse();
        let nsi_matrix = mat4_to_nsi(cam_to_world);

        self.context.set_attribute(
            &self.handles.camera_xform,
            &[nsi::matrix_f64!("transformationmatrix", &nsi_matrix)],
        );
        // `clippingrange` isn't in the documented perspectivecamera
        // attribute set and 3Delight rejects every encoding we try
        // ("E6007 wrong type for attribute"). Use the renderer's default
        // clipping until we find the right node/type for it.
        let _ = (near_clip, far_clip);
        self.context.set_attribute(
            &self.handles.camera,
            &[nsi::f32!("fov", fov_y_degrees)],
        );

        self.send_command(NsiCommand::Synchronize);
    }

    pub fn start(&self) {
        let mut state = self.is_rendering.lock();
        if *state {
            return;
        }
        self.send_command(NsiCommand::Start);
        *state = true;
    }

    pub fn stop(&self) {
        let mut state = self.is_rendering.lock();
        if !*state {
            return;
        }
        self.send_command(NsiCommand::Stop);
        *state = false;
    }

    /// Resize the callback image and matching NSI screen to the viewport.
    pub fn set_overlay_resolution(&self, width: u32, height: u32) {
        let width = width.max(1) as usize;
        let height = height.max(1) as usize;
        let mut image = self.overlay_image.lock();
        if image.width == width && image.height == height {
            return;
        }
        image.width = width;
        image.height = height;
        image.pixels = vec![0; width * height * 4];
        image.dirty = false;
        image.first_dirty_row = usize::MAX;
        image.last_dirty_row = 0;
        drop(image);

        self.context.set_attribute(
            &self.handles.screen,
            &[
                nsi::i32_slice!("resolution", &[width as i32, height as i32])
                    .array_len(NonZeroUsize::new(2).unwrap()),
            ],
        );
        self.send_command(NsiCommand::Synchronize);
    }

    /// Return changed image rows once after a 3Delight callback.
    pub fn take_overlay_rows(
        &self,
    ) -> Option<(Vec<u8>, usize, usize, usize, usize)> {
        let mut image = self.overlay_image.lock();
        if !image.dirty {
            return None;
        }
        image.dirty = false;
        let first = image.first_dirty_row.min(image.height);
        let last = image.last_dirty_row.clamp(first, image.height);
        image.first_dirty_row = usize::MAX;
        image.last_dirty_row = 0;
        let row_count = last.saturating_sub(first);
        (row_count > 0).then(|| {
            let stride = image.width * 4;
            (
                image.pixels[first * stride..last * stride].to_vec(),
                image.width,
                image.height,
                first,
                row_count,
            )
        })
    }

    /// Return a full callback-image snapshot for first or resized textures.
    pub fn overlay_image(&self) -> (Vec<u8>, usize, usize) {
        let image = self.overlay_image.lock();
        (image.pixels.clone(), image.width, image.height)
    }
}

#[cfg(feature = "nsi-render")]
impl Drop for NsiRenderState {
    fn drop(&mut self) {
        self.send_command(NsiCommand::Stop);
        // Drop our sender so the worker's `recv()` returns Err and the
        // loop exits cleanly.
        self.command_tx = None;
        if let Some(handle) = self.render_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(feature = "nsi-render")]
fn render_thread_main(ctx: nsi::Context<'static>, rx: Receiver<NsiCommand>) {
    let mut is_rendering = false;

    while let Ok(command) = rx.recv() {
        match command {
            NsiCommand::Start => {
                if !is_rendering {
                    log::info!(
                        "NSI: render start (interactive + progressive, viewport callback)"
                    );
                    ctx.render_control(
                        nsi::Action::Start,
                        Some(&[
                            nsi::i32!("interactive", 1),
                            nsi::i32!("progressive", 1),
                        ]),
                    );
                    ctx.render_control(nsi::Action::Synchronize, None);
                    is_rendering = true;
                }
            }
            NsiCommand::Stop => {
                if is_rendering {
                    log::trace!("NSI: render stop");
                    ctx.render_control(nsi::Action::Stop, None);
                    ctx.render_control(nsi::Action::Wait, None);
                    is_rendering = false;
                }
            }
            NsiCommand::Synchronize => {
                if is_rendering {
                    ctx.render_control(nsi::Action::Synchronize, None);
                }
            }
        }
    }

    if is_rendering {
        ctx.render_control(nsi::Action::Stop, None);
        ctx.render_control(nsi::Action::Wait, None);
    }
}

fn mat4_to_nsi(mat: Mat4) -> [f64; 16] {
    // glam stores column-major; NSI expects row-major. Our column-major
    // flattening of glam M is byte-equivalent to row-major flattening
    // of NSI M because glam uses column-vector convention while NSI
    // (RenderMan-style) uses row-vector convention — they're transposes
    // of each other and the storage transposes back.
    mat.to_cols_array().map(|v| v as f64)
}

#[cfg(all(test, feature = "nsi-render"))]
mod tests {
    use super::{NsiHandles, NsiRenderState};
    use std::path::PathBuf;

    /// Path of the recorded ɴsɪ call stream for the singleton scene
    /// nodes.
    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nsi_scene_nodes.nsi")
    }

    /// Drop 3Delight's banner and timestamp comments so the comparison
    /// is about the calls we made, not about when we made them.
    fn calls_only(stream: &str) -> Vec<String> {
        stream
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    /// Record `create_scene_nodes` into an ɴsɪ stream instead of
    /// rendering it.
    fn record_scene_nodes() -> Vec<String> {
        let out = std::env::temp_dir().join("mstpv_scene_nodes.nsi");
        let _ = std::fs::remove_file(&out);

        {
            let ctx = nsi::Context::new(Some(&[
                nsi::string!("type", "apistream"),
                nsi::string!("streamfilename", out.to_str().unwrap()),
                nsi::string!("streamformat", "nsi"),
            ]))
            .expect("could not create an apistream ɴsɪ context");

            NsiRenderState::create_scene_nodes(
                &ctx,
                &NsiHandles::default(),
                NsiRenderState::new_overlay_image(),
            )
            .expect("create_scene_nodes failed");
        } // Context::drop calls NSIEnd, which flushes the stream.

        calls_only(&std::fs::read_to_string(&out).expect("stream written"))
    }

    /// The singleton scene setup must emit exactly the same ɴsɪ calls,
    /// in the same order, as the recorded fixture.
    ///
    /// This is the regression gate for making `create_scene_nodes`
    /// generic over `nsi_trait::Nsi`: the refactor must not add, drop
    /// or reorder a single call. Handles are fixed literals, so the
    /// stream is byte-reproducible.
    ///
    /// Regenerate deliberately with `MSTPV_BLESS_NSI=1`.
    #[test]
    fn scene_nodes_emit_the_expected_nsi_calls() {
        let actual = record_scene_nodes();
        let path = fixture_path();

        if std::env::var_os("MSTPV_BLESS_NSI").is_some() || !path.exists() {
            std::fs::write(&path, actual.join("\n") + "\n")
                .expect("could not write fixture");
            eprintln!("blessed fixture at {}", path.display());
            return;
        }

        let expected = calls_only(
            &std::fs::read_to_string(&path).expect("fixture readable"),
        );
        assert_eq!(expected, actual, "ɴsɪ call stream changed");
    }
}
