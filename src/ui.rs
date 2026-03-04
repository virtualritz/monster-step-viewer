use crate::state::{DEFAULT_PANEL_WIDTH, MainCamera, ViewerState};
use bevy::{app::AppExit, camera::Viewport, prelude::MessageWriter, prelude::*};
use bevy_egui::{EguiContexts, egui};
use monster_step_viewer::Parameter;
use std::cell::Cell;

/// Render a STEP Parameter as a collapsible tree in egui.
pub(crate) fn parameter_ui(ui: &mut egui::Ui, param: &Parameter, label: &str, depth: usize) {
    match param {
        Parameter::List(items) if !items.is_empty() => {
            egui::CollapsingHeader::new(format!("{} ({})", label, items.len()))
                .id_salt(format!("{}_{}", label, depth))
                .default_open(depth < 1)
                .show(ui, |ui| {
                    for (i, item) in items.iter().enumerate() {
                        parameter_ui(ui, item, &format!("[{}]", i), depth + 1);
                    }
                });
        }
        Parameter::List(_) => {
            ui.label(format!("{}: []", label));
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
        Parameter::Typed { keyword, parameter } => {
            egui::CollapsingHeader::new(format!("{}: {}", label, keyword))
                .id_salt(format!("{}_typed_{}", label, depth))
                .default_open(depth < 2)
                .show(ui, |ui| {
                    parameter_ui(ui, parameter, "value", depth + 1);
                });
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
    mut exit: MessageWriter<AppExit>,
    windows: Query<&Window>,
    mut camera_query: Query<&mut Camera, With<MainCamera>>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    egui::TopBottomPanel::top("menu").show(ctx, |ui| {
        ui.horizontal(|ui| {
            if ui.button("Open STEP file\u{2026}").clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("STEP", &["stp", "step"])
                    .pick_file()
                {
                    state.pending_path = Some(path);
                }

                #[cfg(target_arch = "wasm32")]
                {
                    state.error = Some("File open dialog is not supported on wasm".to_string());
                }
            }

            ui.separator();

            // Quality slider (logarithmic scale for better UX).
            // Higher quality = finer mesh (smaller tessellation factor).
            // Quality maps to -log10(tessellation_factor): 2.0 (low) to 5.0 (ultra).
            // Factor range: 0.01 (coarse) to 0.00001 (ultra fine).
            let mut quality = -state.tessellation_factor.log10();
            ui.label("Quality:");
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
            }
            // Reload when slider released and factor differs from what was used to load.
            let factor_changed =
                (state.tessellation_factor - state.applied_tessellation_factor).abs() > 1e-10;

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

            ui.separator();

            if let Some(path) = &state.loaded_path {
                ui.label(format!("Loaded: {}", path.display()));
            } else {
                ui.label("No file loaded");
            }
        });
    });

    let panel_response = egui::SidePanel::left("entities")
        .default_width(DEFAULT_PANEL_WIDTH)
        .resizable(true)
        .show(ctx, |ui| {
            ui.heading("Model Hierarchy");
            ui.separator();

            if state.shells.is_empty() && state.loading_job.is_none() {
                ui.label("Load a STEP file to see hierarchy");
            } else if state.shells.is_empty() {
                ui.label("Loading...");
            } else {
                // Track if any visibility checkbox was toggled.
                let vis_changed = Cell::new(false);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    // We need to collect shell data first to avoid borrow issues.
                    let shell_data: Vec<_> = state
                        .shells
                        .iter()
                        .map(|s| (s.id, s.name.clone(), s.expanded, s.face_ids.clone()))
                        .collect();

                    for (shell_id, shell_name, expanded, face_ids) in shell_data {
                        let face_count = face_ids.len();
                        let header = egui::CollapsingHeader::new(format!(
                            "{} ({} faces)",
                            shell_name, face_count
                        ))
                        .id_salt(format!("shell_{}", shell_id))
                        .default_open(expanded);

                        header.show(ui, |ui| {
                            for &face_id in &face_ids {
                                if let Some(face) = state.faces.iter_mut().find(|f| f.id == face_id)
                                {
                                    let color = egui::Color32::from_rgb(
                                        (face.ui_color[0] * 255.0) as u8,
                                        (face.ui_color[1] * 255.0) as u8,
                                        (face.ui_color[2] * 255.0) as u8,
                                    );
                                    ui.horizontal(|ui| {
                                        let prev_visible = face.visible;
                                        ui.checkbox(&mut face.visible, "");
                                        if face.visible != prev_visible {
                                            vis_changed.set(true);
                                        }
                                        ui.colored_label(color, "\u{25a0}");
                                        ui.label(format!(
                                            "{} ({} tris)",
                                            face.name, face.triangles
                                        ));
                                    });
                                }
                            }
                        });
                    }
                });

                // Set visibility_changed flag if any checkbox was toggled.
                if vis_changed.get() {
                    state.visibility_changed = true;
                }
            }
        });

    // Track left panel width.
    let left_panel_width = panel_response.response.rect.width();

    let right_panel_response = egui::SidePanel::right("metadata")
        .resizable(true)
        .default_width(380.0)
        .show(ctx, |ui| {
            ui.heading("File Information");
            ui.separator();
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

    // Track right panel width.
    let right_panel_width = right_panel_response.response.rect.width();
    state.panel_width = left_panel_width;

    // Get window info for viewport overlay positioning.
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

        // Viewport toolbar (top-right of 3D viewport, not main window).
        if state.scene_data.is_some() {
            let toolbar_margin = 8.0;
            // Position relative to right edge of viewport (before the right panel).
            let toolbar_x = left_panel_width + viewport_width - toolbar_margin;
            // Below menu bar.
            let toolbar_y = toolbar_margin + 24.0;

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

                        // Random colors toggle (dice icon).
                        let colors_btn = ui.selectable_label(
                            state.show_random_colors,
                            egui::RichText::new("\u{1f3b2}").size(18.0),
                        );
                        if colors_btn.clicked() {
                            state.show_random_colors = !state.show_random_colors;
                            state.needs_mesh_rebuild = true;
                        }
                        colors_btn.on_hover_text("Random colors");

                        // Bounding box toggle (box icon).
                        let bbox_btn = ui.selectable_label(
                            state.show_bounding_box,
                            egui::RichText::new("\u{2b1c}").size(18.0),
                        );
                        if bbox_btn.clicked() {
                            state.show_bounding_box = !state.show_bounding_box;
                        }
                        bbox_btn.on_hover_text("Bounding box");

                        // Wireframe toggle (grid icon).
                        let wire_btn = ui.selectable_label(
                            state.show_wireframe,
                            egui::RichText::new("\u{25a6}").size(18.0),
                        );
                        if wire_btn.clicked() {
                            state.show_wireframe = !state.show_wireframe;
                        }
                        wire_btn.on_hover_text("Wireframe edges");
                    });
                });
        }

        if let Some(err) = &state.error {
            // Error overlay.
            egui::Area::new(egui::Id::new("error_overlay"))
                .fixed_pos(egui::pos2(viewport_x + 10.0, window_height - 40.0))
                .show(ctx, |ui| {
                    ui.colored_label(egui::Color32::RED, err);
                });
        } else if let Some(job) = &state.loading_job {
            // Progress bar at bottom of viewport.
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

                    // Background.
                    ui.painter().rect_filled(
                        rect,
                        4.0,
                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200),
                    );

                    // Progress bar fill.
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

                    // Text.
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

            // Request repaint to update progress.
            ctx.request_repaint();
        }
    }

    // Allow escape to quit quickly on desktop.
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        exit.write(AppExit::Success);
    }
}
