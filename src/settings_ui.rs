//! The settings window — an ordinary window, run as `wl-dropdown settings`.
//!
//! A separate process on purpose: the terminal's surface is an overlay layer
//! that holds the keyboard, so the two could never share the screen anyway.
//!
//! Controls write straight through to [`Settings`], which debounces the disk
//! write and then tells a running terminal to reload, so changes show up
//! live.

use std::time::Instant;

use eframe::egui;
use wl_dropdown::color::Rgb;
use wl_dropdown::colorscheme;
use wl_dropdown::paths::APP_NAME;
use wl_dropdown::settings::{self, Settings, Source};

pub fn run() -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{APP_NAME} settings"))
            .with_app_id(format!("{APP_NAME}-settings"))
            .with_inner_size([520.0, 600.0])
            .with_min_inner_size([440.0, 340.0]),
        ..Default::default()
    };
    eframe::run_native(
        &format!("{APP_NAME} settings"),
        options,
        Box::new(|_| Ok(Box::new(SettingsApp::new()))),
    )
    .map_err(|e| e.to_string())
}

/// Text fields commit when editing finishes (Enter or focus leaving), so a
/// half-typed font name never reaches the terminal. Each keeps its own
/// buffer, refreshed from settings whenever it is not being edited.
#[derive(Default)]
struct Field {
    buffer: String,
}

impl Field {
    /// Shows the field; returns the text to commit when editing finished.
    fn show(&mut self, ui: &mut egui::Ui, current: &str, hint: &str) -> Option<String> {
        let id = ui.next_auto_id();
        if !ui.memory(|m| m.has_focus(id)) {
            self.buffer = current.to_owned();
        }
        let r = ui.add(
            egui::TextEdit::singleline(&mut self.buffer)
                .hint_text(hint)
                .desired_width(f32::INFINITY),
        );
        (r.lost_focus() && self.buffer != current).then(|| self.buffer.clone())
    }
}

struct SettingsApp {
    settings: Settings,
    schemes: Vec<String>,
    font: Field,
    shell: Field,
    previous: Field,
    next: Field,
    move_left: Field,
    move_right: Field,
}

impl SettingsApp {
    fn new() -> Self {
        Self {
            settings: Settings::load(),
            schemes: colorscheme::available(),
            font: Field::default(),
            shell: Field::default(),
            previous: Field::default(),
            next: Field::default(),
            move_left: Field::default(),
            move_right: Field::default(),
        }
    }
}

/// Says which layer a value came from and, for a hand-picked override,
/// offers to drop it so the key follows the desktop theme again. This is the
/// whole "follow the theme?" control: there is no separate flag.
fn source_tag(ui: &mut egui::Ui, source: Source) -> bool {
    let text = match source {
        // "revert" rather than "use theme": without a theme layer this falls
        // back to the preset or the built-in value.
        Source::User => "custom — revert",
        Source::Theme => "desktop theme",
        Source::Baseline => "preset",
        Source::Scheme => "colour scheme",
        Source::Builtin => "default",
    };
    let small = egui::RichText::new(text).small();
    if source == Source::User {
        ui.link(small).clicked()
    } else {
        ui.label(small.weak());
        false
    }
}

fn colour_swatch(ui: &mut egui::Ui, value: Rgb) -> Option<String> {
    let mut rgb = [value.r, value.g, value.b];
    let r = egui::widgets::color_picker::color_edit_button_srgb(ui, &mut rgb);
    (r.changed()).then(|| Rgb::new(rgb[0], rgb[1], rgb[2]).to_hex())
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let v = self.settings.values().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Size");
                egui::Grid::new("size").num_columns(3).spacing([8.0, 10.0]).show(ui, |ui| {
                    let (lo, hi) = settings::WIDTH_RANGE;
                    let mut w = v.width_percent;
                    ui.label("Width");
                    if ui.add(egui::Slider::new(&mut w, lo..=hi).show_value(false)).changed() {
                        self.settings.set_width_percent(w);
                    }
                    ui.label(format!("{}%", (w * 100.0).round()));
                    ui.end_row();

                    let (lo, hi) = settings::HEIGHT_RANGE;
                    let mut h = v.height_percent;
                    ui.label("Height");
                    if ui.add(egui::Slider::new(&mut h, lo..=hi).show_value(false)).changed() {
                        self.settings.set_height_percent(h);
                    }
                    ui.label(format!("{}%", (h * 100.0).round()));
                    ui.end_row();
                });

                ui.add_space(8.0);
                ui.heading("Appearance");
                egui::Grid::new("appearance").num_columns(3).spacing([8.0, 10.0]).show(ui, |ui| {
                    ui.label("Font");
                    if let Some(f) = self.font.show(ui, &v.font_family, "Monospace family, e.g. Hack") {
                        self.settings.set_font_family(&f);
                    }
                    ui.label("");
                    ui.end_row();

                    let mut size = v.font_size;
                    ui.label("Font size");
                    if ui.add(egui::Slider::new(&mut size, 6.0..=24.0).step_by(0.5).show_value(false)).changed() {
                        self.settings.set_font_size(size);
                    }
                    ui.label(format!("{size:.1}"));
                    ui.end_row();

                    let mut opacity = v.background_opacity;
                    ui.label("Opacity");
                    if ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0).show_value(false)).changed() {
                        self.settings.set_background_opacity(opacity);
                    }
                    ui.label(format!("{}%", (opacity * 100.0).round()));
                    ui.end_row();

                    // "" is "no scheme". A scheme set by path, or one that has
                    // since gone missing, is listed as written so the box can
                    // still show what is configured.
                    ui.label("Colour scheme");
                    let mut names = vec![String::new()];
                    names.extend(self.schemes.iter().cloned());
                    if !v.color_scheme.is_empty() && !names.contains(&v.color_scheme) {
                        names.push(v.color_scheme.clone());
                    }
                    let shown = if v.color_scheme.is_empty() { "None".to_owned() } else { v.color_scheme.clone() };
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("scheme").selected_text(shown).width(220.0).show_ui(ui, |ui| {
                        for name in &names {
                            let label = if name.is_empty() { "None" } else { name };
                            if ui.selectable_label(*name == v.color_scheme, label).clicked() {
                                chosen = Some(name.clone());
                            }
                        }
                    });
                    if let Some(name) = chosen {
                        self.settings.set_color_scheme(&name);
                    }
                    if source_tag(ui, self.settings.source_of(settings::KEY_SCHEME)) {
                        self.settings.clear_override(settings::KEY_SCHEME);
                    }
                    ui.end_row();

                    ui.label("Text colour");
                    if let Some(c) = colour_swatch(ui, v.foreground) {
                        self.settings.set_foreground(&c);
                    }
                    if source_tag(ui, self.settings.source_of(settings::KEY_FOREGROUND)) {
                        self.settings.clear_override(settings::KEY_FOREGROUND);
                    }
                    ui.end_row();

                    ui.label("Background");
                    if let Some(c) = colour_swatch(ui, v.background) {
                        self.settings.set_background(&c);
                    }
                    if source_tag(ui, self.settings.source_of(settings::KEY_BACKGROUND)) {
                        self.settings.clear_override(settings::KEY_BACKGROUND);
                    }
                    ui.end_row();

                    let mut radius = v.corner_radius;
                    ui.label("Corners");
                    if ui.add(egui::Slider::new(&mut radius, 0..=32).show_value(false)).changed() {
                        self.settings.set_corner_radius(radius);
                    }
                    ui.label(format!("{radius}px"));
                    ui.end_row();

                    let mut anim = v.animation_ms;
                    ui.label("Animation");
                    if ui.add(egui::Slider::new(&mut anim, 0..=600).step_by(10.0).show_value(false)).changed() {
                        self.settings.set_animation_ms(anim);
                    }
                    ui.label(format!("{anim}ms"));
                    ui.end_row();
                });

                ui.add_space(8.0);
                ui.heading("Shell");
                if let Some(s) = self.shell.show(ui, &v.shell_program, "Leave empty to use your login shell") {
                    self.settings.set_shell_program(&s);
                }

                ui.add_space(8.0);
                ui.heading("Tab shortcuts");
                egui::Grid::new("tabkeys").num_columns(2).spacing([8.0, 10.0]).show(ui, |ui| {
                    ui.label("Previous tab");
                    if let Some(s) = self.previous.show(ui, &v.previous_tab_keys, "Unbound") {
                        self.settings.set_previous_tab_keys(&s);
                    }
                    ui.end_row();
                    ui.label("Next tab");
                    if let Some(s) = self.next.show(ui, &v.next_tab_keys, "Unbound") {
                        self.settings.set_next_tab_keys(&s);
                    }
                    ui.end_row();
                    ui.label("Move tab left");
                    if let Some(s) = self.move_left.show(ui, &v.move_tab_left_keys, "Unbound") {
                        self.settings.set_move_tab_left_keys(&s);
                    }
                    ui.end_row();
                    ui.label("Move tab right");
                    if let Some(s) = self.move_right.show(ui, &v.move_tab_right_keys, "Unbound") {
                        self.settings.set_move_tab_right_keys(&s);
                    }
                    ui.end_row();
                });
                ui.label(
                    egui::RichText::new(
                        "Key names as Qt spells them, e.g. Shift+Left or Ctrl+Shift+Tab. \
                         Separate several with commas; leave empty to unbind.",
                    )
                    .small()
                    .weak(),
                );

                ui.add_space(6.0);
                let provisioned = self.settings.has_provisioned_defaults();
                let note = if provisioned {
                    format!(
                        "Overrides are saved to ~/.config/{APP_NAME}/{APP_NAME}.conf as you go. Reset discards \
                         them and returns to the preset in defaults.conf. The shell applies to new tabs."
                    )
                } else {
                    format!(
                        "Saved to ~/.config/{APP_NAME}/{APP_NAME}.conf as you go. Changes reach a running \
                         terminal immediately; the shell applies to new tabs."
                    )
                };
                ui.label(egui::RichText::new(note).small().weak());
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button(if provisioned { "Reset to preset" } else { "Reset to defaults" }).clicked() {
                        self.settings.reset_to_defaults();
                    }
                });
            });
        });

        // The debounced write lands once input pauses.
        let now = Instant::now();
        self.settings.flush_if_due(now);
        if let Some(deadline) = self.settings.flush_deadline() {
            ui.ctx()
                .request_repaint_after(deadline.saturating_duration_since(now));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.flush();
    }
}
