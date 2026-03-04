mod scene;
mod state;
mod ui;

use bevy::{
    log::LogPlugin,
    prelude::*,
    window::{PresentMode, WindowTheme},
    winit::WinitSettings,
};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};
use bevy_panorbit_camera::PanOrbitCameraPlugin;
use state::ViewerState;
use std::env;
use std::path::PathBuf;

fn main() {
    let cli_path = env::args().nth(1).map(PathBuf::from);

    App::new()
        .insert_resource(ViewerState {
            pending_path: cli_path.clone(),
            ..Default::default()
        })
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "STEP Viewer (Bevy + egui)".into(),
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
        .add_plugins(EguiPlugin::default())
        .add_plugins(PanOrbitCameraPlugin)
        .insert_resource(WinitSettings::desktop_app())
        .add_systems(Startup, scene::setup_scene)
        .add_systems(Update, scene::process_load_requests)
        .add_systems(Update, scene::rebuild_meshes_on_toggle)
        .add_systems(EguiPrimaryContextPass, ui::ui_system)
        .add_systems(Update, scene::normalize_scene_and_setup_camera)
        .add_systems(Update, scene::apply_face_visibility)
        .add_systems(Update, scene::disable_camera_when_egui_wants_input)
        .add_systems(Update, scene::draw_gizmos)
        .run();
}
