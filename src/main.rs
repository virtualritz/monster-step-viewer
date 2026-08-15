pub use monster_step_viewer::{HashMap, HashSet};

mod browser;
mod icons;
mod persistence;
mod scene;
mod state;
mod ui;
mod viewer_material;

#[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
mod nsi_overlay;
#[cfg(all(
    any(feature = "nsi-render", feature = "nsi-export"),
    not(target_arch = "wasm32")
))]
mod nsi_render;

use bevy::{
    log::LogPlugin,
    prelude::*,
    window::{PresentMode, WindowTheme},
    winit::WinitSettings,
};
use bevy_editor_cam::{
    controller::MinimalEditorCamPlugin,
    input::{CameraPointerMap, EditorCamInputMessage},
    prelude::EditorCam,
};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass, EguiUserTextures};
use state::{
    AppMode, BrowserState, ClipPlaneDragState, ViewerState, ViewportClickGuard,
};
use std::{
    env,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

struct CliArgs {
    path: Option<PathBuf>,
    verbose: bool,
}

fn parse_cli_args() -> CliArgs {
    let mut path: Option<PathBuf> = None;
    let mut verbose = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "-v" | "--verbose" => verbose = true,
            "-h" | "--help" => {
                eprintln!(
                    "mstpv — STEP CAD viewer\n\nUSAGE:\n    mstpv [OPTIONS] [FILE]\n\nOPTIONS:\n    -v, --verbose   Enable INFO log messages (default: WARN+)\n    -h, --help      Show this help"
                );
                std::process::exit(0);
            }
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => {}
        }
    }
    CliArgs { path, verbose }
}

fn main() {
    let cli = parse_cli_args();
    let settings = persistence::load_settings();
    let initial_path = cli.path.or(settings.last_file_path.clone());

    let browser_root = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut initial_tree = browser::scan_subdirs(&browser_root);

    // Expand tree to the previously selected directory (and scan files only if
    // starting in browser mode).
    let mut initial_previews = Vec::new();
    if let Some(ref last_dir) = settings.last_browser_dir {
        browser::expand_tree_to_path(
            &mut initial_tree,
            &browser_root,
            last_dir,
        );
        if settings.mode == AppMode::Browser {
            initial_previews = browser::scan_step_files(last_dir);
        }
    }

    let mut app = App::new();
    app.insert_resource(ViewerState {
        pending_path: initial_path,
        panel_width: settings.panel_width,
        right_panel_width: settings.right_panel_width,
        show_random_colors: settings.show_random_colors,
        show_step_colors: settings.show_step_colors,
        show_bounding_box: settings.show_bounding_box,
        show_polygon_edges: settings.show_polygon_edges,
        show_wireframe: settings.show_wireframe,
        tessellation_factor: settings.tessellation_factor,
        applied_tessellation_factor: settings.tessellation_factor,
        meshing: settings.meshing,
        applied_meshing: settings.meshing,
        meshing_panel_expanded: settings.meshing_panel_expanded,
        up_axis: settings.up_axis,
        mode: settings.mode,
        clip_planes: settings.clip_planes,
        shading_mode: settings.shading_mode,
        ..Default::default()
    })
    .insert_resource(BrowserState {
        root: browser_root,
        tree: initial_tree,
        selected_dir: settings.last_browser_dir.clone(),
        previews: initial_previews,
        render_slots: Vec::new(),
        cancel_flag: Arc::new(AtomicBool::new(false)),
        preview_receiver: None,
        scroll_offset: 0.0,
        visible_rows: 4,
        grid_cols: 3,
        thumb_size: 200.0,
    })
    .insert_resource(persistence::SaveTimer::default())
    .insert_resource(ClipPlaneDragState::default())
    .insert_resource(ViewportClickGuard::default())
    .add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "Monster STEP Viewer".into(),
                    present_mode: PresentMode::AutoVsync,
                    fit_canvas_to_parent: true,
                    prevent_default_event_handling: false,
                    window_theme: Some(WindowTheme::Dark),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .set(if cli.verbose {
                LogPlugin {
                    filter: "info,wgpu_core=warn,wgpu_hal=warn".into(),
                    level: bevy::log::Level::INFO,
                    ..Default::default()
                }
            } else {
                LogPlugin {
                    filter: "warn".into(),
                    level: bevy::log::Level::WARN,
                    ..Default::default()
                }
            }),
    )
    .add_plugins(
        bevy::pbr::MaterialPlugin::<viewer_material::ViewerMaterial>::default(),
    )
    // SSAO plugin is already in DefaultPlugins (via PbrPlugin); the
    // `ScreenSpaceAmbientOcclusion` component on the camera is enough.
    .add_plugins(EguiPlugin::default())
    .add_plugins(MeshPickingPlugin)
    // Print fps/frame time periodically so we can read the actual frame
    // budget instead of guessing.
    .add_plugins(bevy::diagnostic::FrameTimeDiagnosticsPlugin::default())
    .add_plugins(bevy::diagnostic::LogDiagnosticsPlugin {
        wait_duration: Duration::from_secs(2),
        ..Default::default()
    })
    .add_plugins(MinimalEditorCamPlugin)
    .add_message::<EditorCamInputMessage>()
    .init_resource::<CameraPointerMap>()
    .add_systems(
        PreUpdate,
        (
            scene::editor_cam_mouse_inputs,
            EditorCamInputMessage::receive_messages,
            EditorCamInputMessage::send_pointer_inputs,
        )
            .chain()
            .after(bevy::picking::PickingSystems::Last)
            .before(EditorCam::update_camera_positions),
    )
    .add_systems(
        PreUpdate,
        scene::gate_picking_on_primary_button
            .after(bevy::input::InputSystems)
            .before(bevy::picking::PickingSystems::Backend),
    )
    .insert_resource(WinitSettings::desktop_app())
    .add_systems(Startup, scene::setup_scene)
    .add_systems(Startup, scene::configure_gizmos)
    .add_systems(Startup, viewer_material::setup_matcap_texture)
    .add_systems(Startup, viewer_material::setup_material_palette)
    .add_systems(Startup, scene::setup_polygon_edges_material)
    .add_systems(Startup, scene::setup_isoparams_material)
    .add_systems(Startup, setup_browser_render_slots)
    .add_systems(Update, scene::process_load_requests)
    .add_systems(Update, scene::rebuild_meshes_on_toggle)
    .add_systems(Update, scene::apply_shading_mode)
    .add_systems(Update, scene::rebuild_normals)
    .add_systems(EguiPrimaryContextPass, ui::ui_system)
    .add_systems(Update, scene::normalize_scene_and_setup_camera)
    .add_systems(Update, scene::apply_face_visibility)
    .add_systems(Update, scene::apply_polygon_edges_visibility)
    .add_systems(Update, scene::apply_isoparams_visibility)
    .add_systems(Update, scene::apply_selection_highlight)
    .add_systems(Update, scene::update_face_state_buffer)
    .add_systems(Update, scene::disable_camera_when_egui_wants_input)
    .add_systems(Update, scene::handle_view_shortcuts)
    .add_systems(Update, scene::apply_up_axis_change)
    .add_systems(Update, scene::clear_selection_on_empty_click)
    .add_systems(Update, scene::draw_gizmos)
    .add_systems(Update, scene::retessellate_face)
    .add_systems(Update, scene::update_clip_plane_uniforms)
    .add_systems(Update, scene::manage_clip_plane_visuals)
    .add_systems(Update, persistence::auto_save_system)
    .add_observer(scene::on_mesh_click)
    .add_observer(scene::on_clip_plane_drag_start)
    .add_observer(scene::on_clip_plane_drag)
    .add_observer(scene::on_clip_plane_drag_end)
    .add_systems(
        Update,
        browser::update_turntable_system.run_if(in_browser_mode),
    )
    .add_systems(
        Update,
        browser::manage_render_slots_system.run_if(in_browser_mode),
    );

    #[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
    app.add_plugins(nsi_overlay::NsiOverlayPlugin);

    app.run();
}

fn in_browser_mode(state: Res<ViewerState>) -> bool {
    state.mode == AppMode::Browser
}

fn setup_browser_render_slots(
    mut browser: ResMut<BrowserState>,
    mut images: ResMut<Assets<Image>>,
    mut egui_textures: ResMut<EguiUserTextures>,
    state: Res<ViewerState>,
) {
    browser.render_slots =
        browser::setup_render_slots(&mut images, &mut egui_textures);
    // Start loading previews only if starting in browser mode.
    if state.mode == AppMode::Browser && !browser.previews.is_empty() {
        browser::start_preview_loads(&mut browser);
    }
}
