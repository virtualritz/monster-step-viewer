mod browser;
mod icons;
mod persistence;
mod scene;
mod state;
mod ui;
mod viewer_material;

use bevy::{
    log::LogPlugin,
    prelude::*,
    window::{PresentMode, WindowTheme},
    winit::WinitSettings,
};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass, EguiUserTextures};
use bevy_panorbit_camera::PanOrbitCameraPlugin;
use state::{AppMode, BrowserState, ViewerState};
use std::{
    env,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

fn main() {
    let cli_path = env::args().nth(1).map(PathBuf::from);
    let settings = persistence::load_settings();
    let initial_path = cli_path.or(settings.last_file_path.clone());

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

    App::new()
        .insert_resource(ViewerState {
            pending_path: initial_path,
            panel_width: settings.panel_width,
            right_panel_width: settings.right_panel_width,
            show_random_colors: settings.show_random_colors,
            show_bounding_box: settings.show_bounding_box,
            show_wireframe: settings.show_wireframe,
            show_edges: settings.show_edges,
            tessellation_factor: settings.tessellation_factor,
            applied_tessellation_factor: settings.tessellation_factor,
            mode: settings.mode,
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
                .set(LogPlugin {
                    filter: "info,wgpu_core=warn,wgpu_hal=warn".into(),
                    level: bevy::log::Level::INFO,
                    ..Default::default()
                }),
        )
        .add_plugins(bevy::pbr::MaterialPlugin::<viewer_material::ViewerMaterial>::default())
        .add_plugins(EguiPlugin::default())
        .add_plugins(MeshPickingPlugin)
        .add_plugins(PanOrbitCameraPlugin)
        .insert_resource(WinitSettings::desktop_app())
        .add_systems(Startup, scene::setup_scene)
        .add_systems(Startup, setup_browser_render_slots)
        .add_systems(Update, scene::process_load_requests)
        .add_systems(Update, scene::rebuild_meshes_on_toggle)
        .add_systems(EguiPrimaryContextPass, ui::ui_system)
        .add_systems(Update, scene::normalize_scene_and_setup_camera)
        .add_systems(Update, scene::apply_face_visibility)
        .add_systems(Update, scene::apply_selection_highlight)
        .add_systems(Update, scene::disable_camera_when_egui_wants_input)
        .add_systems(Update, scene::draw_gizmos)
        .add_systems(Update, scene::retessellate_face)
        .add_systems(Update, persistence::auto_save_system)
        .add_observer(scene::on_mesh_click)
        .add_systems(
            Update,
            browser::update_turntable_system.run_if(in_browser_mode),
        )
        .add_systems(
            Update,
            browser::manage_render_slots_system.run_if(in_browser_mode),
        )
        .run();
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
