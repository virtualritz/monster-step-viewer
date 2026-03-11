use crate::browser::{poll_preview_loads, scan_step_files, scan_subdirs, start_preview_loads};
use crate::icons::{
    ICON_BOUNDING_BOX, ICON_CASINO, ICON_EDGES, ICON_WIREFRAME, configure_fonts, icon_text,
};
use crate::state::{
    AppMode, BrowserState, DEFAULT_PANEL_WIDTH, DirectoryEntry, MainCamera, PreviewStatus,
    Selection, ViewerState,
};
use bevy::{app::AppExit, camera::Viewport, prelude::MessageWriter, prelude::*};
use bevy_egui::{EguiContexts, egui};
use monster_step_viewer::Parameter;
use std::cell::Cell;

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
pub(crate) fn parameter_ui(ui: &mut egui::Ui, param: &Parameter, label: &str, depth: usize) {
    // Max depth guard.
    if depth > 4 {
        ui.label(format!("{}: ...", label));
        return;
    }

    match param {
        Parameter::List(items) if items.is_empty() => {
            ui.label(format!("{}: []", label));
        }
        Parameter::List(items) if items.len() <= 3 && items.iter().all(is_leaf_param) => {
            let values: Vec<String> = items.iter().map(format_leaf_param).collect();
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
    windows: Query<&Window>,
    mut camera_query: Query<&mut Camera, With<MainCamera>>,
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

    // Top bar with tabs and menu items.
    egui::TopBottomPanel::top("menu").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.style_mut().override_text_style = Some(egui::TextStyle::Heading);
            // Tab buttons.
            let viewer_selected = state.mode == AppMode::Viewer;
            if ui.selectable_label(viewer_selected, "Viewer").clicked() && !viewer_selected {
                state.mode = AppMode::Viewer;
                state.settings_dirty = true;
            }
            if ui.selectable_label(!viewer_selected, "Browser").clicked() && viewer_selected {
                state.mode = AppMode::Browser;
                state.settings_dirty = true;
                // Load previews on first switch if a directory was previously selected.
                if browser.previews.is_empty()
                    && let Some(dir) = &browser.selected_dir {
                        browser.previews = scan_step_files(dir);
                        start_preview_loads(&mut browser);
                    }
            }

            ui.separator();

            if state.mode == AppMode::Viewer {
                if ui.button("Open STEP\u{2026}").clicked() {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("STEP", &["stp", "step"])
                        .pick_file()
                    {
                        state.pending_path = Some(path);
                        state.settings_dirty = true;
                    }

                    #[cfg(target_arch = "wasm32")]
                    {
                        state.error = Some("File open dialog is not supported on wasm".to_string());
                    }
                }

                if let Some(path) = &state.loaded_path {
                    ui.separator();
                    ui.label(format!("{}", path.display()));
                }
            } else {
                // Browser mode: show breadcrumb path.
                let display_path = browser.selected_dir.as_deref().unwrap_or(&browser.root);
                let mut breadcrumb_nav: Option<std::path::PathBuf> = None;
                let mut accumulated = std::path::PathBuf::new();
                for (i, component) in display_path.components().enumerate() {
                    accumulated.push(component);
                    if i > 0 {
                        ui.label("/");
                    }
                    let name = component.as_os_str().to_string_lossy();
                    if ui
                        .add(
                            egui::Label::new(
                                egui::RichText::new(name.as_ref()).color(egui::Color32::LIGHT_GRAY),
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
                    // Expand tree to this path.
                    let root = browser.root.clone();
                    crate::browser::expand_tree_to_path(&mut browser.tree, &root, &nav_to);
                    state.settings_dirty = true;
                }
            }
        });
    });

    match state.mode {
        AppMode::Viewer => viewer_ui(ctx, &mut state, &windows, &mut camera_query),
        AppMode::Browser => browser_ui(ctx, &mut state, &mut browser),
    }

    // Ctrl+/- to zoom egui UI.
    ctx.input(|i| {
        if i.modifiers.command {
            let mut ppp = ctx.pixels_per_point();
            if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                ppp = (ppp + 0.1).min(3.0);
            } else if i.key_pressed(egui::Key::Minus) {
                ppp = (ppp - 0.1).max(0.5);
            } else if i.key_pressed(egui::Key::Num0) {
                ppp = 1.0;
            }
            ctx.set_pixels_per_point(ppp);
        }
    });

    // Allow escape to quit quickly on desktop.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        exit.write(AppExit::Success);
    }
}

fn viewer_ui(
    ctx: &egui::Context,
    state: &mut ResMut<ViewerState>,
    windows: &Query<&Window>,
    camera_query: &mut Query<&mut Camera, With<MainCamera>>,
) {
    let panel_response = egui::SidePanel::left("entities")
        .default_width(state.panel_width.max(DEFAULT_PANEL_WIDTH))
        .resizable(true)
        .show(ctx, |ui| {
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
                // None = no change, Some(x) = set selection to x
                let new_selection: Cell<Option<Option<Selection>>> = Cell::new(None);

                egui::ScrollArea::vertical().show(ui, |ui| {
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
                                s.face_ids.clone(),
                                s.standalone_edge_ids.clone(),
                            )
                        })
                        .collect();

                    // Snapshot face data.
                    struct FaceSnap {
                        id: usize,
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
                        face_ids,
                        standalone_edge_ids,
                    ) in shell_data
                    {
                        let face_count = face_ids.len();

                        ui.horizontal(|ui| {
                            let mut vis = shell_visible;
                            if ui.checkbox(&mut vis, "").changed() {
                                vis_changed.set(true);
                                let mut changes = shell_vis_changes.take();
                                changes.push((shell_idx, vis));
                                shell_vis_changes.set(changes);
                            }

                            let header = egui::CollapsingHeader::new(format!(
                                "{} ({} faces)",
                                shell_name, face_count
                            ))
                            .id_salt(format!("shell_{}", shell_id))
                            .default_open(expanded);

                            header.show(ui, |ui| {
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
                                            let face_header_text = if face_sel {
                                                egui::RichText::new(format!(
                                                    "{} ({} tris)",
                                                    face.name, face.triangles
                                                ))
                                                .color(egui::Color32::from_rgb(255, 217, 0))
                                            } else {
                                                egui::RichText::new(format!(
                                                    "{} ({} tris)",
                                                    face.name, face.triangles
                                                ))
                                            };
                                            let face_resp = egui::CollapsingHeader::new(
                                                face_header_text,
                                            )
                                            .id_salt(format!("face_{}", face_id))
                                            .default_open(false)
                                            .show(ui, |ui| {
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
                                                    let loop_header_text = if loop_sel {
                                                        egui::RichText::new(&loop_label).color(
                                                            egui::Color32::from_rgb(255, 217, 0),
                                                        )
                                                    } else {
                                                        egui::RichText::new(&loop_label)
                                                    };
                                                    ui.horizontal(|ui| {
                                                        let mut trim = loop_rec.trimming_active;
                                                        if ui.checkbox(&mut trim, "").changed() {
                                                            let mut changes =
                                                                loop_trim_changes.take();
                                                            changes.push((loop_id, trim));
                                                            loop_trim_changes.set(changes);
                                                        }
                                                        let loop_resp =
                                                            egui::CollapsingHeader::new(
                                                                loop_header_text,
                                                            )
                                                            .id_salt(format!("loop_{}", loop_id))
                                                            .default_open(false)
                                                            .show(ui, |ui| {
                                                                // Edge entries.
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
                                                                        if ui
                                                                            .selectable_label(
                                                                                is_sel,
                                                                                format!(
                                                                                "{} ({} pts)",
                                                                                edge.name,
                                                                                edge.point_count
                                                                            ),
                                                                            )
                                                                            .clicked()
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
                                                                    });
                                                                }
                                                            });
                                                        if loop_resp.header_response.clicked() {
                                                            new_selection.set(Some(if loop_sel {
                                                                None
                                                            } else {
                                                                Some(Selection::Loop(loop_id))
                                                            }));
                                                        }
                                                    }); // close ui.horizontal for loop
                                                }
                                            });
                                            if face_resp.header_response.clicked() {
                                                new_selection.set(Some(if face_sel {
                                                    None
                                                } else {
                                                    Some(Selection::Face(face_id))
                                                }));
                                            }
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
                                            if ui
                                                .selectable_label(
                                                    face_sel,
                                                    format!(
                                                        "{} ({} tris)",
                                                        face.name, face.triangles
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                new_selection.set(Some(if face_sel {
                                                    None
                                                } else {
                                                    Some(Selection::Face(face_id))
                                                }));
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
                                                if ui
                                                    .selectable_label(
                                                        is_sel,
                                                        format!(
                                                            "{} ({} pts)",
                                                            edge.name, edge.point_count
                                                        ),
                                                    )
                                                    .clicked()
                                                {
                                                    new_selection.set(Some(if is_sel {
                                                        None
                                                    } else {
                                                        Some(Selection::Edge(edge_id))
                                                    }));
                                                }
                                            });
                                        }
                                    });
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
                if vis_changed.get() {
                    state.visibility_changed = true;
                }
                if edge_vis_changed.get() {
                    state.edge_visibility_changed = true;
                }
            }
        });

    let left_panel_width = panel_response.response.rect.width();

    let right_panel_response = egui::SidePanel::right("metadata")
        .resizable(true)
        .default_width(state.right_panel_width.max(200.0))
        .show(ctx, |ui| {
            if let Some(meta) = &state.metadata {
                ui.label(format!("Entity Count: {}", meta.entity_count));
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for entry in &meta.headers {
                        egui::CollapsingHeader::new(&entry.name)
                            .id_salt(&entry.name)
                            .default_open(entry.name == "FILE_NAME" || entry.name == "FILE_SCHEMA")
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
        let viewport_width = window_width - left_panel_width - right_panel_width;

        let toolbar_margin = 8.0;
        let toolbar_y = toolbar_margin + 24.0;

        // Quality slider overlay (top-left of viewport).
        {
            let slider_x = viewport_x + toolbar_margin;
            egui::Area::new(egui::Id::new("quality_overlay"))
                .fixed_pos(egui::pos2(slider_x, toolbar_y))
                .show(ctx, |ui| {
                    ui.visuals_mut().widgets.inactive.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(40, 40, 40, 220);
                    ui.visuals_mut().widgets.hovered.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(60, 60, 60, 230);
                    ui.visuals_mut().widgets.active.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(80, 80, 80, 240);

                    egui::Frame::popup(ui.style()).show(ui, |ui| {
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
                        let factor_changed =
                            (state.tessellation_factor - state.applied_tessellation_factor).abs()
                                > 1e-10;

                        if !slider.dragged()
                            && factor_changed
                            && state.loaded_path.is_some()
                            && state.loading_job.is_none()
                        {
                            log::info!(
                                "Quality changed: reloading with tessellation_factor={:.6}",
                                state.tessellation_factor
                            );
                            state.pending_path = state.loaded_path.clone();
                        }
                        slider.on_hover_text("Tessellation quality");
                    });
                });
        }

        // Toggle toolbar (top-right of window).
        if state.scene_data.is_some() {
            let toolbar_x = window_width - toolbar_margin;

            egui::Area::new(egui::Id::new("viewport_toolbar"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(0.0, 0.0))
                .fixed_pos(egui::pos2(toolbar_x, toolbar_y))
                .show(ctx, |ui| {
                    ui.visuals_mut().widgets.inactive.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(40, 40, 40, 220);
                    ui.visuals_mut().widgets.hovered.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(60, 60, 60, 230);
                    ui.visuals_mut().widgets.active.weak_bg_fill =
                        egui::Color32::from_rgba_unmultiplied(80, 80, 80, 240);

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(4.0, 0.0);

                        let colors_btn =
                            ui.selectable_label(state.show_random_colors, icon_text(ICON_CASINO));
                        if colors_btn.clicked() {
                            state.show_random_colors = !state.show_random_colors;
                            state.needs_mesh_rebuild = true;
                            state.settings_dirty = true;
                        }
                        colors_btn.on_hover_text("Random colors");

                        let bbox_btn = ui.selectable_label(
                            state.show_bounding_box,
                            icon_text(ICON_BOUNDING_BOX),
                        );
                        if bbox_btn.clicked() {
                            state.show_bounding_box = !state.show_bounding_box;
                            state.settings_dirty = true;
                        }
                        bbox_btn.on_hover_text("Bounding box");

                        let wire_btn =
                            ui.selectable_label(state.show_wireframe, icon_text(ICON_WIREFRAME));
                        if wire_btn.clicked() {
                            state.show_wireframe = !state.show_wireframe;
                            state.settings_dirty = true;
                        }
                        wire_btn.on_hover_text("Wireframe edges");

                        let edge_btn = ui.selectable_label(state.show_edges, icon_text(ICON_EDGES));
                        if edge_btn.clicked() {
                            state.show_edges = !state.show_edges;
                            state.settings_dirty = true;
                        }
                        edge_btn.on_hover_text("Curve edges");
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

                    if fraction > 0.0 {
                        let progress_rect = egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2(viewport_width * fraction, bar_height),
                        );
                        ui.painter().rect_filled(
                            progress_rect,
                            4.0,
                            egui::Color32::from_rgb(100, 149, 237),
                        );
                    }

                    let text = if total > 0 {
                        format!(
                            "Tessellating shell {}/{} ({:.0}%)",
                            current,
                            total,
                            fraction * 100.0
                        )
                    } else {
                        "Parsing STEP file...".to_string()
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
    ctx: &egui::Context,
    state: &mut ResMut<ViewerState>,
    browser: &mut ResMut<BrowserState>,
) {
    // Left panel: directory tree.
    egui::SidePanel::left("browser_tree")
        .default_width(state.panel_width.max(DEFAULT_PANEL_WIDTH))
        .resizable(true)
        .show(ctx, |ui| {
            ui.heading("Directories");
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                // Collect actions to avoid borrow issues.
                let mut select_dir: Option<std::path::PathBuf> = None;
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
                    if let Some(entry) = get_entry_mut(&mut browser.tree, &path_indices) {
                        entry.expanded = expanded;
                    }
                }

                // Lazy-load children.
                for path_indices in children_to_load {
                    if let Some(entry) = get_entry_mut(&mut browser.tree, &path_indices)
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
                }
            });
        });

    // Main area: preview grid.
    egui::CentralPanel::default().show(ctx, |ui| {
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
            browser.visible_rows = (ui.clip_rect().height() / cell_height).ceil() as usize + 1;

            // Render visible cells.
            let first_visible_row = (browser.scroll_offset / cell_height).floor().max(0.0) as usize;
            let last_visible_row = (first_visible_row + browser.visible_rows + 1).min(rows);

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
                    painter.rect_filled(thumb_rect, 4.0, egui::Color32::from_rgb(40, 40, 40));

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
                            painter.text(
                                thumb_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Loading\u{2026}",
                                egui::FontId::proportional(12.0),
                                egui::Color32::GRAY,
                            );
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
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 149, 237)),
                            egui::epaint::StrokeKind::Outside,
                        );
                    }
                }
            }
        });

        // Request repaint while any previews are loading or turntables are active.
        let has_active_slots = browser
            .render_slots
            .iter()
            .any(|s| s.preview_index.is_some());
        let has_loading = browser
            .previews
            .iter()
            .any(|p| matches!(p.status, PreviewStatus::Loading | PreviewStatus::Pending));
        if has_active_slots || has_loading {
            ctx.request_repaint();
        }
    });
}

/// Render directory tree recursively, collecting actions.
fn render_dir_tree(
    ui: &mut egui::Ui,
    entries: &[DirectoryEntry],
    selected_dir: &Option<std::path::PathBuf>,
    select_dir: &mut Option<std::path::PathBuf>,
    expand_actions: &mut Vec<(Vec<usize>, bool)>,
    children_to_load: &mut Vec<Vec<usize>>,
    current_path: &mut Vec<usize>,
) {
    for (i, entry) in entries.iter().enumerate() {
        current_path.push(i);
        let is_selected = selected_dir.as_ref() == Some(&entry.path);
        let has_children = entry.children.as_ref().is_some_and(|c| !c.is_empty());
        let not_loaded = entry.children.is_none();

        if has_children || not_loaded {
            let header = egui::CollapsingHeader::new(egui::RichText::new(&entry.name).color(
                if is_selected {
                    egui::Color32::from_rgb(100, 149, 237)
                } else {
                    egui::Color32::LIGHT_GRAY
                },
            ))
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
