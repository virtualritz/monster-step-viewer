#[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
use crate::icons::ICON_COUNTER_3;
#[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
use crate::nsi_overlay::NsiOverlayState;
#[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
use crate::nsi_render::{NsiFileExportOptions, export_scene_to_nsi_file};
use crate::{
    browser::{
        poll_preview_loads, refresh_tree_entry, scan_step_files, scan_subdirs,
        start_preview_loads,
    },
    icons::{
        ICON_BOUNDING_BOX, ICON_CASINO, ICON_DETAILS, ICON_GRID_ON,
        ICON_PALETTE, ICON_WIREFRAME, configure_fonts, icon_text,
    },
    state::{
        AppMode, BrowserState, DirectoryEntry, MainCamera, MeshingMode,
        MeshingOptions, MeshingPrimitiveMode, PreviewStatus, Selection,
        ShadingMode, ViewerState,
    },
};
use bevy::{
    app::AppExit,
    camera::Viewport,
    ecs::system::SystemParam,
    prelude::{MessageWriter, *},
};
use bevy_egui::{EguiContexts, egui};
#[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
use glam::Mat4 as NsiMat4;
use monster_step_viewer::Parameter;
#[cfg(not(target_arch = "wasm32"))]
use rfd::FileDialog;
use std::{cell::Cell, path::PathBuf};

const TOOLBAR_ICON_BUTTON_SIZE: f32 = 26.0;

type MainCameraViewportQuery<'w, 's> =
    Query<'w, 's, &'static mut Camera, With<MainCamera>>;
type WindowQuery<'w, 's> = Query<'w, 's, &'static Window>;

#[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
type MainCameraExportQuery<'w, 's> = Query<
    'w,
    's,
    (&'static GlobalTransform, &'static Projection),
    With<MainCamera>,
>;

#[derive(SystemParam)]
pub(crate) struct MainCameraQueries<'w, 's> {
    viewport: MainCameraViewportQuery<'w, 's>,
    #[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
    export: MainCameraExportQuery<'w, 's>,
}

#[derive(SystemParam)]
pub(crate) struct UiSystemParams<'w, 's> {
    windows: WindowQuery<'w, 's>,
    camera_queries: MainCameraQueries<'w, 's>,
    #[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
    nsi_overlay: ResMut<'w, NsiOverlayState>,
}

fn toolbar_icon_toggle(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
) -> egui::Response {
    ui.add_sized(
        egui::vec2(TOOLBAR_ICON_BUTTON_SIZE, TOOLBAR_ICON_BUTTON_SIZE),
        egui::Button::selectable(selected, icon_text(icon)),
    )
}

#[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
fn nsi_render_toolbar_toggle(
    ui: &mut egui::Ui,
    overlay: &mut NsiOverlayState,
) -> egui::Response {
    let response = ui.add_enabled(
        overlay.available,
        egui::Button::selectable(overlay.enabled, icon_text(ICON_COUNTER_3))
            .min_size(egui::vec2(
                TOOLBAR_ICON_BUTTON_SIZE,
                TOOLBAR_ICON_BUTTON_SIZE,
            )),
    );
    if !overlay.available {
        response.on_hover_text(
            "3Delight not detected. Set `DELIGHT` or install it at `/usr/local/3delight`.",
        )
    } else {
        if response.clicked() {
            if overlay.enabled {
                if let Some(render) = overlay.render.as_ref() {
                    render.stop();
                }
                overlay.enabled = false;
            } else {
                overlay.enabled = true;
                if overlay.render.is_none() {
                    overlay.init_requested = true;
                } else if let Some(render) = overlay.render.as_ref() {
                    render.start();
                    overlay.last_pushed_scene_ptr = 0;
                }
            }
        }
        response.on_hover_text("3Delight NSI render overlay")
    }
}

/// Rollout panel exposing every monstertruck tessellation knob. Lives at
/// the bottom of the left SidePanel; when collapsed it's a single-row
/// `CollapsingHeader`. Top of the open body holds a Quality/Custom
/// toggle — Quality leaves the knobs grayed out (the top-toolbar
/// tessellation slider drives the shape), Custom ungrays them.
///
/// Edits push a re-tessellation through `state.pending_retessellate` the
/// same way the toolbar slider does.
fn meshing_panel(ui: &mut egui::Ui, state: &mut ViewerState) {
    let mut requested_re_tess = false;

    let response = egui::CollapsingHeader::new("Meshing")
        .id_salt("meshing_panel")
        .default_open(state.meshing_panel_expanded)
        .show(ui, |ui| {
            // Mode toggle row.
            ui.horizontal(|ui| {
                let mut mode = state.meshing.mode;
                if ui
                    .add(egui::Button::selectable(
                        mode == MeshingMode::Quality,
                        "Quality",
                    ))
                    .clicked()
                {
                    mode = MeshingMode::Quality;
                }
                if ui
                    .add(egui::Button::selectable(
                        mode == MeshingMode::Custom,
                        "Custom",
                    ))
                    .clicked()
                {
                    mode = MeshingMode::Custom;
                }
                if mode != state.meshing.mode {
                    state.meshing.mode = mode;
                    state.settings_dirty = true;
                }
            });
            ui.separator();

            // Quality slider — always editable (mirrors the toolbar's
            // top-right slider so users can tune density without
            // hunting). Only this widget runs in Quality mode.
            ui.horizontal(|ui| {
                ui.label("Quality");
                let mut quality = -state.tessellation_factor.log10();
                let slider = ui.add(
                    egui::Slider::new(&mut quality, 2.0_f64..=5.0_f64)
                        .show_value(false)
                        .custom_formatter(|v, _| {
                            if v > 4.5 {
                                "Ultra".to_string()
                            } else if v > 3.8 {
                                "High".to_string()
                            } else if v > 3.0 {
                                "Medium".to_string()
                            } else {
                                "Low".to_string()
                            }
                        }),
                );
                if slider.changed() {
                    state.tessellation_factor = 10_f64.powf(-quality);
                    state.settings_dirty = true;
                }
                if slider.drag_stopped()
                    && (state.tessellation_factor
                        - state.applied_tessellation_factor)
                        .abs()
                        > 1e-10
                {
                    requested_re_tess = true;
                }
            });

            // Everything below is the per-knob group — grayed in
            // Quality mode, live in Custom mode.
            let custom = state.meshing.mode == MeshingMode::Custom;
            ui.add_enabled_ui(custom, |ui| {
                ui.separator();
                egui::Grid::new("meshing_custom_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("Primitive");
                        egui::ComboBox::from_id_salt("meshing_prim_mode")
                            .selected_text(state.meshing.primitive_mode.label())
                            .show_ui(ui, |ui| {
                                for mode in MeshingPrimitiveMode::ALL {
                                    if ui
                                        .selectable_label(
                                            state.meshing.primitive_mode
                                                == mode,
                                            mode.label(),
                                        )
                                        .clicked()
                                    {
                                        state.meshing.primitive_mode = mode;
                                        state.settings_dirty = true;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("Search trials");
                        if ui
                            .add(
                                egui::DragValue::new(
                                    &mut state.meshing.search_trials,
                                )
                                .range(1..=10_000),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Plane tolerance");
                        if ui
                            .add(
                                egui::DragValue::new(
                                    &mut state.meshing.plane_tolerance,
                                )
                                .range(1.0e-6..=1.0)
                                .speed(0.001),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Score tolerance");
                        if ui
                            .add(
                                egui::DragValue::new(
                                    &mut state.meshing.score_tolerance,
                                )
                                .range(0.0..=10.0)
                                .speed(0.05),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Normal blend (°)");
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut state.meshing.normal_blend_angle_deg,
                                    0.0..=180.0,
                                )
                                .suffix("°"),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Min quad area");
                        if ui
                            .add(
                                egui::DragValue::new(
                                    &mut state.meshing.minimum_area,
                                )
                                .range(1.0e-15..=1.0)
                                .speed(1.0e-9),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Max corner (°)");
                        if ui
                            .add(
                                egui::Slider::new(
                                    &mut state.meshing.maximum_corner_angle_deg,
                                    0.0..=180.0,
                                )
                                .suffix("°"),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Iso samples/dir");
                        if ui
                            .add(
                                egui::DragValue::new(
                                    &mut state
                                        .meshing
                                        .iso_samples_per_direction,
                                )
                                .range(0..=64),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Iso segments/curve");
                        if ui
                            .add(
                                egui::DragValue::new(
                                    &mut state.meshing.iso_segments_per_curve,
                                )
                                .range(1..=256),
                            )
                            .changed()
                        {
                            state.settings_dirty = true;
                        }
                        ui.end_row();
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked()
                        && state.scene_data.is_some()
                        && state.loading_job.is_none()
                    {
                        state.pending_retessellate =
                            Some(state.tessellation_factor);
                    }
                    if ui.button("Reset").clicked() {
                        let current_mode = state.meshing.mode;
                        state.meshing = MeshingOptions {
                            mode: current_mode,
                            ..MeshingOptions::default()
                        };
                        state.settings_dirty = true;
                    }
                });
            });
        });

    // Mirror collapse state back to the persistent flag.
    let new_expanded = response.fully_open();
    if new_expanded != state.meshing_panel_expanded {
        state.meshing_panel_expanded = new_expanded;
        state.settings_dirty = true;
    }

    if requested_re_tess
        && state.scene_data.is_some()
        && state.loading_job.is_none()
    {
        state.pending_retessellate = Some(state.tessellation_factor);
    }
}

/// Clickable collapse/expand arrow drawn with the painter (font-independent).
fn collapse_arrow(ui: &mut egui::Ui, is_open: bool) -> egui::Response {
    let size = egui::vec2(ui.spacing().indent, ui.spacing().interact_size.y);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let center = rect.center();
        let half = 4.0;
        let color = ui.visuals().text_color();
        let points = if is_open {
            vec![
                egui::pos2(center.x - half, center.y - half * 0.5),
                egui::pos2(center.x + half, center.y - half * 0.5),
                egui::pos2(center.x, center.y + half * 0.5),
            ]
        } else {
            vec![
                egui::pos2(center.x - half * 0.5, center.y - half),
                egui::pos2(center.x + half * 0.5, center.y),
                egui::pos2(center.x - half * 0.5, center.y + half),
            ]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            color,
            egui::Stroke::NONE,
        ));
    }
    resp
}

/// Check if a Parameter is a leaf (non-recursive) value.
fn is_leaf_param(p: &Parameter) -> bool {
    matches!(
        p,
        Parameter::String(_)
            | Parameter::Integer(_)
            | Parameter::Real(_)
            | Parameter::Enumeration(_)
            | Parameter::Ref(_)
            | Parameter::NotProvided
            | Parameter::Omitted
    )
}

/// Format a leaf parameter as a compact string.
fn format_leaf_param(p: &Parameter) -> String {
    match p {
        Parameter::String(s) if s.is_empty() => "(empty)".to_string(),
        Parameter::String(s) => format!("'{}'", s),
        Parameter::Integer(n) => n.to_string(),
        Parameter::Real(x) => x.to_string(),
        Parameter::Enumeration(e) => format!(".{}.", e),
        Parameter::Ref(name) => format!("{:?}", name),
        Parameter::NotProvided => "$".to_string(),
        Parameter::Omitted => "*".to_string(),
        _ => "...".to_string(),
    }
}

/// Render a STEP Parameter as a flat tree in egui.
pub(crate) fn parameter_ui(
    ui: &mut egui::Ui,
    param: &Parameter,
    label: &str,
    depth: usize,
) {
    // Max depth guard.
    if depth > 4 {
        ui.label(format!("{}: ...", label));
        return;
    }

    match param {
        Parameter::List(items) if items.is_empty() => {
            ui.label(format!("{}: []", label));
        }
        Parameter::List(items)
            if items.len() <= 3 && items.iter().all(is_leaf_param) =>
        {
            let values: Vec<String> =
                items.iter().map(format_leaf_param).collect();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{}: [{}]", label, values.join(", ")));
            });
        }
        Parameter::List(items) => {
            egui::CollapsingHeader::new(format!("{} ({})", label, items.len()))
                .id_salt(format!("{}_{}", label, depth))
                .default_open(depth < 1)
                .show(ui, |ui| {
                    for (i, item) in items.iter().enumerate() {
                        parameter_ui(ui, item, &format!("[{}]", i), depth + 1);
                    }
                });
        }
        Parameter::Typed { keyword, parameter } if is_leaf_param(parameter) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{}: {} = {}",
                    label,
                    keyword,
                    format_leaf_param(parameter)
                ));
            });
        }
        Parameter::Typed { keyword, parameter } => {
            ui.label(format!("{}: {}", label, keyword));
            ui.indent(format!("{}_typed_{}", label, depth), |ui| {
                parameter_ui(ui, parameter, "value", depth + 1);
            });
        }
        Parameter::String(s) if s.is_empty() => {
            ui.label(format!("{}: (empty)", label));
        }
        Parameter::String(s) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{}:", label));
                ui.add(egui::Label::new(s.as_str()).wrap());
            });
        }
        Parameter::Integer(n) => {
            ui.label(format!("{}: {}", label, n));
        }
        Parameter::Real(x) => {
            ui.label(format!("{}: {}", label, x));
        }
        Parameter::Enumeration(e) => {
            ui.label(format!("{}: .{}.", label, e));
        }
        Parameter::Ref(name) => {
            ui.label(format!("{}: {:?}", label, name));
        }
        Parameter::NotProvided => {
            ui.label(format!("{}: $", label));
        }
        Parameter::Omitted => {
            ui.label(format!("{}: *", label));
        }
    }
}

pub(crate) fn ui_system(
    mut contexts: EguiContexts,
    mut state: ResMut<ViewerState>,
    mut browser: ResMut<BrowserState>,
    mut exit: MessageWriter<AppExit>,
    mut params: UiSystemParams,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Configure custom fonts once.
    if !state.fonts_configured {
        configure_fonts(ctx);
        state.fonts_configured = true;
    }

    // Poll preview loads.
    poll_preview_loads(&mut browser);

    // egui 0.35 panels attach to a `Ui`, not to the `Context`, so all panels
    // for this frame have to share one root `Ui` spanning the viewport.
    let mut root_ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("viewport"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );

    // Top bar: File menu on left, mode tabs on right.
    egui::Panel::top("menu").show(&mut root_ui, |ui| {
        ui.horizontal(|ui| {
            ui.style_mut().override_text_style = Some(egui::TextStyle::Heading);

            // File menu on the left.
            ui.menu_button("File", |ui| {
                ui.style_mut().override_text_style =
                    Some(egui::TextStyle::Body);
                if ui.button("Open STEP\u{2026}").clicked() {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(path) = FileDialog::new()
                        .add_filter("STEP", &["stp", "step"])
                        .pick_file()
                    {
                        state.pending_path = Some(path);
                        state.settings_dirty = true;
                    }

                    #[cfg(target_arch = "wasm32")]
                    {
                        state.error = Some(
                            "File open dialog is not supported on wasm"
                                .to_string(),
                        );
                    }
                    ui.close();
                }
                if ui.button("Open URL\u{2026}").clicked() {
                    state.show_url_dialog = true;
                    ui.close();
                }
                #[cfg(all(
                    feature = "nsi-export",
                    not(target_arch = "wasm32")
                ))]
                {
                    let export_response = ui.add_enabled(
                        state.scene_data.is_some(),
                        egui::Button::new("Export NSI..."),
                    );
                    if export_response.clicked() {
                        export_nsi_dialog(
                            &mut state,
                            &params.camera_queries.export,
                            &params.windows,
                        );
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    exit.write(AppExit::Success);
                    ui.close();
                }
            });

            if state.mode == AppMode::Browser {
                // Browser mode: show breadcrumb path.
                ui.separator();
                let display_path =
                    browser.selected_dir.as_deref().unwrap_or(&browser.root);
                let mut breadcrumb_nav: Option<PathBuf> = None;
                let mut accumulated = PathBuf::new();
                for (i, component) in display_path.components().enumerate() {
                    accumulated.push(component);
                    if i > 0 {
                        ui.label("/");
                    }
                    let name = component.as_os_str().to_string_lossy();
                    if ui
                        .add(
                            egui::Label::new(
                                egui::RichText::new(name.as_ref())
                                    .color(egui::Color32::LIGHT_GRAY),
                            )
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        breadcrumb_nav = Some(accumulated.clone());
                    }
                }
                if let Some(nav_to) = breadcrumb_nav
                    && browser.selected_dir.as_ref() != Some(&nav_to)
                {
                    browser.selected_dir = Some(nav_to.clone());
                    browser.previews = scan_step_files(&nav_to);
                    start_preview_loads(&mut browser);
                    let root = browser.root.clone();
                    crate::browser::expand_tree_to_path(
                        &mut browser.tree,
                        &root,
                        &nav_to,
                    );
                    state.settings_dirty = true;
                }
            }

            // Right-aligned tabs.
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    let viewer_selected = state.mode == AppMode::Viewer;
                    // Render in reverse order (right-to-left layout).
                    if ui
                        .selectable_label(!viewer_selected, "Browser")
                        .clicked()
                        && viewer_selected
                    {
                        state.mode = AppMode::Browser;
                        state.settings_dirty = true;
                        if browser.previews.is_empty()
                            && let Some(dir) = &browser.selected_dir
                        {
                            browser.previews = scan_step_files(dir);
                            start_preview_loads(&mut browser);
                        }
                    }
                    if ui.selectable_label(viewer_selected, "Viewer").clicked()
                        && !viewer_selected
                    {
                        state.mode = AppMode::Viewer;
                        state.settings_dirty = true;
                    }
                },
            );
        });
    });

    // --- Open URL dialog ---
    if state.show_url_dialog {
        let mut open = true;
        let mut submit = false;
        egui::Window::new("Open URL")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("URL:");
                    let resp = ui.text_edit_singleline(&mut state.url_input);
                    if resp.lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        submit = true;
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Fetch").clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        state.show_url_dialog = false;
                    }
                });
            });
        if submit {
            let url = state.url_input.trim().to_string();
            if !url.is_empty() {
                let (tx, rx) = std::sync::mpsc::channel();
                let request = ehttp::Request::get(&url);
                ehttp::fetch(request, move |result| {
                    let msg =
                        match result {
                            Ok(resp) if resp.ok => match resp.text() {
                                Some(text) => Ok(text.to_string()),
                                None => Err("Response is not valid UTF-8 text"
                                    .to_string()),
                            },
                            Ok(resp) => Err(format!(
                                "HTTP {} {}",
                                resp.status, resp.status_text
                            )),
                            Err(e) => Err(e),
                        };
                    let _ = tx.send(msg);
                });
                state.url_fetch = Some(parking_lot::Mutex::new(rx));
                state.show_url_dialog = false;
            }
        }
        if !open {
            state.show_url_dialog = false;
        }
    }

    // --- Poll URL fetch result ---
    if let Some(ref fetch) = state.url_fetch {
        let receiver = fetch.lock();
        if let Ok(result) = receiver.try_recv() {
            drop(receiver);
            state.url_fetch = None;
            match result {
                Ok(step_text) => {
                    state.pending_url_data = Some(step_text);
                }
                Err(e) => {
                    state.error = Some(format!("URL fetch failed: {e}"));
                }
            }
        }
    }

    match state.mode {
        AppMode::Viewer => {
            #[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
            {
                viewer_ui(
                    &mut root_ui,
                    &mut state,
                    &params.windows,
                    &mut params.camera_queries.viewport,
                    &mut params.nsi_overlay,
                )
            }
            #[cfg(not(all(
                feature = "nsi-render",
                not(target_arch = "wasm32")
            )))]
            {
                viewer_ui(
                    &mut root_ui,
                    &mut state,
                    &params.windows,
                    &mut params.camera_queries.viewport,
                )
            }
        }
        AppMode::Browser => browser_ui(&mut root_ui, &mut state, &mut browser),
    }

    // Ctrl+/- to zoom egui UI via egui's own zoom factor.
    let zoom_delta = ctx.input(|i| {
        if i.modifiers.command {
            if i.key_pressed(egui::Key::Plus)
                || i.key_pressed(egui::Key::Equals)
            {
                Some(0.1)
            } else if i.key_pressed(egui::Key::Minus) {
                Some(-0.1)
            } else if i.key_pressed(egui::Key::Num0) {
                Some(0.0) // sentinel for reset
            } else {
                None
            }
        } else {
            None
        }
    });
    if let Some(delta) = zoom_delta {
        let zoom = if delta == 0.0 {
            1.0
        } else {
            (ctx.zoom_factor() + delta as f32).clamp(0.5, 3.0)
        };
        ctx.set_zoom_factor(zoom);
    }

    // Allow escape to quit quickly on desktop.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        exit.write(AppExit::Success);
    }
}

#[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
fn export_nsi_dialog(
    state: &mut ViewerState,
    camera_query: &MainCameraExportQuery,
    windows: &Query<&Window>,
) {
    let Some(scene) = state.scene_data.as_ref() else {
        state.error =
            Some("Cannot export NSI: no STEP scene is loaded".to_string());
        return;
    };
    let Some(options) =
        current_nsi_export_options(state, camera_query, windows)
    else {
        state.error =
            Some("Cannot export NSI: main camera is unavailable".to_string());
        return;
    };
    let Some(path) = FileDialog::new()
        .add_filter("NSI", &["nsi"])
        .set_file_name(nsi_default_filename(state).as_str())
        .save_file()
    else {
        return;
    };

    match export_scene_to_nsi_file(&path, scene, options) {
        Ok(surface_count) => {
            log::info!(
                "Exported NSI scene with {surface_count} BRep surfaces to {}",
                path.display()
            );
        }
        Err(error) => {
            state.error = Some(format!("NSI export failed: {error}"));
        }
    }
}

#[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
fn current_nsi_export_options(
    state: &ViewerState,
    camera_query: &MainCameraExportQuery,
    windows: &Query<&Window>,
) -> Option<NsiFileExportOptions> {
    let (camera_transform, projection) = camera_query.single().ok()?;
    let view_matrix = NsiMat4::from_cols_array(
        &camera_transform.to_matrix().inverse().to_cols_array(),
    );
    let fov_y_degrees = match projection {
        Projection::Perspective(perspective) => perspective.fov.to_degrees(),
        Projection::Orthographic(_) | Projection::Custom(_) => 45.0,
    };
    let resolution = windows
        .single()
        .ok()
        .map(|window| [window.physical_width(), window.physical_height()])
        .unwrap_or([1024, 1024]);

    Some(NsiFileExportOptions {
        model_matrix: nsi_scene_model_matrix(state),
        view_matrix,
        fov_y_degrees,
        resolution,
    })
}

#[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
fn nsi_scene_model_matrix(state: &ViewerState) -> NsiMat4 {
    let matrix = Mat4::from_scale(Vec3::splat(state.scene_scale))
        * Mat4::from_translation(-state.scene_center);
    NsiMat4::from_cols_array(&matrix.to_cols_array())
}

#[cfg(all(feature = "nsi-export", not(target_arch = "wasm32")))]
fn nsi_default_filename(state: &ViewerState) -> String {
    let stem = state
        .loaded_path
        .as_ref()
        .and_then(|path| path.file_stem())
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("mstpv_export");
    format!("{stem}.nsi")
}

fn viewer_ui(
    root_ui: &mut egui::Ui,
    state: &mut ResMut<ViewerState>,
    windows: &Query<&Window>,
    camera_query: &mut MainCameraViewportQuery,
    #[cfg(all(feature = "nsi-render", not(target_arch = "wasm32")))]
    nsi_overlay: &mut ResMut<NsiOverlayState>,
) {
    let ctx = root_ui.ctx().clone();
    let ctx = &ctx;
    // Pre-seed panel width from settings on first frame (before egui has stored
    // state).
    let panel_id = egui::Id::new("entities");
    if ctx
        .data_mut(|d| {
            d.get_persisted::<egui::containers::panel::PanelState>(panel_id)
        })
        .is_none()
    {
        ctx.data_mut(|d| {
            d.insert_persisted(
                panel_id,
                egui::containers::panel::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(state.panel_width, 100.0),
                    ),
                },
            );
        });
    }
    let panel_response = egui::Panel::left(panel_id)
        .resizable(true)
        .size_range(100.0..=800.0)
        .show(root_ui, |ui| {
            // Claim the full panel width so the user can drag the resize
            // handle wider than the inner content (collapsed-by-default shell
            // headers are short; without this egui treats the content's tight
            // rect as the panel's "real" width and snaps back on every frame).
            ui.set_min_width(ui.available_width());
            if state.shells.is_empty() && state.loading_job.is_none() {
                ui.label("Load a STEP file to see hierarchy");
            } else if state.shells.is_empty() {
                ui.label("Loading...");
            } else {
                let vis_changed = Cell::new(false);
                let edge_vis_changed = Cell::new(false);
                let shell_vis_changes: Cell<Vec<(usize, bool)>> = Cell::new(Vec::new());
                let face_vis_changes: Cell<Vec<(usize, bool)>> = Cell::new(Vec::new());
                let edge_vis_changes: Cell<Vec<(usize, bool)>> = Cell::new(Vec::new());
                let loop_trim_changes: Cell<Vec<(usize, bool)>> = Cell::new(Vec::new());
                let current_selection = state.selection;
                let viewport_selected = state.selection_from_viewport;
                state.selection_from_viewport = false;
                // None = no change, Some(x) = set selection to x
                let new_selection: Cell<Option<Option<Selection>>> = Cell::new(None);
                // Track hover: None = nothing hovered this frame.
                let new_hover: Cell<Option<Selection>> = Cell::new(None);

                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    ui.spacing_mut().indent = 12.0;
                    // Snapshot all data needed for rendering (immutable borrows only).
                    let shell_data: Vec<_> = state
                        .shells
                        .iter()
                        .enumerate()
                        .map(|(idx, s)| {
                            (
                                idx,
                                s.id,
                                s.name.clone(),
                                s.expanded,
                                s.visible,
                                s.failed_faces,
                                s.face_ids.clone(),
                                s.standalone_edge_ids.clone(),
                            )
                        })
                        .collect();

                    // Snapshot face data.
                    struct FaceSnap {
                        id: usize,
                        source_face_id: usize,
                        name: String,
                        triangles: usize,
                        visible: bool,
                        ui_color: [f32; 3],
                        loop_ids: Vec<usize>,
                    }
                    let face_snaps: Vec<FaceSnap> = state
                        .faces
                        .iter()
                        .map(|f| FaceSnap {
                            id: f.id,
                            source_face_id: f.source_face_id,
                            name: f.name.clone(),
                            triangles: f.triangles,
                            visible: f.visible,
                            ui_color: f.ui_color,
                            loop_ids: f.loop_ids.clone(),
                        })
                        .collect();

                    // Snapshot loop data.
                    struct LoopSnap {
                        id: usize,
                        is_outer: bool,
                        edge_ids: Vec<usize>,
                        trimming_active: bool,
                    }
                    let loop_snaps: Vec<LoopSnap> = state
                        .loops
                        .iter()
                        .map(|l| LoopSnap {
                            id: l.id,
                            is_outer: l.is_outer,
                            edge_ids: l.edge_ids.clone(),
                            trimming_active: l.trimming_active,
                        })
                        .collect();

                    // Snapshot edge data.
                    struct EdgeSnap {
                        id: usize,
                        name: String,
                        point_count: usize,
                        visible: bool,
                    }
                    let edge_snaps: Vec<EdgeSnap> = state
                        .edges
                        .iter()
                        .map(|e| EdgeSnap {
                            id: e.id,
                            name: e.name.clone(),
                            point_count: e.point_count,
                            visible: e.visible,
                        })
                        .collect();

                    for (
                        shell_idx,
                        shell_id,
                        shell_name,
                        expanded,
                        shell_visible,
                        failed_faces,
                        face_ids,
                        standalone_edge_ids,
                    ) in shell_data
                    {
                        let face_count = face_ids.len();

                        // Force shell open if viewport click selected a face in this shell.
                        let force_open = viewport_selected
                            && matches!(current_selection, Some(Selection::Face(fid)) if face_ids.contains(&fid));

                        ui.horizontal(|ui| {
                            let mut vis = shell_visible;
                            if ui.checkbox(&mut vis, "").changed() {
                                vis_changed.set(true);
                                let mut changes = shell_vis_changes.take();
                                changes.push((shell_idx, vis));
                                shell_vis_changes.set(changes);
                            }

                            let header_text = if failed_faces > 0 {
                                format!(
                                    "{} ({} faces, {} failed)",
                                    shell_name, face_count, failed_faces
                                )
                            } else {
                                format!("{} ({} faces)", shell_name, face_count)
                            };
                            let mut header = egui::CollapsingHeader::new(header_text)
                            .id_salt(format!("shell_{}", shell_id))
                            .default_open(expanded);

                            if force_open {
                                header = header.open(Some(true));
                            }

                            let shell_resp = header.show(ui, |ui| {
                                if !shell_visible {
                                    ui.disable();
                                }

                                for &face_id in &face_ids {
                                    let Some(face) = face_snaps.iter().find(|f| f.id == face_id)
                                    else {
                                        continue;
                                    };
                                    let color = egui::Color32::from_rgb(
                                        (face.ui_color[0] * 255.0) as u8,
                                        (face.ui_color[1] * 255.0) as u8,
                                        (face.ui_color[2] * 255.0) as u8,
                                    );
                                    let has_loops = !face.loop_ids.is_empty();

                                    if has_loops {
                                        // Collapsible face with loops/edges.
                                        // Arrow toggles collapse, label text selects.
                                        let face_sel =
                                            current_selection == Some(Selection::Face(face_id));
                                        let face_state_id = ui.make_persistent_id(format!("face_{}", face_id));
                                        let mut face_state = egui::collapsing_header::CollapsingState::load_with_default_open(
                                            ui.ctx(), face_state_id, false,
                                        );
                                        ui.horizontal(|ui| {
                                            let mut fvis = face.visible;
                                            if ui.checkbox(&mut fvis, "").changed() {
                                                vis_changed.set(true);
                                                let mut changes = face_vis_changes.take();
                                                changes.push((face_id, fvis));
                                                face_vis_changes.set(changes);
                                            }
                                            ui.colored_label(color, "\u{25a0}");

                                            // Arrow button for collapse/expand.
                                            if collapse_arrow(ui, face_state.is_open()).clicked() {
                                                face_state.toggle(ui);
                                            }

                                            // Selectable label for selection.
                                            let face_text = format!(
                                                "{} ({} tris)",
                                                face.name, face.triangles
                                            );
                                            let label = ui.add(
                                                egui::Button::selectable(face_sel, face_text)
                                                    .truncate(),
                                            )
                                            .on_hover_text(format!(
                                                "Source face {}",
                                                face.source_face_id + 1
                                            ));
                                            if label.clicked() {
                                                new_selection.set(Some(if face_sel {
                                                    None
                                                } else {
                                                    Some(Selection::Face(face_id))
                                                }));
                                            }
                                            if label.hovered() {
                                                new_hover.set(Some(Selection::Face(face_id)));
                                            }
                                            if viewport_selected && face_sel {
                                                label.scroll_to_me(Some(egui::Align::Center));
                                            }
                                            // RMB context menu for edge visibility.
                                            let face_loop_ids = face.loop_ids.clone();
                                            label.context_menu(|ui| {
                                                let all_edge_ids: Vec<usize> = face_loop_ids.iter()
                                                    .flat_map(|lid| {
                                                        loop_snaps.iter()
                                                            .find(|l| l.id == *lid)
                                                            .map(|l| l.edge_ids.clone())
                                                            .unwrap_or_default()
                                                    })
                                                    .collect();
                                                let all_vis = all_edge_ids.iter().all(|eid| {
                                                    edge_snaps.iter().find(|e| e.id == *eid).is_some_and(|e| e.visible)
                                                });
                                                let none_vis = all_edge_ids.iter().all(|eid| {
                                                    edge_snaps.iter().find(|e| e.id == *eid).is_some_and(|e| !e.visible)
                                                });
                                                if ui.add_enabled(!all_vis, egui::Button::new("Show All Edges")).clicked() {
                                                    edge_vis_changed.set(true);
                                                    let mut changes = edge_vis_changes.take();
                                                    for &eid in &all_edge_ids { changes.push((eid, true)); }
                                                    edge_vis_changes.set(changes);
                                                    ui.close();
                                                }
                                                if ui.add_enabled(!none_vis, egui::Button::new("Hide All Edges")).clicked() {
                                                    edge_vis_changed.set(true);
                                                    let mut changes = edge_vis_changes.take();
                                                    for &eid in &all_edge_ids { changes.push((eid, false)); }
                                                    edge_vis_changes.set(changes);
                                                    ui.close();
                                                }
                                                if ui.button("Invert Edge Visibility").clicked() {
                                                    edge_vis_changed.set(true);
                                                    let mut changes = edge_vis_changes.take();
                                                    for &eid in &all_edge_ids {
                                                        let cur = edge_snaps.iter().find(|e| e.id == eid).is_some_and(|e| e.visible);
                                                        changes.push((eid, !cur));
                                                    }
                                                    edge_vis_changes.set(changes);
                                                    ui.close();
                                                }
                                            });
                                        });
                                        face_state.show_body_unindented(ui, |ui| {
                                            ui.indent(face_state_id, |ui| {
                                                for &loop_id in &face.loop_ids {
                                                    let Some(loop_rec) =
                                                        loop_snaps.iter().find(|l| l.id == loop_id)
                                                    else {
                                                        continue;
                                                    };
                                                    let loop_label = if loop_rec.is_outer {
                                                        format!(
                                                            "Outer Loop ({} edges)",
                                                            loop_rec.edge_ids.len()
                                                        )
                                                    } else {
                                                        format!(
                                                            "Hole ({} edges)",
                                                            loop_rec.edge_ids.len()
                                                        )
                                                    };

                                                    let loop_sel = current_selection
                                                        == Some(Selection::Loop(loop_id));
                                                    let loop_state_id = ui.make_persistent_id(format!("loop_{}", loop_id));
                                                    let mut loop_state = egui::collapsing_header::CollapsingState::load_with_default_open(
                                                        ui.ctx(), loop_state_id, false,
                                                    );
                                                    ui.horizontal(|ui| {
                                                        let mut trim = loop_rec.trimming_active;
                                                        if ui.checkbox(&mut trim, "").changed() {
                                                            let mut changes =
                                                                loop_trim_changes.take();
                                                            changes.push((loop_id, trim));
                                                            loop_trim_changes.set(changes);
                                                        }

                                                        // Arrow button for collapse/expand.
                                                        if collapse_arrow(ui, loop_state.is_open()).clicked() {
                                                            loop_state.toggle(ui);
                                                        }

                                                        // Selectable label for selection.
                                                        let label = ui.add(
                                                            egui::Button::selectable(loop_sel, &loop_label)
                                                                .truncate(),
                                                        );
                                                        if label.clicked() {
                                                            new_selection.set(Some(if loop_sel {
                                                                None
                                                            } else {
                                                                Some(Selection::Loop(loop_id))
                                                            }));
                                                        }
                                                        if label.hovered() {
                                                            new_hover.set(Some(Selection::Loop(loop_id)));
                                                        }
                                                        // RMB context menu for edge visibility in this loop.
                                                        let loop_edge_ids = loop_rec.edge_ids.clone();
                                                        label.context_menu(|ui| {
                                                            let all_vis = loop_edge_ids.iter().all(|eid| {
                                                                edge_snaps.iter().find(|e| e.id == *eid).is_some_and(|e| e.visible)
                                                            });
                                                            let none_vis = loop_edge_ids.iter().all(|eid| {
                                                                edge_snaps.iter().find(|e| e.id == *eid).is_some_and(|e| !e.visible)
                                                            });
                                                            if ui.add_enabled(!all_vis, egui::Button::new("Show All")).clicked() {
                                                                edge_vis_changed.set(true);
                                                                let mut changes = edge_vis_changes.take();
                                                                for &eid in &loop_edge_ids { changes.push((eid, true)); }
                                                                edge_vis_changes.set(changes);
                                                                ui.close();
                                                            }
                                                            if ui.add_enabled(!none_vis, egui::Button::new("Hide All")).clicked() {
                                                                edge_vis_changed.set(true);
                                                                let mut changes = edge_vis_changes.take();
                                                                for &eid in &loop_edge_ids { changes.push((eid, false)); }
                                                                edge_vis_changes.set(changes);
                                                                ui.close();
                                                            }
                                                            if ui.button("Invert").clicked() {
                                                                edge_vis_changed.set(true);
                                                                let mut changes = edge_vis_changes.take();
                                                                for &eid in &loop_edge_ids {
                                                                    let cur = edge_snaps.iter().find(|e| e.id == eid).is_some_and(|e| e.visible);
                                                                    changes.push((eid, !cur));
                                                                }
                                                                edge_vis_changes.set(changes);
                                                                ui.close();
                                                            }
                                                        });
                                                    });
                                                    loop_state.show_body_unindented(ui, |ui| {
                                                        ui.indent(loop_state_id, |ui| {
                                                            for &edge_id in &loop_rec.edge_ids {
                                                                let Some(edge) = edge_snaps
                                                                    .iter()
                                                                    .find(|e| e.id == edge_id)
                                                                else {
                                                                    continue;
                                                                };
                                                                ui.horizontal(|ui| {
                                                                    let mut evis = edge.visible;
                                                                    if ui
                                                                        .checkbox(&mut evis, "")
                                                                        .changed()
                                                                    {
                                                                        edge_vis_changed
                                                                            .set(true);
                                                                        let mut changes =
                                                                            edge_vis_changes
                                                                                .take();
                                                                        changes.push((
                                                                            edge_id, evis,
                                                                        ));
                                                                        edge_vis_changes
                                                                            .set(changes);
                                                                    }
                                                                    let is_sel =
                                                                        current_selection
                                                                            == Some(
                                                                                Selection::Edge(
                                                                                    edge_id,
                                                                                ),
                                                                            );
                                                                    let edge_resp = ui.add(
                                                                        egui::Button::selectable(
                                                                            is_sel,
                                                                            format!(
                                                                            "{} ({} pts)",
                                                                            edge.name,
                                                                            edge.point_count
                                                                        ),
                                                                        )
                                                                        .truncate(),
                                                                    );
                                                                    if edge_resp.clicked()
                                                                    {
                                                                        new_selection.set(
                                                                            Some(if is_sel {
                                                                                None
                                                                            } else {
                                                                                Some(
                                                                                Selection::Edge(
                                                                                    edge_id,
                                                                                ),
                                                                            )
                                                                            }),
                                                                        );
                                                                    }
                                                                    if edge_resp.hovered() {
                                                                        new_hover.set(Some(Selection::Edge(edge_id)));
                                                                    }
                                                                });
                                                            }
                                                        });
                                                    });
                                                }
                                            });
                                        });
                                    } else {
                                        // Simple face label (no loops).
                                        ui.horizontal(|ui| {
                                            let mut fvis = face.visible;
                                            if ui.checkbox(&mut fvis, "").changed() {
                                                vis_changed.set(true);
                                                let mut changes = face_vis_changes.take();
                                                changes.push((face_id, fvis));
                                                face_vis_changes.set(changes);
                                            }
                                            ui.colored_label(color, "\u{25a0}");
                                            let face_sel =
                                                current_selection == Some(Selection::Face(face_id));
                                            let face_label = ui.add(
                                                egui::Button::selectable(
                                                    face_sel,
                                                    format!(
                                                        "{} ({} tris)",
                                                        face.name, face.triangles
                                                    ),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(format!(
                                                "Source face {}",
                                                face.source_face_id + 1
                                            ));
                                            if face_label.clicked() {
                                                new_selection.set(Some(if face_sel {
                                                    None
                                                } else {
                                                    Some(Selection::Face(face_id))
                                                }));
                                            }
                                            if face_label.hovered() {
                                                new_hover.set(Some(Selection::Face(face_id)));
                                            }
                                            // Scroll to this face when selected from viewport.
                                            if viewport_selected && face_sel {
                                                face_label.scroll_to_me(Some(egui::Align::Center));
                                            }
                                        });
                                    }
                                }

                                // Standalone edges.
                                if !standalone_edge_ids.is_empty() {
                                    egui::CollapsingHeader::new(format!(
                                        "Standalone Curves ({})",
                                        standalone_edge_ids.len()
                                    ))
                                    .id_salt(format!("standalone_{}", shell_id))
                                    .default_open(false)
                                    .show(ui, |ui| {
                                        for &edge_id in &standalone_edge_ids {
                                            let Some(edge) =
                                                edge_snaps.iter().find(|e| e.id == edge_id)
                                            else {
                                                continue;
                                            };
                                            ui.horizontal(|ui| {
                                                let mut evis = edge.visible;
                                                if ui.checkbox(&mut evis, "").changed() {
                                                    edge_vis_changed.set(true);
                                                    let mut changes = edge_vis_changes.take();
                                                    changes.push((edge_id, evis));
                                                    edge_vis_changes.set(changes);
                                                }
                                                let is_sel = current_selection
                                                    == Some(Selection::Edge(edge_id));
                                                let sa_edge_resp = ui.add(
                                                    egui::Button::selectable(
                                                        is_sel,
                                                        format!(
                                                            "{} ({} pts)",
                                                            edge.name, edge.point_count
                                                        ),
                                                    )
                                                    .truncate(),
                                                );
                                                if sa_edge_resp.clicked() {
                                                    new_selection.set(Some(if is_sel {
                                                        None
                                                    } else {
                                                        Some(Selection::Edge(edge_id))
                                                    }));
                                                }
                                                if sa_edge_resp.hovered() {
                                                    new_hover.set(Some(Selection::Edge(edge_id)));
                                                }
                                            });
                                        }
                                    });
                                }
                            });
                            // Right-click context menu on shell header.
                            shell_resp.header_response.context_menu(|ui| {
                                let all_visible = face_ids.iter().all(|fid| {
                                    face_snaps.iter().find(|f| f.id == *fid).is_some_and(|f| f.visible)
                                });
                                let none_visible = face_ids.iter().all(|fid| {
                                    face_snaps.iter().find(|f| f.id == *fid).is_some_and(|f| !f.visible)
                                });
                                if ui.add_enabled(!all_visible, egui::Button::new("Show All")).clicked() {
                                    vis_changed.set(true);
                                    let mut changes = face_vis_changes.take();
                                    for &fid in &face_ids {
                                        changes.push((fid, true));
                                    }
                                    face_vis_changes.set(changes);
                                    ui.close();
                                }
                                if ui.add_enabled(!none_visible, egui::Button::new("Hide All")).clicked() {
                                    vis_changed.set(true);
                                    let mut changes = face_vis_changes.take();
                                    for &fid in &face_ids {
                                        changes.push((fid, false));
                                    }
                                    face_vis_changes.set(changes);
                                    ui.close();
                                }
                                if ui.button("Invert").clicked() {
                                    vis_changed.set(true);
                                    let mut changes = face_vis_changes.take();
                                    for &fid in &face_ids {
                                        let cur = face_snaps.iter().find(|f| f.id == fid).is_some_and(|f| f.visible);
                                        changes.push((fid, !cur));
                                    }
                                    face_vis_changes.set(changes);
                                    ui.close();
                                }
                            });
                        });
                    }
                });

                // Apply deferred changes.
                for (shell_idx, new_vis) in shell_vis_changes.take() {
                    state.shells[shell_idx].visible = new_vis;
                }
                for (face_id, new_vis) in face_vis_changes.take() {
                    if let Some(face) = state.faces.iter_mut().find(|f| f.id == face_id) {
                        face.visible = new_vis;
                    }
                }
                for (edge_id, new_vis) in edge_vis_changes.take() {
                    if let Some(edge) = state.edges.iter_mut().find(|e| e.id == edge_id) {
                        edge.visible = new_vis;
                    }
                }
                for (loop_id, new_trim) in loop_trim_changes.take() {
                    if let Some(loop_rec) = state.loops.iter_mut().find(|l| l.id == loop_id) {
                        loop_rec.trimming_active = new_trim;
                        state.retessellate_face = Some(loop_rec.face_id);
                    }
                }
                if let Some(sel) = new_selection.take() {
                    state.selection = sel;
                }
                state.hover = new_hover.take();
                if vis_changed.get() {
                    state.visibility_changed = true;
                    state.face_state_visibility_dirty = true;
                }
                if edge_vis_changed.get() {
                    state.edge_visibility_changed = true;
                }
            }

            meshing_panel(ui, state);
        });

    let left_panel_width = panel_response.response.rect.width();

    let right_panel_id = egui::Id::new("metadata");
    if ctx
        .data_mut(|d| {
            d.get_persisted::<egui::containers::panel::PanelState>(
                right_panel_id,
            )
        })
        .is_none()
    {
        ctx.data_mut(|d| {
            d.insert_persisted(
                right_panel_id,
                egui::containers::panel::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(state.right_panel_width, 100.0),
                    ),
                },
            );
        });
    }
    let right_panel_response = egui::Panel::right(right_panel_id)
        .resizable(true)
        .size_range(100.0..=800.0)
        .show(root_ui, |ui| {
            if let Some(meta) = &state.metadata {
                ui.label(format!("Entity Count: {}", meta.entity_count));
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for entry in &meta.headers {
                        egui::CollapsingHeader::new(&entry.name)
                            .id_salt(&entry.name)
                            .default_open(
                                entry.name == "FILE_NAME"
                                    || entry.name == "FILE_SCHEMA",
                            )
                            .show(ui, |ui| {
                                parameter_ui(ui, &entry.parameter, "value", 0);
                            });
                    }
                });
            } else {
                ui.label("No metadata available");
            }
        });

    let right_panel_width = right_panel_response.response.rect.width();

    if (state.panel_width - left_panel_width).abs() > 1.0
        || (state.right_panel_width - right_panel_width).abs() > 1.0
    {
        state.settings_dirty = true;
    }
    state.panel_width = left_panel_width;
    state.right_panel_width = right_panel_width;

    let window_info = windows.single().ok().map(|w| (w.width(), w.height()));

    // Update camera viewport to account for UI panels.
    if let Ok(mut camera) = camera_query.single_mut()
        && let Ok(window) = windows.single()
    {
        let scale_factor = window.scale_factor();
        let left_panel_physical = (left_panel_width * scale_factor) as u32;
        let right_panel_physical = (right_panel_width * scale_factor) as u32;
        let window_width_physical = window.physical_width();
        let window_height_physical = window.physical_height();

        let viewport_width = window_width_physical
            .saturating_sub(left_panel_physical)
            .saturating_sub(right_panel_physical);

        camera.viewport = Some(Viewport {
            physical_position: UVec2::new(left_panel_physical, 0),
            physical_size: UVec2::new(viewport_width, window_height_physical),
            ..Default::default()
        });
    }

    // Show viewport toolbar and overlays.
    if let Some((window_width, window_height)) = window_info {
        let viewport_x = left_panel_width;
        let viewport_width =
            window_width - left_panel_width - right_panel_width;

        let toolbar_margin = 8.0;
        let toolbar_y = toolbar_margin + 24.0;

        // Viewport overlay frame (no shadow).
        let overlay_frame = egui::Frame::NONE
            .fill(egui::Color32::from_rgba_unmultiplied(40, 40, 40, 220))
            .corner_radius(4.0)
            .inner_margin(4.0);

        // Filename overlay (top-left of viewport).
        if state.mode == AppMode::Viewer
            && let Some(path) = &state.loaded_path
        {
            let filename = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            egui::Area::new(egui::Id::new("filename_overlay"))
                .fixed_pos(egui::pos2(viewport_x + toolbar_margin, toolbar_y))
                .show(ctx, |ui| {
                    overlay_frame.show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(filename)
                                .color(egui::Color32::LIGHT_GRAY),
                        );
                    });
                });
        }

        // Viewport toolbar: toggle icons + quality slider (top-right of
        // viewport).
        if state.scene_data.is_some() {
            egui::Area::new(egui::Id::new("viewport_toolbar"))
                .fixed_pos(egui::pos2(
                    viewport_x + viewport_width - toolbar_margin,
                    toolbar_y,
                ))
                .pivot(egui::Align2::RIGHT_TOP)
                .show(ctx, |ui| {
                    overlay_frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);

                            // Shading mode dropdown.
                            let mode_label = match state.shading_mode {
                                ShadingMode::Shaded => "Shaded",
                                ShadingMode::Flat => "Flat",
                                ShadingMode::Matcap => "Matcap",
                                ShadingMode::XRay => "X-Ray",
                                ShadingMode::Wireframe => "Wireframe",
                            };
                            egui::ComboBox::from_id_salt("shading_mode")
                                .selected_text(mode_label)
                                .width(90.0)
                                .show_ui(ui, |ui| {
                                    for (mode, label) in [
                                        (ShadingMode::Shaded, "Shaded"),
                                        (ShadingMode::Flat, "Flat"),
                                        (ShadingMode::Matcap, "Matcap"),
                                        (ShadingMode::XRay, "X-Ray"),
                                        (ShadingMode::Wireframe, "Wireframe"),
                                    ] {
                                        if ui.selectable_label(state.shading_mode == mode, label).clicked() {
                                            state.shading_mode = mode;
                                            state.shading_mode_changed = true;
                                            state.settings_dirty = true;
                                        }
                                    }
                                });

                            ui.separator();

                            // Quality slider.
                            let mut quality = -state.tessellation_factor.log10();
                            let slider = ui.add(
                                egui::Slider::new(&mut quality, 2.0_f64..=5.0_f64)
                                    .show_value(false)
                                    .custom_formatter(|v, _| {
                                        if v > 4.5 {
                                            "Ultra".to_string()
                                        } else if v > 3.8 {
                                            "High".to_string()
                                        } else if v > 3.0 {
                                            "Medium".to_string()
                                        } else {
                                            "Low".to_string()
                                        }
                                    }),
                            );
                            let new_factor = 10_f64.powf(-quality);
                            if slider.changed() {
                                state.tessellation_factor = new_factor;
                                state.settings_dirty = true;
                            }
                            let factor_changed = (state.tessellation_factor
                                - state.applied_tessellation_factor)
                                .abs()
                                > 1e-10;

                            if !slider.dragged()
                                && factor_changed
                                && state.scene_data.is_some()
                                && state.loading_job.is_none()
                            {
                                log::info!(
                                    "Quality changed: re-tessellating with tessellation_factor={:.6}",
                                    state.tessellation_factor
                                );
                                state.pending_retessellate =
                                    Some(state.tessellation_factor);
                            }
                            slider.on_hover_text("Tessellation quality");

                            ui.separator();

                            let colors_btn = toolbar_icon_toggle(
                                ui,
                                state.show_random_colors,
                                ICON_CASINO,
                            );
                            if colors_btn.clicked() {
                                state.show_random_colors = !state.show_random_colors;
                                state.needs_mesh_rebuild = true;
                                state.settings_dirty = true;
                            }
                            colors_btn.on_hover_text("Random colors");

                            let step_colors_btn = toolbar_icon_toggle(
                                ui,
                                state.show_step_colors,
                                ICON_PALETTE,
                            );
                            if step_colors_btn.clicked() {
                                state.show_step_colors = !state.show_step_colors;
                                state.needs_mesh_rebuild = true;
                                state.settings_dirty = true;
                            }
                            step_colors_btn.on_hover_text("STEP colors");

                            let bbox_btn = toolbar_icon_toggle(
                                ui,
                                state.show_bounding_box,
                                ICON_BOUNDING_BOX,
                            );
                            if bbox_btn.clicked() {
                                state.show_bounding_box = !state.show_bounding_box;
                                state.settings_dirty = true;
                            }
                            bbox_btn.on_hover_text("Bounding box");

                            let wireframe_btn = toolbar_icon_toggle(
                                ui,
                                state.show_wireframe,
                                ICON_WIREFRAME,
                            );
                            if wireframe_btn.clicked() {
                                state.show_wireframe = !state.show_wireframe;
                                state.settings_dirty = true;
                            }
                            wireframe_btn.on_hover_text("Wireframe");

                            let iso_btn = toolbar_icon_toggle(
                                ui,
                                state.show_isoparams,
                                ICON_GRID_ON,
                            );
                            if iso_btn.clicked() {
                                state.show_isoparams = !state.show_isoparams;
                                state.settings_dirty = true;
                            }
                            iso_btn.on_hover_text("Isoparametric curves");

                            let poly_edges_btn = toolbar_icon_toggle(
                                ui,
                                state.show_polygon_edges,
                                ICON_DETAILS,
                            );
                            if poly_edges_btn.clicked() {
                                state.show_polygon_edges = !state.show_polygon_edges;
                                state.settings_dirty = true;
                            }
                            poly_edges_btn.on_hover_text("Polygon edges");

                            // Up-axis flip — a plain button (not a toggle):
                            // the current up axis is shown first, the other
                            // second, and the two swap positions each press.
                            // Plain ASCII text, not a Material Symbols glyph.
                            let up_btn = ui.add_sized(
                                egui::vec2(
                                    TOOLBAR_ICON_BUTTON_SIZE,
                                    TOOLBAR_ICON_BUTTON_SIZE,
                                ),
                                egui::Button::new(
                                    egui::RichText::new(format!(
                                        "{}/{}",
                                        state.up_axis.letter(),
                                        state.up_axis.toggled().letter(),
                                    ))
                                    .size(16.0),
                                ),
                            );
                            if up_btn.clicked() {
                                state.up_axis = state.up_axis.toggled();
                                state.up_axis_changed = true;
                                state.settings_dirty = true;
                            }
                            up_btn.on_hover_text(format!(
                                "Up axis: {} (click to swap)",
                                state.up_axis.letter()
                            ));

                            #[cfg(all(
                                feature = "nsi-render",
                                not(target_arch = "wasm32")
                            ))]
                            {
                                let _ =
                                    nsi_render_toolbar_toggle(ui, nsi_overlay);
                            }

                            ui.separator();

                            let clip_labels = ["X", "Y", "Z"];
                            let clip_colors = [
                                egui::Color32::from_rgb(220, 80, 80),
                                egui::Color32::from_rgb(80, 200, 80),
                                egui::Color32::from_rgb(80, 120, 220),
                            ];
                            for i in 0..3 {
                                let cp = &state.clip_planes[i];
                                let (label_str, tooltip) = if !cp.enabled {
                                    (clip_labels[i].to_string(), format!("Clip +{} axis (click to enable)", clip_labels[i]))
                                } else if !cp.flip {
                                    (format!("+{}", clip_labels[i]), format!("Clip +{} (click for −{})", clip_labels[i], clip_labels[i]))
                                } else {
                                    (format!("−{}", clip_labels[i]), format!("Clip −{} (click to disable)", clip_labels[i]))
                                };
                                let label = if cp.enabled {
                                    egui::RichText::new(&label_str)
                                        .color(clip_colors[i])
                                        .strong()
                                } else {
                                    egui::RichText::new(&label_str)
                                };
                                let clip_btn = ui.selectable_label(cp.enabled, label);
                                if clip_btn.clicked() {
                                    // Cycle: Off → +axis → −axis → Off
                                    if !state.clip_planes[i].enabled {
                                        state.clip_planes[i].enabled = true;
                                        state.clip_planes[i].flip = false;
                                    } else if !state.clip_planes[i].flip {
                                        state.clip_planes[i].flip = true;
                                    } else {
                                        state.clip_planes[i].enabled = false;
                                        state.clip_planes[i].flip = false;
                                    }
                                    state.clip_planes_dirty = true;
                                    state.settings_dirty = true;
                                }
                                clip_btn.on_hover_text(tooltip);
                            }

                        });
                    });
                });
        }

        if let Some(err) = &state.error {
            egui::Area::new(egui::Id::new("error_overlay"))
                .fixed_pos(egui::pos2(viewport_x + 10.0, window_height - 40.0))
                .show(ctx, |ui| {
                    ui.colored_label(egui::Color32::RED, err);
                });
        } else if let Some(job) = &state.loading_job {
            let bar_height = 24.0;
            let bar_y = window_height - bar_height - 10.0;

            let current = job.current_shell;
            let total = job.total_shells;
            let fraction = if total > 0 {
                current as f32 / total as f32
            } else {
                0.0
            };

            egui::Area::new(egui::Id::new("progress_overlay"))
                .fixed_pos(egui::pos2(viewport_x, bar_y))
                .show(ctx, |ui| {
                    let rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(viewport_width, bar_height),
                    );

                    ui.painter().rect_filled(
                        rect,
                        4.0,
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200),
                    );

                    let progress_rect = if total > 0 {
                        (fraction > 0.0).then(|| {
                            egui::Rect::from_min_size(
                                rect.min,
                                egui::vec2(
                                    viewport_width * fraction,
                                    bar_height,
                                ),
                            )
                        })
                    } else {
                        let segment_width = (viewport_width * 0.28)
                            .max(32.0)
                            .min(viewport_width);
                        let offset = (ctx.input(|i| i.time) as f32 * 0.75)
                            .fract()
                            * (viewport_width - segment_width);
                        Some(egui::Rect::from_min_size(
                            egui::pos2(rect.min.x + offset, rect.min.y),
                            egui::vec2(segment_width, bar_height),
                        ))
                    };

                    if let Some(progress_rect) = progress_rect {
                        ui.painter().rect_filled(
                            progress_rect,
                            4.0,
                            egui::Color32::from_rgb(100, 149, 237),
                        );
                    }

                    let phase_label = job.phase.label();
                    let text = if total > 0 {
                        format!(
                            "{} {}/{} ({:.0}%)",
                            phase_label,
                            current,
                            total,
                            fraction * 100.0
                        )
                    } else {
                        format!("{}...", phase_label)
                    };

                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::FontId::proportional(14.0),
                        egui::Color32::WHITE,
                    );
                });

            ctx.request_repaint();
        }
    }
}

fn browser_ui(
    root_ui: &mut egui::Ui,
    state: &mut ResMut<ViewerState>,
    browser: &mut ResMut<BrowserState>,
) {
    let ctx = root_ui.ctx().clone();
    let ctx = &ctx;
    // Left panel: directory tree.
    let browser_panel_id = egui::Id::new("browser_tree");
    if ctx
        .data_mut(|d| {
            d.get_persisted::<egui::containers::panel::PanelState>(
                browser_panel_id,
            )
        })
        .is_none()
    {
        ctx.data_mut(|d| {
            d.insert_persisted(
                browser_panel_id,
                egui::containers::panel::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::pos2(0.0, 0.0),
                        egui::vec2(state.panel_width, 100.0),
                    ),
                },
            );
        });
    }
    egui::Panel::left(browser_panel_id)
        .resizable(true)
        .size_range(100.0..=800.0)
        .show(root_ui, |ui| {
            ui.heading("Directories");
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                // Collect actions to avoid borrow issues.
                let mut select_dir: Option<PathBuf> = None;
                let mut expand_actions: Vec<(Vec<usize>, bool)> = Vec::new();
                let mut children_to_load: Vec<Vec<usize>> = Vec::new();

                render_dir_tree(
                    ui,
                    &browser.tree,
                    &browser.selected_dir,
                    &mut select_dir,
                    &mut expand_actions,
                    &mut children_to_load,
                    &mut Vec::new(),
                );

                // Apply expand/collapse.
                for (path_indices, expanded) in expand_actions {
                    if let Some(entry) =
                        get_entry_mut(&mut browser.tree, &path_indices)
                    {
                        entry.expanded = expanded;
                    }
                }

                // Lazy-load children.
                for path_indices in children_to_load {
                    if let Some(entry) =
                        get_entry_mut(&mut browser.tree, &path_indices)
                        && entry.children.is_none()
                    {
                        entry.children = Some(scan_subdirs(&entry.path));
                    }
                }

                // Handle directory selection.
                if let Some(dir) = select_dir
                    && browser.selected_dir.as_ref() != Some(&dir)
                {
                    browser.selected_dir = Some(dir.clone());
                    browser.previews = scan_step_files(&dir);
                    start_preview_loads(browser);
                    state.settings_dirty = true;
                    // Refresh the selected directory's children in the tree
                    // so that newly created or removed subdirectories are
                    // reflected.
                    refresh_tree_entry(&mut browser.tree, &dir);
                }
            });
        });

    // Main area: preview grid.
    egui::CentralPanel::default().show(root_ui, |ui| {
        if browser.selected_dir.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("Select a directory to browse STEP files");
            });
            return;
        }

        if browser.previews.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No STEP files in this directory");
            });
            return;
        }

        let available_width = ui.available_width();
        let thumb_size = 200.0_f32;
        let spacing = 8.0_f32;
        let cols = ((available_width + spacing) / (thumb_size + spacing))
            .floor()
            .max(1.0) as usize;
        let label_height = 20.0;
        let cell_height = thumb_size + label_height + spacing;
        let rows = browser.previews.len().div_ceil(cols);
        let total_height = rows as f32 * cell_height;

        browser.grid_cols = cols;
        browser.thumb_size = thumb_size;

        egui::ScrollArea::vertical().show(ui, |ui| {
            // Reserve space for the full grid.
            let (response, painter) = ui.allocate_painter(
                egui::vec2(available_width, total_height),
                egui::Sense::click(),
            );
            let origin = response.rect.min;

            browser.scroll_offset = ui.clip_rect().min.y - origin.y;
            browser.visible_rows =
                (ui.clip_rect().height() / cell_height).ceil() as usize + 1;

            // Render visible cells.
            let first_visible_row =
                (browser.scroll_offset / cell_height).floor().max(0.0) as usize;
            let last_visible_row =
                (first_visible_row + browser.visible_rows + 1).min(rows);

            for row in first_visible_row..last_visible_row {
                for col in 0..cols {
                    let idx = row * cols + col;
                    if idx >= browser.previews.len() {
                        break;
                    }

                    let x = origin.x + col as f32 * (thumb_size + spacing);
                    let y = origin.y + row as f32 * cell_height;
                    let thumb_rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(thumb_size, thumb_size),
                    );

                    let preview = &browser.previews[idx];

                    // Draw thumbnail background.
                    painter.rect_filled(
                        thumb_rect,
                        4.0,
                        egui::Color32::from_rgb(40, 40, 40),
                    );

                    match &preview.status {
                        PreviewStatus::Ready(_) => {
                            // Find the render slot for this preview.
                            if let Some(slot) = browser
                                .render_slots
                                .iter()
                                .find(|s| s.preview_index == Some(idx))
                            {
                                if let Some(tex_id) = slot.egui_texture_id {
                                    painter.image(
                                        tex_id,
                                        thumb_rect,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        egui::Color32::WHITE,
                                    );
                                }
                            } else {
                                // Not in a render slot — show placeholder.
                                painter.text(
                                    thumb_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "Ready",
                                    egui::FontId::proportional(12.0),
                                    egui::Color32::GRAY,
                                );
                            }
                        }
                        PreviewStatus::Loading | PreviewStatus::Pending => {
                            // Animated spinner.
                            let time = ctx.input(|i| i.time);
                            let radius = thumb_size * 0.12;
                            let center = thumb_rect.center();
                            let n_points = 24u32;
                            let start_angle = time * std::f64::consts::TAU;
                            let end_angle =
                                start_angle + 240f64.to_radians() * time.sin();
                            let points: Vec<egui::Pos2> = (0..n_points)
                                .map(|i| {
                                    let t = i as f64 / n_points as f64;
                                    let angle = start_angle
                                        + (end_angle - start_angle) * t;
                                    let (sin, cos) = angle.sin_cos();
                                    center
                                        + egui::vec2(
                                            cos as f32 * radius,
                                            sin as f32 * radius,
                                        )
                                })
                                .collect();
                            painter.add(egui::Shape::line(
                                points,
                                egui::Stroke::new(2.5_f32, egui::Color32::GRAY),
                            ));
                        }
                        PreviewStatus::Failed(err) => {
                            painter.text(
                                thumb_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                err,
                                egui::FontId::proportional(12.0),
                                egui::Color32::from_rgb(200, 80, 80),
                            );
                        }
                    }

                    // Filename label.
                    let label_pos = egui::pos2(x, y + thumb_size + 2.0);
                    painter.text(
                        label_pos,
                        egui::Align2::LEFT_TOP,
                        &preview.filename,
                        egui::FontId::proportional(12.0),
                        egui::Color32::LIGHT_GRAY,
                    );

                    // Double-click detection.
                    let click_rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(thumb_size, cell_height),
                    );
                    let click_response = ui.interact(
                        click_rect,
                        egui::Id::new(("preview_click", idx)),
                        egui::Sense::click(),
                    );
                    if click_response.double_clicked() {
                        state.pending_path = Some(preview.path.clone());
                        state.mode = AppMode::Viewer;
                        state.settings_dirty = true;
                    }

                    // Hover highlight.
                    if click_response.hovered() {
                        painter.rect_stroke(
                            thumb_rect,
                            4.0,
                            egui::Stroke::new(
                                2.0_f32,
                                egui::Color32::from_rgb(100, 149, 237),
                            ),
                            egui::epaint::StrokeKind::Outside,
                        );
                    }
                }
            }
        });

        // Request repaint while any previews are loading or turntables are
        // active.
        let has_active_slots = browser
            .render_slots
            .iter()
            .any(|s| s.preview_index.is_some());
        let has_loading = browser.previews.iter().any(|p| {
            matches!(p.status, PreviewStatus::Loading | PreviewStatus::Pending)
        });
        if has_active_slots || has_loading {
            ctx.request_repaint();
        }
    });
}

/// Render directory tree recursively, collecting actions.
fn render_dir_tree(
    ui: &mut egui::Ui,
    entries: &[DirectoryEntry],
    selected_dir: &Option<PathBuf>,
    select_dir: &mut Option<PathBuf>,
    expand_actions: &mut Vec<(Vec<usize>, bool)>,
    children_to_load: &mut Vec<Vec<usize>>,
    current_path: &mut Vec<usize>,
) {
    for (i, entry) in entries.iter().enumerate() {
        current_path.push(i);
        let is_selected = selected_dir.as_ref() == Some(&entry.path);
        let has_children =
            entry.children.as_ref().is_some_and(|c| !c.is_empty());
        let not_loaded = entry.children.is_none();

        if has_children || not_loaded {
            let header = egui::CollapsingHeader::new(
                egui::RichText::new(&entry.name).color(if is_selected {
                    egui::Color32::from_rgb(100, 149, 237)
                } else {
                    egui::Color32::LIGHT_GRAY
                }),
            )
            .id_salt(entry.path.display().to_string())
            .default_open(entry.expanded)
            .show_background(is_selected);

            let response = header.show(ui, |ui| {
                if let Some(children) = &entry.children {
                    render_dir_tree(
                        ui,
                        children,
                        selected_dir,
                        select_dir,
                        expand_actions,
                        children_to_load,
                        current_path,
                    );
                }
            });

            // Track expand/collapse.
            let now_open = response.body_response.is_some();
            if now_open != entry.expanded {
                expand_actions.push((current_path.clone(), now_open));
            }

            // Lazy-load children when expanded.
            if now_open && entry.children.is_none() {
                children_to_load.push(current_path.clone());
            }

            // Click to select directory.
            if response.header_response.clicked() {
                *select_dir = Some(entry.path.clone());
            }
        } else {
            // Leaf directory (no subdirectories).
            let label = ui.selectable_label(is_selected, &entry.name);
            if label.clicked() {
                *select_dir = Some(entry.path.clone());
            }
        }

        current_path.pop();
    }
}

/// Navigate a mutable tree by index path.
fn get_entry_mut<'a>(
    tree: &'a mut [DirectoryEntry],
    path: &[usize],
) -> Option<&'a mut DirectoryEntry> {
    let (&first, rest) = path.split_first()?;
    let entry = tree.get_mut(first)?;
    if rest.is_empty() {
        Some(entry)
    } else if let Some(children) = &mut entry.children {
        get_entry_mut(children, rest)
    } else {
        None
    }
}
