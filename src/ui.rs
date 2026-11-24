use crate::lan::{DeviceIcon, DeviceKind, DeviceStyle};
use crate::{Between, Command, Mode, Phase, Status};
use eframe::egui::{
    self, Align, Color32, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2,
};
use serde::{Deserialize, Serialize};
use std::{f32::consts::TAU, time::Duration};

const BLUE: Color32 = Color32::from_rgb(0, 112, 246);
const GREEN: Color32 = Color32::from_rgb(57, 190, 91);
const BACKDROP: u8 = 70;

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

struct Palette {
    motion: bool,
    panel: Color32,
    card: Color32,
    line: Color32,
    text: Color32,
    muted: Color32,
    tint: Color32,
    warning: Color32,
}

impl Palette {
    fn new(dark: bool) -> Self {
        let colors = if dark {
            [
                0x141619, 0x202328, 0x383d44, 0xf3f5f8, 0xabb4c1, 0x303641, 0xffc078,
            ]
        } else {
            [
                0xf5f6f8, 0xffffff, 0xdce0e6, 0x191e29, 0x545e70, 0xe8ebf1, 0x95520c,
            ]
        };
        let [panel, card, line, text, muted, tint, warning] =
            colors.map(|rgb| Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
        Self {
            motion: true,
            panel,
            card,
            line,
            text,
            muted,
            tint,
            warning,
        }
    }

    fn visuals(&self, dark: bool) -> egui::Visuals {
        let mut visuals = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = self.panel;
        visuals.window_fill = self.card;
        visuals.extreme_bg_color = self.panel;
        visuals.override_text_color = Some(self.text);
        visuals.selection.bg_fill = BLUE;
        visuals.selection.stroke.color = Color32::WHITE;
        for state in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
        ] {
            state.bg_fill = self.card;
            state.weak_bg_fill = self.card;
            state.bg_stroke = Stroke::new(1.0_f32, self.line);
            state.fg_stroke.color = self.text;
            state.corner_radius = 9.into();
        }
        visuals.widgets.hovered.weak_bg_fill = self.tint;
        visuals.widgets.active.weak_bg_fill = self.tint;
        visuals
    }
}

pub struct View<'a> {
    pub paired: bool,
    pub laptop: bool,
    pub busy: bool,
    pub receiver: &'a str,
    pub shown: Option<&'a Status>,
    pub detail: &'a str,
}

#[derive(Clone, Copy)]
enum Symbol {
    Right,
    Both,
    Sun,
    Moon,
    Gear,
    Pause,
    Sync,
    Tick,
}

fn symbol(painter: &egui::Painter, center: Pos2, size: f32, color: Color32, icon: Symbol) {
    let stroke = Stroke::new((size * 0.061).max(1.25), color);
    let point = |x: f32, y: f32| center + vec2(x, y) * size;
    let line = |a: (f32, f32), b: (f32, f32)| {
        painter.line_segment([point(a.0, a.1), point(b.0, b.1)], stroke);
    };
    let path = |coords: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            coords.iter().map(|&(x, y)| point(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        Symbol::Right | Symbol::Both => {
            line((-0.42, 0.0), (0.40, 0.0));
            path(&[(0.19, -0.22), (0.41, 0.0), (0.19, 0.22)]);
            if matches!(icon, Symbol::Both) {
                path(&[(-0.19, 0.22), (-0.41, 0.0), (-0.19, -0.22)]);
            }
        }
        Symbol::Sun => {
            painter.circle_stroke(center, size * 0.21, stroke);
            for index in 0..8 {
                let v = Vec2::angled(index as f32 * TAU / 8.0);
                painter.line_segment([center + v * size * 0.33, center + v * size * 0.46], stroke);
            }
        }
        Symbol::Moon => {
            path(&[
                (0.35, 0.15),
                (0.24, 0.37),
                (-0.03, 0.43),
                (-0.28, 0.32),
                (-0.42, 0.08),
                (-0.38, -0.18),
                (-0.23, -0.37),
                (0.04, -0.43),
                (-0.06, -0.23),
                (-0.03, 0.0),
                (0.14, 0.15),
                (0.35, 0.15),
            ]);
        }
        Symbol::Gear => {
            let outline = (0..=32)
                .map(|index| {
                    let angle = index as f32 * TAU / 32.0;
                    let radius = if matches!(index % 4, 1 | 2) {
                        0.46
                    } else {
                        0.35
                    };
                    center + Vec2::angled(angle) * size * radius
                })
                .collect();
            painter.add(egui::Shape::line(outline, stroke));
            painter.circle_stroke(center, size * 0.15, stroke);
        }
        Symbol::Pause => {
            line((-0.17, -0.33), (-0.17, 0.33));
            line((0.17, -0.33), (0.17, 0.33));
        }
        Symbol::Sync => {
            for offset in [0.0, TAU / 2.0] {
                let orbit = (0..=12)
                    .map(|i| center + Vec2::angled(offset + 0.5 + i as f32 * 0.18) * size * 0.39)
                    .collect();
                painter.add(egui::Shape::line(orbit, stroke));
                let head = center + Vec2::angled(offset + 2.66) * size * 0.39;
                let tangent = Vec2::angled(offset + 2.66 + TAU / 4.0);
                let normal = tangent.rot90();
                painter.add(egui::Shape::line(
                    vec![
                        head - tangent * size * 0.17 + normal * size * 0.13,
                        head,
                        head - tangent * size * 0.17 - normal * size * 0.13,
                    ],
                    stroke,
                ));
            }
        }
        Symbol::Tick => path(&[(-0.36, -0.02), (-0.12, 0.23), (0.37, -0.24)]),
    }
}

fn icon(ui: &mut egui::Ui, size: f32, color: Color32, shape: Symbol) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    symbol(ui.painter(), rect.center(), size, color, shape);
}

fn theme_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    label: &str,
    shape: Symbol,
    active: bool,
) -> bool {
    let response = ui
        .add(
            egui::Button::new("")
                .min_size(vec2(42.0, 34.0))
                .fill(if active { BLUE } else { palette.card }),
        )
        .on_hover_text(label);
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, active, label));
    let color = if active { Color32::WHITE } else { palette.text };
    symbol(ui.painter(), response.rect.center(), 20.0, color, shape);
    response.clicked()
}

fn header(app: &mut Between, ui: &mut egui::Ui, p: &Palette, dark: bool) {
    ui.horizontal(|ui| {
        let (tile, _) = ui.allocate_exact_size(Vec2::splat(33.0), Sense::hover());
        ui.painter().rect_filled(tile, 10, BLUE);
        for (x, y) in [(-3.3, -2.4), (3.3, 2.4)] {
            ui.painter().rect_stroke(
                Rect::from_center_size(tile.center() + vec2(x, y), vec2(14.0, 10.5)),
                3,
                Stroke::new(1.8_f32, Color32::WHITE),
                StrokeKind::Middle,
            );
        }
        ui.add_space(3.0);
        ui.label(RichText::new("B T W N").size(20.0).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme_button(ui, p, "Settings", Symbol::Gear, app.settings_open) {
                app.name_draft = app.settings.device_name.clone();
                app.style_draft = app.settings.device_style;
                app.settings_tab = SettingsTab::Device;
                app.settings_open = !app.settings_open;
            }
            let (label, icon) = if dark {
                ("Switch to light appearance", Symbol::Sun)
            } else {
                ("Switch to dark appearance", Symbol::Moon)
            };
            if theme_button(ui, p, label, icon, false) {
                app.settings.theme = if dark { Theme::Light } else { Theme::Dark };
                app.save();
            }
        });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Device,
    Transfers,
    Appearance,
}

fn settings_tabs(ui: &mut egui::Ui, p: &Palette, tab: &mut SettingsTab) {
    let (rail, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
    ui.painter().rect_filled(rail, 11, p.panel);
    let inner = rail.shrink(3.0);
    let width = inner.width() / 3.0;
    let cell = |index: f32| {
        Rect::from_min_size(
            inner.min + vec2(index * width, 0.0),
            vec2(width, inner.height()),
        )
    };
    let tabs = [
        (SettingsTab::Device, "My device"),
        (SettingsTab::Transfers, "Transfers"),
        (SettingsTab::Appearance, "Appearance"),
    ];
    let buttons = tabs.map(|(value, label)| {
        let response = ui.interact(
            cell(value as u8 as f32),
            ui.make_persistent_id(("settings-tab", label)),
            Sense::click(),
        );
        if response.clicked() {
            *tab = value;
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, *tab == value, label)
        });
        (response, value, label)
    });
    let target = *tab as u8 as f32;
    let index = if p.motion {
        ui.ctx()
            .animate_value_with_time(ui.id().with("tab-position"), target, 0.17)
    } else {
        target
    };
    let thumb = cell(index);
    ui.painter().rect_filled(
        thumb.translate(vec2(0.0, 1.0)),
        8,
        Color32::from_black_alpha(24),
    );
    ui.painter().rect_filled(thumb, 8, p.card);
    ui.painter()
        .rect_stroke(thumb, 8, Stroke::new(1.0_f32, p.line), StrokeKind::Inside);
    for (response, value, label) in buttons {
        let color = if *tab == value || response.hovered() {
            p.text
        } else {
            p.muted
        };
        ui.painter().text(
            response.rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(14.0),
            color,
        );
        if response.has_focus() {
            ui.painter().rect_stroke(
                response.rect,
                8,
                Stroke::new(1.5_f32, BLUE),
                StrokeKind::Inside,
            );
        }
    }
}

fn identity_settings(app: &mut Between, ui: &mut egui::Ui, p: &Palette, laptop: bool) {
    let fallback_name = crate::default_pc_name(laptop);
    let preview_name = if app.name_draft.trim().is_empty() {
        fallback_name
    } else {
        app.name_draft.trim()
    };
    let (preview, _) = ui.allocate_exact_size(vec2(ui.available_width(), 104.0), Sense::hover());
    let dark = ui.visuals().dark_mode;
    let stage = if dark {
        Color32::from_rgb(33, 44, 64)
    } else {
        Color32::from_rgb(233, 240, 252)
    };
    ui.painter().rect_filled(preview, 16, stage);
    let center = pos2(preview.right() - 67.0, preview.center().y);
    let wash = if dark {
        Color32::from_rgb(44, 65, 98)
    } else {
        Color32::from_rgb(214, 228, 250)
    };
    ui.painter()
        .circle_filled(center, 47.0, stage.lerp_to_gamma(wash, 0.5));
    ui.painter().circle_filled(center, 36.0, wash);
    let current = app.style_draft.icon();
    for shape in [
        DeviceIcon::Monitor,
        DeviceIcon::Notebook,
        DeviceIcon::Tower,
        DeviceIcon::Mini,
    ] {
        let selected = shape == current;
        let reveal = if p.motion {
            ui.ctx()
                .animate_bool_with_time(ui.id().with(("preview", shape as u8)), selected, 0.19)
        } else {
            f32::from(selected)
        };
        if reveal > 0.0 {
            let mut painter = ui.painter().clone();
            painter.multiply_opacity(reveal);
            device(
                &painter,
                center + vec2(0.0, (1.0 - reveal) * 4.0),
                shape,
                0.86 * (0.93 + 0.07 * reveal),
            );
        }
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        preview_name.to_owned(),
        egui::FontId::proportional(20.0),
        p.text,
    );
    job.wrap.max_width = (preview.width() - 151.0).max(60.0);
    job.wrap.max_rows = 1;
    let name = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(
        pos2(preview.left() + 19.0, preview.center().y - 22.0),
        name,
        p.text,
    );
    ui.painter().text(
        pos2(preview.left() + 19.0, preview.center().y + 8.0),
        egui::Align2::LEFT_TOP,
        "This PC",
        egui::FontId::proportional(12.5),
        p.muted,
    );
    ui.add_space(6.0);
    let width = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(width - 133.0, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                let label = ui.label(RichText::new("Device name").strong());
                ui.add_sized(
                    [ui.available_width(), 32.0],
                    egui::TextEdit::singleline(&mut app.name_draft)
                        .hint_text(fallback_name)
                        .margin(egui::Margin::symmetric(9, 6))
                        .vertical_align(Align::Center)
                        .char_limit(crate::lan::MAX_DEVICE_NAME),
                )
                .labelled_by(label.id);
            },
        );
        ui.vertical(|ui| {
            ui.label(RichText::new("Device type").strong());
            ui.spacing_mut().interact_size.y = 32.0;
            egui::ComboBox::from_id_salt("device-kind")
                .width(119.0)
                .selected_text(app.style_draft.label())
                .show_ui(ui, |ui| {
                    for choice in [None, Some(DeviceKind::Desktop), Some(DeviceKind::Laptop)] {
                        ui.selectable_value(
                            &mut app.style_draft.kind,
                            choice,
                            choice.map_or("PC", DeviceKind::label),
                        );
                    }
                });
        });
    });
    ui.add_space(6.0);
    ui.label(RichText::new("Device icon").strong());
    let icons = [
        (DeviceIcon::Auto, "Auto"),
        (DeviceIcon::Monitor, "Monitor"),
        (DeviceIcon::Notebook, "Notebook"),
        (DeviceIcon::Tower, "Tower"),
        (DeviceIcon::Mini, "Mini"),
    ];
    let gap = ui.spacing().item_spacing.x;
    let columns = if ui.available_width() >= 5.0 * 65.0 + 4.0 * gap {
        5
    } else {
        3
    };
    let tile_width = ((ui.available_width() + gap) / columns as f32 - gap).floor();
    for row in icons.chunks(columns) {
        ui.horizontal(|ui| {
            for &(choice, label) in row {
                let selected = choice == app.style_draft.icon;
                let button = egui::Button::new("")
                    .selected(selected)
                    .fill(if selected { stage } else { p.panel })
                    .corner_radius(10)
                    .stroke(Stroke::new(
                        if selected { 1.6_f32 } else { 1.0_f32 },
                        if selected { BLUE } else { p.line },
                    ));
                let r = ui.add_sized([tile_width, 65.0], button);
                r.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, label)
                });
                let shape = DeviceStyle {
                    icon: choice,
                    ..app.style_draft
                }
                .icon();
                let hover = if p.motion {
                    ui.ctx().animate_bool_with_time(
                        r.id.with("lift"),
                        r.hovered() || r.has_focus(),
                        0.15,
                    )
                } else {
                    0.0
                };
                device(
                    ui.painter(),
                    pos2(r.rect.center().x, r.rect.top() + 25.0 - 2.0 * hover),
                    shape,
                    (tile_width / 155.0).min(0.46) * (1.0 + hover * 0.07),
                );
                if selected {
                    let check = r.rect.right_top() + vec2(-9.5, 9.5);
                    ui.painter().circle_filled(check, 5.5, BLUE);
                    symbol(ui.painter(), check, 7.0, Color32::WHITE, Symbol::Tick);
                }
                ui.painter().text(
                    pos2(r.rect.center().x, r.rect.bottom() - 7.0),
                    egui::Align2::CENTER_BOTTOM,
                    label,
                    egui::FontId::proportional(12.0),
                    p.text,
                );
                if r.clicked() {
                    app.style_draft.icon = choice;
                }
            }
        });
    }
}

fn settings_window(app: &mut Between, context: &egui::Context, p: &Palette, view: &View<'_>) {
    let viewport = context.content_rect().size();
    let padding = if viewport.x < 420.0 || viewport.y < 500.0 {
        12
    } else {
        22
    };
    let inset = 2.0 * f32::from(padding) + 18.0;
    let modal = egui::Modal::new(egui::Id::new("device-settings"))
        .backdrop_color(Color32::from_black_alpha(BACKDROP))
        .frame(
            egui::Frame::popup(&context.style())
                .fill(p.card)
                .stroke(Stroke::new(1.0_f32, p.line))
                .corner_radius(20)
                .inner_margin(padding),
        )
        .show(context, |ui| {
            ui.set_width((viewport.x - inset).clamp(1.0, 445.0));
            ui.set_max_height((viewport.y - inset).max(1.0));
            ui.spacing_mut().item_spacing = vec2(7.0, 8.0);
            ui.spacing_mut().button_padding = vec2(9.0, 5.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").size(24.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("BTWN {}", env!("CARGO_PKG_VERSION")))
                            .size(12.0)
                            .color(p.muted),
                    );
                });
            });
            ui.add_space(2.0);
            settings_tabs(ui, p, &mut app.settings_tab);
            ui.add_space(3.0);
            egui::ScrollArea::vertical()
                .id_salt(("settings-content", app.settings_tab as u8))
                .min_scrolled_height(32.0)
                .max_height(
                    (viewport.y - inset - (ui.cursor().top() - ui.max_rect().top()) - 65.0)
                        .clamp(32.0, 340.0),
                )
                .auto_shrink([false, false])
                .show(ui, |ui| match app.settings_tab {
                    SettingsTab::Device => identity_settings(app, ui, p, view.laptop),
                    SettingsTab::Transfers => transfer_settings(app, ui, p, view),
                    SettingsTab::Appearance => appearance_settings(app, ui, p),
                });
            ui.add_space(5.0);
            ui.separator();
            let changed = app.settings.device_name != app.name_draft.trim()
                || app.settings.device_style != app.style_draft;
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), 33.0),
                Layout::right_to_left(Align::Center),
                |ui| {
                    let label = if changed { "Save changes" } else { "Done" };
                    if ui
                        .add_enabled(
                            !app.name_draft.chars().any(char::is_control),
                            egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                .fill(BLUE)
                                .min_size(vec2(88.0, 33.0)),
                        )
                        .clicked()
                    {
                        if changed {
                            app.settings.device_name = app.name_draft.trim().to_owned();
                            app.settings.device_style = app.style_draft;
                            app.save();
                            app.next_poll = std::time::Instant::now();
                        }
                        ui.close();
                    }
                    if changed && ui.button("Cancel").clicked() {
                        ui.close();
                    }
                },
            );
        });
    if modal.should_close() {
        app.settings_open = false;
    }
}

fn appearance_settings(app: &mut Between, ui: &mut egui::Ui, p: &Palette) {
    ui.label(RichText::new("Window appearance").strong());
    let width = (ui.available_width() - 2.0 * ui.spacing().item_spacing.x) / 3.0;
    ui.horizontal(|ui| {
        for (theme, label) in [
            (Theme::System, "Match Windows"),
            (Theme::Light, "Light"),
            (Theme::Dark, "Dark"),
        ] {
            let selected = app.settings.theme == theme;
            let (tile, response) = ui.allocate_exact_size(vec2(width, 89.0), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, label)
            });
            ui.painter().rect_filled(tile, 11, p.panel);
            ui.painter().rect_stroke(
                tile,
                11,
                Stroke::new(
                    if selected { 1.8_f32 } else { 1.0_f32 },
                    if selected || response.has_focus() {
                        BLUE
                    } else {
                        p.line
                    },
                ),
                StrokeKind::Inside,
            );
            let window = Rect::from_center_size(
                pos2(tile.center().x, tile.top() + 33.0),
                vec2(width.min(83.0) - 10.0, 42.0),
            );
            let light = Color32::from_rgb(235, 239, 247);
            let dark = Color32::from_rgb(39, 48, 65);
            let paint = ui.painter();
            paint.rect_filled(window, 5, if theme == Theme::Dark { dark } else { light });
            if theme == Theme::System {
                let right = Rect::from_min_max(pos2(window.center().x, window.top()), window.max);
                paint.with_clip_rect(right).rect_filled(window, 5, dark);
            }
            paint.rect_stroke(window, 5, Stroke::new(1.0_f32, p.line), StrokeKind::Inside);
            for n in 0..3 {
                paint.circle_filled(
                    window.min + vec2(6.0 + n as f32 * 4.0, 5.5),
                    1.05,
                    Color32::GRAY,
                );
            }
            let preview = window.shrink2(vec2(7.0, 0.0));
            paint.rect_filled(
                Rect::from_min_size(
                    preview.min + vec2(0.0, 14.0),
                    vec2(preview.width() * 0.68, 3.0),
                ),
                2,
                Color32::from_rgb(133, 153, 185),
            );
            paint.rect_filled(
                Rect::from_min_size(
                    preview.min + vec2(0.0, 24.0),
                    vec2(preview.width() * 0.44, 6.0),
                ),
                3,
                BLUE,
            );
            paint.text(
                pos2(tile.center().x, tile.bottom() - 10.0),
                egui::Align2::CENTER_BOTTOM,
                label,
                egui::FontId::proportional(13.0),
                p.text,
            );
            if response.clicked() && !selected {
                app.settings.theme = theme;
                app.save();
            }
        }
    });
    ui.add_space(12.0);
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Reduce motion").strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if toggle(ui, p, &mut app.settings.reduce_motion, "Reduce motion").changed() {
                app.save();
            }
        });
    });
    ui.label(
        RichText::new("Windows animation preferences also apply.")
            .size(12.5)
            .color(p.muted),
    );
}

fn transfer_settings(app: &mut Between, ui: &mut egui::Ui, p: &Palette, view: &View<'_>) {
    if view.paired
        && !view.laptop
        && ui.button("Export pairing file").clicked()
        && let Err(error) = app.export_pair()
    {
        app.error = true;
        app.status = format!("{error:#}");
    }
    ui.label(RichText::new("Receive project files").strong());
    let destination = app.settings.projects_folder.clone().or_else(|| {
        crate::codex::home()
            .ok()
            .and_then(|home| crate::backup::project_destination(&home).ok())
    });
    if let Some(path) = destination {
        let label = crate::storage::windows_path(&path);
        egui::Frame::new()
            .fill(p.panel)
            .corner_radius(10)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(&label).color(p.text)).truncate())
                    .on_hover_text(label);
            });
    }
    ui.label(
        RichText::new("BTWN saves newly received projects here.")
            .size(12.5)
            .color(p.muted),
    );
    ui.add_enabled_ui(
        !view.busy
            && !view
                .shown
                .is_some_and(|state| matches!(state.phase, Phase::Applying | Phase::Receiving)),
        |ui| {
            ui.horizontal(|ui| {
                if ui.button("Choose folder").clicked() {
                    let mut dialog =
                        rfd::FileDialog::new().set_title("Choose where to save new projects");
                    if let Some(path) = &app.settings.projects_folder {
                        dialog = dialog.set_directory(path);
                    }
                    if let Some(path) = dialog.pick_folder() {
                        app.settings.projects_folder = Some(path);
                        app.save();
                    }
                }
                if ui
                    .add_enabled(
                        app.settings.projects_folder.is_some(),
                        egui::Button::new("Use default"),
                    )
                    .clicked()
                {
                    app.settings.projects_folder = None;
                    app.save();
                }
            });
        },
    );
    if let Ok(paths) = &app.paths {
        let received = paths.home.join("Received");
        if ui
            .add_enabled(received.is_dir(), egui::Button::new("Open received files"))
            .clicked()
        {
            if let Err(error) = std::process::Command::new("explorer.exe")
                .arg(received)
                .spawn()
            {
                app.status = format!("Could not open received files. {error}");
                app.error = true;
            }
        }
    }
    ui.separator();
    ui.label(RichText::new("Network").strong());
    ui.label(
        RichText::new(format!("Local network · port {}", crate::discovery::PORT)).color(p.muted),
    );
    if view.laptop {
        ui.horizontal(|ui| {
            ui.label("Discovery timeout");
            let mut timeout = app
                .settings
                .discovery_timeout_ms
                .unwrap_or(crate::discovery::DEFAULT_TIMEOUT_MS);
            if ui
                .add(
                    egui::DragValue::new(&mut timeout)
                        .range(500..=5000)
                        .speed(50.0)
                        .suffix(" ms"),
                )
                .changed()
            {
                app.settings.discovery_timeout_ms = Some(timeout);
                app.save();
            }
        });
    }
    ui.label(
        RichText::new(if view.paired {
            "Paired connection · Encrypted · Checksums verified"
        } else {
            "Pair a computer to enable encrypted transfers."
        })
        .color(p.muted),
    );
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.line))
        .corner_radius(14)
        .inner_margin(egui::Margin::same(17))
}

// Paint the two devices at the current display scale, including the curved wallpaper.
fn device(painter: &egui::Painter, center: Pos2, shape: DeviceIcon, scale: f32) {
    let point = |x: f32, y: f32| center + vec2(x, y) * scale;
    let area =
        |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(point(x, y), vec2(w, h) * scale);
    let metal = Color32::from_rgb(169, 182, 202);
    let edge = Color32::from_rgb(91, 104, 125);
    if matches!(shape, DeviceIcon::Tower | DeviceIcon::Mini) {
        let tower = shape == DeviceIcon::Tower;
        let (w, h) = if tower { (35.0, 59.0) } else { (67.0, 23.0) };
        let frame = Rect::from_center_size(center, vec2(w, h) * scale);
        painter.add(egui::Shape::ellipse_filled(
            pos2(center.x, frame.bottom() + 2.0 * scale),
            vec2(w * 0.56, 3.5) * scale,
            Color32::from_black_alpha(30),
        ));
        painter.rect_filled(frame, 5, metal);
        painter.rect_stroke(frame, 5, Stroke::new(scale, edge), StrokeKind::Inside);
        let face = frame.shrink(3.0 * scale);
        painter.rect_filled(face, 3, Color32::from_rgb(65, 77, 99));
        if tower {
            for line in 0..4 {
                let y = face.top() + (5.0 + line as f32 * 3.2) * scale;
                painter.line_segment(
                    [
                        pos2(face.left() + 4.0 * scale, y),
                        pos2(face.right() - 4.0 * scale, y),
                    ],
                    Stroke::new(1.15 * scale, edge),
                );
            }
        }
        painter.circle_filled(
            face.right_bottom() - vec2(5.0, 5.0) * scale,
            1.7 * scale,
            Color32::from_rgb(129, 209, 255),
        );
        return;
    }
    let laptop = shape == DeviceIcon::Notebook;
    let bottom = if laptop { 23.0 } else { 33.0 };
    painter.add(egui::Shape::ellipse_filled(
        point(0.0, bottom + 2.0),
        vec2(44.0, 3.0) * scale,
        Color32::from_black_alpha(28),
    ));
    if !laptop {
        painter.rect_filled(area(-5.0, 17.0, 10.0, 15.0), 1, metal);
        painter.rect_filled(area(-16.0, 30.0, 32.0, 3.0), 2, edge);
    }
    let width = if laptop { 81.0 } else { 94.0 };
    let height = if laptop { 47.0 } else { 57.0 };
    let frame = area(-width / 2.0, 20.0 - height, width, height);
    painter.rect_filled(frame, 5, Color32::from_rgb(26, 34, 48));
    painter.rect_stroke(
        frame,
        5,
        Stroke::new(1.3 * scale, metal),
        StrokeKind::Inside,
    );
    let screen = frame.shrink(3.1 * scale);
    let glass = painter.with_clip_rect(screen);
    glass.rect_filled(screen, 0, Color32::from_rgb(42, 47, 112));
    let bands = if laptop {
        [
            (80, 70, 172),
            (121, 107, 216),
            (171, 162, 247),
            (64, 100, 191),
            (33, 65, 137),
        ]
    } else {
        [
            (53, 83, 166),
            (86, 133, 222),
            (144, 194, 251),
            (65, 106, 202),
            (53, 67, 156),
        ]
    };
    for (index, (r, g, b)) in bands.into_iter().enumerate() {
        let mut mesh = egui::Mesh::default();
        let color = Color32::from_rgb(r, g, b);
        for step in 0..=32 {
            let t = step as f32 / 32.0;
            let y = 0.03 + 0.115 * index as f32 + 0.55 * (t * 4.6 + index as f32 * 0.19).sin()
                - 0.21 * t;
            let x = screen.left() + screen.width() * t;
            mesh.colored_vertex(pos2(x, screen.top() + y * screen.height()), color);
            mesh.colored_vertex(pos2(x, screen.bottom() + screen.height()), color);
            if step > 0 {
                let i = 2 * step;
                mesh.add_triangle(i - 2, i - 1, i);
                mesh.add_triangle(i, i - 1, i + 1);
            }
        }
        glass.add(mesh);
    }
    if laptop {
        painter.add(egui::Shape::convex_polygon(
            vec![
                point(-47.0, 20.0),
                point(47.0, 20.0),
                point(42.0, 24.0),
                point(-42.0, 24.0),
            ],
            metal,
            Stroke::NONE,
        ));
        painter.line_segment(
            [point(-11.0, 21.0), point(11.0, 21.0)],
            Stroke::new(scale, edge),
        );
    }
}

fn connection(app: &Between, ui: &mut egui::Ui, p: &Palette, view: &View<'_>) {
    let peer = app
        .delivery
        .peer_name
        .as_deref()
        .unwrap_or(crate::default_pc_name(!view.laptop));
    let found = view.paired && app.link_error.is_none() && app.delivery.peer_found;
    let mode = app
        .delivery
        .control
        .mode
        .filter(|_| {
            view.busy
                || view
                    .shown
                    .is_some_and(|s| !matches!(s.phase, Phase::Done | Phase::Failed))
        })
        .unwrap_or(app.mode);
    let dark = ui.visuals().dark_mode;
    let stage = if dark {
        Color32::from_rgb(31, 38, 53)
    } else {
        Color32::from_rgb(232, 238, 249)
    };
    let stage_line = if dark {
        Color32::from_rgb(61, 72, 91)
    } else {
        Color32::from_rgb(204, 215, 232)
    };
    let compact = ui.available_width() < 365.0 || ui.ctx().content_rect().height() < 500.0;
    let height = if compact { 153.0 } else { 181.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let paint = ui.painter();
    paint.rect_filled(rect, 19, stage);
    paint.rect_stroke(
        rect,
        19,
        Stroke::new(1.0_f32, stage_line),
        StrokeKind::Inside,
    );
    paint.text(
        rect.min + vec2(18.0, 18.0),
        egui::Align2::LEFT_TOP,
        "Your devices",
        egui::FontId::proportional(17.0),
        p.text,
    );
    let (caption, dot_color) = if found {
        ("Connected", GREEN)
    } else if view.paired {
        ("Searching", p.warning)
    } else {
        ("Unpaired", p.muted)
    };
    let chip_text = paint.layout_no_wrap(caption.into(), egui::FontId::proportional(12.0), p.text);
    let chip = Rect::from_min_size(
        pos2(rect.right() - chip_text.size().x - 44.0, rect.top() + 14.0),
        vec2(chip_text.size().x + 27.0, 25.0),
    );
    paint.rect_filled(chip, 14, p.card);
    let light = pos2(chip.left() + 10.0, chip.center().y);
    let animate = p.motion && ui.is_rect_visible(rect) && !app.settings_open;
    if animate && view.paired && !found {
        let t = ui.input(|i| (i.time / 2.2).fract() as f32);
        paint.circle_stroke(
            light,
            3.3 + 4.0 * t,
            Stroke::new(1.0_f32, dot_color.gamma_multiply(0.6 * (1.0 - t))),
        );
        ui.ctx().request_repaint_after(Duration::from_millis(40));
    }
    paint.circle_filled(light, 3.3, dot_color);
    paint.galley(
        pos2(
            chip.left() + 18.0,
            chip.center().y - chip_text.size().y / 2.0,
        ),
        chip_text,
        p.text,
    );
    let center = pos2(
        rect.center().x,
        rect.top() + if compact { 77.0 } else { 92.0 },
    );
    let gap = (rect.width() * 0.25).min(145.0);
    let scale = (rect.width() / 425.0).clamp(0.65, if compact { 0.82 } else { 1.05 });
    paint.line_segment(
        [
            center - vec2(gap - 51.0 * scale, 0.0),
            center + vec2(gap - 51.0 * scale, 0.0),
        ],
        Stroke::new(1.0_f32, stage_line),
    );
    if animate
        && found
        && !app.pause.active()
        && view.shown.is_some_and(|s| s.phase == Phase::Receiving)
    {
        let t = ui.input(|i| (i.time / 1.45).fract() as f32);
        let span = gap - 52.0 * scale;
        let packet = |offset: f32| {
            let at = center + vec2(span * (2.0 * offset - 1.0), 0.0);
            paint.circle_filled(at, 5.0, BLUE.gamma_multiply(0.18));
            paint.circle_filled(at, 2.3, BLUE);
        };
        packet(t);
        if mode == Mode::Automatic {
            packet(1.0 - t);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(24));
    }
    paint.circle_filled(center, 15.0, p.card);
    symbol(
        paint,
        center,
        15.0,
        p.muted,
        if mode == Mode::Automatic {
            Symbol::Both
        } else {
            Symbol::Right
        },
    );
    for (is_laptop, x) in [
        (mode == Mode::LaptopToDesktop, center.x - gap),
        (mode != Mode::LaptopToDesktop, center.x + gap),
    ] {
        let here = is_laptop == view.laptop;
        let name = if here {
            app.device_name(view.laptop)
        } else {
            peer
        };
        let style = if here {
            app.settings.device_style
        } else {
            app.delivery.peer_style
        };
        device(paint, pos2(x, center.y), style.icon(), scale);
        let mut job = egui::text::LayoutJob::simple_singleline(
            name.to_owned(),
            egui::FontId::proportional(15.0),
            p.text,
        );
        job.wrap.max_width = gap * 1.8;
        job.wrap.max_rows = 1;
        let text = ui.fonts_mut(|f| f.layout_job(job));
        paint.galley(
            pos2(x - text.size().x / 2.0, rect.bottom() - 49.0),
            text,
            p.text,
        );
        let role = if here { "This PC" } else { "Paired PC" };
        paint.text(
            pos2(x, rect.bottom() - 26.0),
            egui::Align2::CENTER_TOP,
            role,
            egui::FontId::proportional(12.0),
            p.muted,
        );
    }
    response.on_hover_text(format!("{} / {peer}", app.device_name(view.laptop)));
}

fn direction(app: &mut Between, ui: &mut egui::Ui, p: &Palette, laptop: bool) {
    ui.label(RichText::new("Sync direction").size(16.0).strong());
    egui::Frame::new()
        .fill(p.tint)
        .corner_radius(11)
        .inner_margin(4)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            let width = (ui.available_width() - 6.0) / 3.0;
            ui.horizontal(|ui| {
                for (mode, label) in [
                    (
                        Mode::DesktopToLaptop,
                        if laptop { "Receive" } else { "Send" },
                    ),
                    (
                        Mode::LaptopToDesktop,
                        if laptop { "Send" } else { "Receive" },
                    ),
                    (Mode::Automatic, "Automatic"),
                ] {
                    let active = app.mode == mode;
                    let button =
                        egui::Button::new(RichText::new(label).size(14.0).color(if active {
                            p.text
                        } else {
                            p.muted
                        }))
                        .selected(active)
                        .corner_radius(8)
                        .fill(if active { p.card } else { Color32::TRANSPARENT })
                        .stroke(if active {
                            Stroke::new(1.0_f32, p.line)
                        } else {
                            Stroke::NONE
                        });
                    if ui.add_sized([width, 33.0], button).clicked() {
                        app.mode = mode;
                        app.chosen = true;
                    }
                }
            });
        });
}

fn toggle(ui: &mut egui::Ui, p: &Palette, value: &mut bool, label: &str) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(47.0, 25.0), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, label)
    });
    let fraction = if p.motion {
        ui.ctx().animate_bool_responsive(response.id, *value)
    } else {
        f32::from(*value)
    };
    ui.painter().rect_filled(
        rect,
        13,
        if *value && ui.is_enabled() {
            BLUE
        } else {
            p.line
        },
    );
    let x = egui::lerp(rect.left() + 13.0..=rect.right() - 13.0, fraction);
    ui.painter()
        .circle_filled(pos2(x, rect.center().y), 9.2, Color32::WHITE);
    response
}

fn primary(ui: &mut egui::Ui, p: &Palette, label: &str, shape: Symbol) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let enabled = ui.is_enabled();
    let fill = if !enabled {
        p.tint
    } else if response.hovered() || response.has_focus() {
        Color32::from_rgb(0, 92, 214)
    } else {
        BLUE
    };
    let fg = if enabled { Color32::WHITE } else { p.muted };
    ui.painter().rect_filled(rect, 12, fill);
    let font = egui::FontId::proportional(18.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let left = rect.center().x - (galley.size().x + 28.0) * 0.5;
    symbol(
        ui.painter(),
        pos2(left + 9.0, rect.center().y),
        19.0,
        fg,
        shape,
    );
    ui.painter().galley(
        pos2(left + 28.0, rect.center().y - galley.size().y * 0.5),
        galley,
        fg,
    );
    response.clicked()
}

fn progress(ui: &mut egui::Ui, p: &Palette, state: &Status, paused: bool) {
    if let Some((done, total)) = state.counts.filter(|(_, total)| *total > 0) {
        let fraction = (done as f64 / total as f64).clamp(0.0, 1.0);
        let percent = (fraction * 100.0).round() as u32;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Transfer progress").size(13.0).color(p.muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{percent}%")).size(15.0).strong());
            });
        });
        ui.add(
            egui::ProgressBar::new(fraction as f32)
                .desired_height(6.0)
                .fill(BLUE),
        );
    } else {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 7.0), Sense::hover());
        ui.painter().rect_filled(rect, 4, p.tint);
        let width = rect.width() * 0.23;
        let travel = if paused || !p.motion {
            0.5
        } else {
            ui.input(|input| (1.0 - (input.time as f32 * 2.8).cos()) * 0.5)
        };
        let start = rect.left() + (rect.width() - width) * travel;
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(start, rect.top()), vec2(width, rect.height())),
            4,
            BLUE,
        );
        if !paused && p.motion {
            ui.ctx().request_repaint_after(Duration::from_millis(32));
        }
    }
}

fn completed(app: &Between, view: &View<'_>) -> Option<(String, Color32)> {
    let state = view.shown.filter(|state| state.phase == Phase::Done)?;
    let automatic = app.delivery.control.mode == Some(Mode::Automatic);
    let outcomes = if automatic {
        let directions = [&app.delivery.incoming, &app.delivery.outgoing];
        let finished = directions
            .iter()
            .map(|(offer, receipt)| {
                let stage = crate::matching_delivery(offer.as_ref()?, receipt)?;
                (stage.phase == Phase::Done).then_some(stage)
            })
            .collect::<Option<Vec<_>>>();
        let Some(finished) = finished else {
            return Some(("One direction verified".into(), BLUE));
        };
        finished
    } else {
        vec![state]
    };
    let review = outcomes.iter().any(|s| {
        s.outcome
            .as_ref()
            .is_some_and(|o| o.chat_conflicts + o.project_conflicts > 0)
            || s.detail.contains("kept separately")
            || s.detail.contains("saved separately")
    });
    if review {
        return Some((
            "Sync finished with items to review".into(),
            Color32::TRANSPARENT,
        ));
    }
    let verified = outcomes
        .iter()
        .all(|s| s.outcome.as_ref().is_some_and(|o| o.sidebar_verified));
    let title = if !verified {
        "Transfer finished"
    } else if app.delivery.control.projects {
        "Everything selected is synced"
    } else {
        "Chats and sidebar are synced"
    };
    Some((title.into(), GREEN))
}

fn status(app: &Between, ui: &mut egui::Ui, p: &Palette, view: &View<'_>) {
    let state = view.shown;
    let phase = state.map(|s| s.phase);
    let paused = app.pause.active();
    let completion = completed(app, view);
    let failed = app.error || app.link_error.is_some() || phase == Some(Phase::Failed);
    let report = paused
        || failed
        || completion.is_some()
        || phase.is_some()
        || app.delivery.control.mode.is_some()
        || !view.paired;
    if !report {
        ui.horizontal(|ui| {
            dot(
                ui,
                if app.delivery.peer_found {
                    GREEN
                } else {
                    p.muted
                },
            );
            ui.label(
                RichText::new(if app.delivery.peer_found {
                    "Ready to sync"
                } else {
                    "Open BTWN on both computers to connect."
                })
                .size(14.0)
                .color(p.muted),
            );
        });
        return;
    }
    let (title, color) = if paused {
        ("Sync paused".into(), p.warning)
    } else if failed {
        ("Sync needs attention".into(), p.warning)
    } else if let Some((label, color)) = &completion {
        (
            label.clone(),
            if *color == Color32::TRANSPARENT {
                p.warning
            } else {
                *color
            },
        )
    } else {
        let label = match phase {
            Some(Phase::Preparing) => "Checking for changes".into(),
            Some(Phase::Ready) => format!("Waiting for {}", view.receiver),
            Some(Phase::Waiting) => "ChatGPT is still open".into(),
            Some(Phase::Receiving) => format!("{} is receiving", view.receiver),
            Some(Phase::Applying) => format!("{} is updating chats", view.receiver),
            _ => "Status".into(),
        };
        (label, BLUE)
    };
    card(p).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            if completion.is_some() && color == GREEN {
                icon(ui, 17.0, GREEN, Symbol::Tick);
            } else {
                dot(ui, color);
            }
            ui.add(egui::Label::new(RichText::new(title).size(16.0).strong()).wrap());
        });
        ui.add_space(3.0);
        let instruction = if paused {
            "Sync is paused on both computers."
        } else if completion.as_ref().is_some_and(|(_, color)| *color == BLUE) {
            "Waiting for the other direction to finish."
        } else if completion.is_some() && !failed {
            "You can close BTWN and open ChatGPT."
        } else {
            view.detail
        };
        ui.label(RichText::new(instruction).color(p.muted));
        if !paused && !failed && completion.is_some() {
            ui.label(RichText::new(view.detail).color(p.muted).size(12.5));
        }
        if let Some(state) = state.filter(|s| {
            matches!(
                s.phase,
                Phase::Preparing | Phase::Receiving | Phase::Applying
            )
        }) {
            ui.add_space(8.0);
            progress(ui, p, state, paused);
        }
    });
}

fn transfer_active(view: &View<'_>) -> bool {
    view.shown.is_some_and(|s| {
        matches!(
            s.phase,
            Phase::Preparing | Phase::Receiving | Phase::Applying
        )
    })
}

fn sync_options(app: &mut Between, ui: &mut egui::Ui, p: &Palette, view: &View<'_>) {
    if !view.paired {
        return;
    }
    let editable =
        app.command.is_none() && !view.busy && !transfer_active(view) && !app.pause.active();
    ui.add_enabled_ui(editable, |ui| {
        direction(app, ui, p, view.laptop);
        ui.add_space(6.0);
        ui.separator();
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            let label_width = (ui.available_width() - 47.0 - ui.spacing().item_spacing.x).max(1.0);
            ui.allocate_ui_with_layout(
                vec2(label_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(label_width);
                    ui.label(RichText::new("Include project files").size(16.0).strong());
                    ui.add(
                        egui::Label::new(
                            RichText::new("Copy missing files and preserve conflicting versions.")
                                .size(12.5)
                                .color(p.muted),
                        )
                        .wrap(),
                    );
                },
            );
            if toggle(ui, p, &mut app.settings.projects, "Include project files").changed() {
                app.chosen = true;
                app.save();
            }
        });
    });
}

fn sync_action(app: &mut Between, ui: &mut egui::Ui, p: &Palette, view: &View<'_>) {
    if !view.paired {
        if app.paths.is_err() {
            return;
        }
        let result = if primary(ui, p, "Create pairing file", Symbol::Right) {
            app.export_pair()
        } else if ui.button("Open pairing file").clicked() {
            app.pair()
        } else {
            Ok(())
        };
        if let Err(error) = result {
            app.status = format!("{error:#}");
            app.error = true;
        }
        return;
    }
    let paused = app.pause.active();
    let transferring = transfer_active(view);
    let available = app.command.is_none() && app.link_error.is_none() && app.delivery.peer_found;
    let start_ready = !paused && !view.busy && !transferring;
    let can_pause = app.delivery.control.mode.is_some()
        && (paused
            || transferring
            || view.busy
            || app.delivery.control.mode == Some(Mode::Automatic)
            || view
                .shown
                .is_some_and(|s| matches!(s.phase, Phase::Ready | Phase::Waiting)));
    if can_pause && !start_ready {
        ui.add_enabled_ui(available, |ui| {
            let (label, icon) = if paused {
                ("Resume sync", Symbol::Right)
            } else {
                ("Pause sync", Symbol::Pause)
            };
            if primary(ui, p, label, icon) {
                app.set_control(ui.ctx(), Command::SetPaused(!paused));
            }
        });
    } else {
        ui.add_enabled_ui(available && start_ready, |ui| {
            if primary(ui, p, "Start sync", Symbol::Sync) {
                app.set_control(
                    ui.ctx(),
                    Command::Start {
                        mode: app.mode,
                        projects: app.settings.projects,
                    },
                );
            }
        });
        if can_pause {
            ui.vertical_centered(|ui| {
                if ui
                    .add_enabled(available, egui::Button::new("Pause sync"))
                    .clicked()
                {
                    app.set_control(ui.ctx(), Command::SetPaused(true));
                }
            });
        }
    }
}

fn dashboard(app: &mut Between, ui: &mut egui::Ui, p: &Palette, view: &View<'_>, dark: bool) {
    let wide = ui.available_width() >= 760.0;
    let short = ui.available_height() < 540.0;
    ui.spacing_mut().item_spacing = vec2(7.0, 6.0);
    header(app, ui, p, dark);
    ui.add_space(if short { 6.0 } else { 16.0 });
    let dock_action = short && !wide && (view.paired || app.paths.is_ok());
    if dock_action {
        egui::TopBottomPanel::bottom("compact-sync-action")
            .frame(egui::Frame::new().fill(p.panel))
            .show_inside(ui, |ui| {
                ui.add_space(5.0);
                sync_action(app, ui, p, view);
            });
    }
    egui::ScrollArea::vertical()
        .id_salt(("dashboard-body", wide))
        .min_scrolled_height(24.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if wide {
                ui.spacing_mut().item_spacing.x = 20.0;
                ui.columns(2, |cols| {
                    connection(app, &mut cols[0], p, view);
                    cols[0].add_space(8.0);
                    status(app, &mut cols[0], p, view);
                    card(p).show(&mut cols[1], |ui| {
                        ui.spacing_mut().item_spacing.x = 7.0;
                        sync_options(app, ui, p, view);
                        if view.paired {
                            ui.add_space(14.0);
                        }
                        sync_action(app, ui, p, view);
                    });
                });
            } else {
                connection(app, ui, p, view);
                ui.add_space(8.0);
                status(app, ui, p, view);
                ui.add_space(if short { 6.0 } else { 14.0 });
                sync_options(app, ui, p, view);
                if !dock_action {
                    if view.paired {
                        ui.add_space(14.0);
                    }
                    sync_action(app, ui, p, view);
                }
            }
        });
}

pub fn draw(app: &mut Between, context: &egui::Context, _frame: &eframe::Frame, view: View<'_>) {
    if app.applied_theme != Some(app.settings.theme) {
        let theme = match app.settings.theme {
            Theme::System => egui::SystemTheme::SystemDefault,
            Theme::Light => egui::SystemTheme::Light,
            Theme::Dark => egui::SystemTheme::Dark,
        };
        context.send_viewport_cmd(egui::ViewportCommand::SetTheme(theme));
        app.applied_theme = Some(app.settings.theme);
    }
    let dark = match app.settings.theme {
        Theme::Light => false,
        Theme::Dark => true,
        Theme::System => context.system_theme().unwrap_or(egui::Theme::Dark) == egui::Theme::Dark,
    };
    let mut p = Palette::new(dark);
    p.motion = !app.settings.reduce_motion;
    #[cfg(windows)]
    {
        let caption = if app.settings_open {
            p.panel
                .lerp_to_gamma(Color32::BLACK, f32::from(BACKDROP) / 255.0)
        } else {
            p.panel
        };
        p.motion &= !app.window_appearance.update(_frame, caption, p.text);
    }
    let motion_time = if p.motion { 0.17 } else { 0.0 };
    if context.style().animation_time != motion_time {
        context.all_styles_mut(|style| style.animation_time = motion_time);
    }
    context.set_visuals_of(
        if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        },
        p.visuals(dark),
    );
    if app.settings.theme == Theme::System {
        context.set_theme(egui::ThemePreference::System);
    } else {
        context.set_theme(if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
    }
    let viewport = context.content_rect().size();
    let margin = if viewport.x < 430.0 || viewport.y < 500.0 {
        12
    } else {
        25
    };
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(p.panel).inner_margin(margin))
        .show(context, |ui| {
            let available = ui.available_width();
            let width = available.min(if available >= 760.0 { 1060.0 } else { 620.0 });
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    vec2(width, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(width);
                        dashboard(app, ui, &p, &view, dark);
                    },
                );
            });
        });
    if app.settings_open {
        settings_window(app, context, &p, &view);
    }
}

pub fn window_icon() -> egui::IconData {
    let pixels = crate::brand::pixels(64);
    egui::IconData {
        rgba: pixels,
        width: 64,
        height: 64,
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn controls_fit_after_resizing_the_dashboard_and_settings() {
        let mut app = Between::from_paths(Err(anyhow::anyhow!("Offline UI fixture")));
        let context = egui::Context::default();
        let frame = eframe::Frame::_new_kittest();
        app.settings.reduce_motion = true;
        app.delivery.peer_found = true;
        app.delivery.control.mode = Some(Mode::DesktopToLaptop);
        for (width, height, joined_pair) in [
            (548.0, 640.0, false),
            (320.0, 320.0, true),
            (360.0, 680.0, false),
            (800.0, 480.0, true),
            (1280.0, 500.0, false),
        ] {
            let screen = Rect::from_min_size(Pos2::ZERO, vec2(width, height));
            for (tab, phase, paused, expected) in [
                (None, Phase::Done, false, "Start sync"),
                (None, Phase::Receiving, false, "Pause sync"),
                (None, Phase::Receiving, true, "Resume sync"),
                (Some(SettingsTab::Device), Phase::Done, false, "My device"),
                (
                    Some(SettingsTab::Transfers),
                    Phase::Done,
                    false,
                    "Receive project files",
                ),
                (
                    Some(SettingsTab::Appearance),
                    Phase::Done,
                    false,
                    "Window appearance",
                ),
            ] {
                let status = Status::new("layout", phase, "Checking the selected changes.");
                app.pause.set(paused);
                app.settings_open = tab.is_some();
                if let Some(tab) = tab {
                    app.settings_tab = tab;
                }
                let mut output = egui::FullOutput::default();
                for _ in 0..5 {
                    output = context.run(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        },
                        |ctx| {
                            draw(
                                &mut app,
                                ctx,
                                &frame,
                                View {
                                    paired: true,
                                    laptop: joined_pair,
                                    busy: false,
                                    receiver: "PC 2",
                                    shown: Some(&status),
                                    detail: &status.detail,
                                },
                            );
                        },
                    );
                }
                let label = |name: &str| {
                    output
                        .shapes
                        .iter()
                        .find_map(|clipped| {
                            if let egui::Shape::Text(t) = &clipped.shape
                                && t.galley.job.text == name
                            {
                                let rect = Rect::from_min_size(t.pos, t.galley.size());
                                assert!(
                                    screen
                                        .intersect(clipped.clip_rect)
                                        .expand(1.0)
                                        .contains_rect(rect),
                                    "{name} clipped at {screen:?}, text={rect:?}, clip={:?}",
                                    clipped.clip_rect
                                );
                                return Some(rect);
                            }
                            None
                        })
                        .unwrap_or_else(|| panic!("{name} is missing at {screen:?}"))
                };
                label(expected);
                if app.settings_open {
                    label("Done");
                } else {
                    let devices = label("Your devices");
                    label("PC 1");
                    label("PC 2");
                    if width >= 810.0 {
                        assert!(label("Sync direction").left() > devices.right());
                    }
                }
            }
        }
    }
}
