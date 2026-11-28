//! The look of the "Launcher" design system, read from its tokens
//! (`scripts/launcher/tokens.json`, the PowerShell launcher's too): the
//! sunset over the water behind the window, the dark panels the words live
//! on, the one gradient button, and the controls (segments, switches,
//! outline buttons, rows). Every colour, size and gradient comes from there.
use std::sync::OnceLock;

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, pos2, vec2,
};
use serde_json::Value;

const TOKENS_JSON: &str = include_str!("../../../scripts/launcher/tokens.json");

fn tokens() -> &'static Value {
    static TOKENS: OnceLock<Value> = OnceLock::new();
    TOKENS.get_or_init(|| serde_json::from_str(TOKENS_JSON).expect("scripts/launcher/tokens.json"))
}

fn hex(text: &str) -> Color32 {
    let t = text.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(t.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    Color32::from_rgb(p(0), p(2), p(4))
}

fn colour_value(v: &Value) -> Color32 {
    match v {
        Value::String(s) => hex(s),
        Value::Object(o) => {
            let base = colour_value(o.get("colour").unwrap_or(&Value::Null));
            let a = o.get("opacity").and_then(Value::as_f64).unwrap_or(1.0);
            Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), (a * 255.0).round() as u8)
        }
        _ => Color32::MAGENTA,
    }
}

/// A colour token by its name in the tokens file (`night`, `panelStrong`...).
pub fn colour(name: &str) -> Color32 {
    colour_value(&tokens()["colour"][name])
}

/// A number under a section of the tokens (`size.playWidth`).
pub fn num(section: &str, name: &str) -> f32 {
    tokens()[section][name].as_f64().unwrap_or(0.0) as f32
}

pub fn radius(name: &str) -> f32 {
    num("radius", name)
}

pub fn type_size(name: &str) -> f32 {
    tokens()["type"]["size"][name].as_f64().unwrap_or(14.0) as f32
}

fn stops(name: &str) -> Vec<(f32, Color32)> {
    tokens()["gradient"][name]["stops"]
        .as_array()
        .map(|a| a.iter().map(|s| (s["offset"].as_f64().unwrap_or(0.0) as f32, colour_value(&s["colour"]))).collect())
        .unwrap_or_default()
}

/// The colour of a gradient at `t` (0..1), held flat past its first and last
/// stops.
fn at(stops: &[(f32, Color32)], t: f32) -> Color32 {
    let Some(first) = stops.first() else { return Color32::TRANSPARENT };
    if t <= first.0 {
        return first.1;
    }
    for w in stops.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if t <= b {
            let f = if b > a { (t - a) / (b - a) } else { 1.0 };
            return lerp(ca, cb, f);
        }
    }
    stops.last().map_or(Color32::TRANSPARENT, |s| s.1)
}

fn lerp(a: Color32, b: Color32, f: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
    Color32::from_rgba_premultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// A gradient over a rectangle, vertical or horizontal, one band per stop.
fn gradient_rect(painter: &egui::Painter, rect: Rect, stops: &[(f32, Color32)], vertical: bool) {
    let mut cuts: Vec<f32> = vec![0.0];
    cuts.extend(stops.iter().map(|s| s.0).filter(|o| *o > 0.0 && *o < 1.0));
    cuts.push(1.0);
    let mut mesh = egui::Mesh::default();
    for t in &cuts {
        let c = at(stops, *t);
        let (p, q) = if vertical {
            let y = rect.top() + rect.height() * t;
            (pos2(rect.left(), y), pos2(rect.right(), y))
        } else {
            let x = rect.left() + rect.width() * t;
            (pos2(x, rect.top()), pos2(x, rect.bottom()))
        };
        mesh.colored_vertex(p, c);
        mesh.colored_vertex(q, c);
    }
    for i in 0..cuts.len() as u32 - 1 {
        let b = 2 * i;
        mesh.add_triangle(b, b + 1, b + 2);
        mesh.add_triangle(b + 1, b + 2, b + 3);
    }
    painter.add(Shape::mesh(mesh));
}

/// The outline of a rounded rectangle.
fn rounded_points(rect: Rect, r: f32) -> Vec<Pos2> {
    let r = r.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corners = [
        (pos2(rect.right() - r, rect.top() + r), -90.0f32),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (pos2(rect.left() + r, rect.bottom() - r), 90.0),
        (pos2(rect.left() + r, rect.top() + r), 180.0),
    ];
    let mut out = Vec::new();
    for (c, start) in corners {
        for i in 0..=8 {
            let a = (start + 90.0 * i as f32 / 8.0).to_radians();
            out.push(c + vec2(a.cos(), a.sin()) * r);
        }
    }
    out
}

/// A convex outline filled with a gradient along x (or y), as a fan.
fn gradient_polygon(painter: &egui::Painter, points: &[Pos2], bounds: Rect, stops: &[(f32, Color32)], vertical: bool) {
    let t = |p: Pos2| {
        if vertical {
            (p.y - bounds.top()) / bounds.height().max(1.0)
        } else {
            (p.x - bounds.left()) / bounds.width().max(1.0)
        }
    };
    let mut mesh = egui::Mesh::default();
    let centre = bounds.center();
    mesh.colored_vertex(centre, at(stops, t(centre)));
    for p in points {
        mesh.colored_vertex(*p, at(stops, t(*p)));
    }
    let n = points.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(Shape::mesh(mesh));
}

/// The scene behind every view: the sky, the sun with its slow breathing
/// glow, the sea band with the sun's reflections, and the two dark fades
/// that keep the words readable. `fade_in` (0..1) brings it up from night.
pub fn paint_scene(painter: &egui::Painter, rect: Rect, time: f64, fade_in: f32) {
    painter.rect_filled(rect, 0.0, colour("night"));
    gradient_rect(painter, rect, &stops("sky"), true);
    let scene = &tokens()["scene"];
    let sun = &scene["sun"];
    let size = sun["size"].as_f64().unwrap_or(240.0) as f32;
    let sun_rect = Rect::from_min_size(
        pos2(
            rect.left() + sun["left"].as_f64().unwrap_or(150.0) as f32,
            rect.bottom() - sun["bottom"].as_f64().unwrap_or(96.0) as f32 - size,
        ),
        Vec2::splat(size),
    );
    // The glow breathes between glowMin and glowMax over glowBreathMs.
    let period = num("motion", "glowBreathMs").max(1.0) as f64 / 1000.0;
    let phase = ((time / period) * std::f64::consts::TAU).sin() * 0.5 + 0.5;
    let breath = num("motion", "glowMin") + (num("motion", "glowMax") - num("motion", "glowMin")) * phase as f32;
    let glow = num("layout", "sunGlowOpacity") * breath;
    let glow_r = size / 2.0 * num("layout", "sunGlowScale");
    for i in 0..6 {
        let f = i as f32 / 6.0;
        let c = colour("sunMid").gamma_multiply(glow * 0.25 * (1.0 - f));
        painter.circle_filled(sun_rect.center(), glow_r * (1.0 - f * 0.4), c);
    }
    let circle: Vec<Pos2> = (0..64)
        .map(|i| {
            let a = i as f32 / 64.0 * std::f32::consts::TAU;
            sun_rect.center() + vec2(a.cos(), a.sin()) * size / 2.0
        })
        .collect();
    gradient_polygon(painter, &circle, sun_rect, &stops("sun"), true);
    let sea_h = tokens()["gradient"]["sea"]["height"].as_f64().unwrap_or(150.0) as f32;
    let sea = Rect::from_min_max(pos2(rect.left(), rect.bottom() - sea_h), rect.max);
    gradient_rect(painter, sea, &stops("sea"), true);
    if let Some(refl) = scene["reflections"].as_array() {
        for r in refl {
            let g = |k: &str| r[k].as_f64().unwrap_or(0.0) as f32;
            let bar = Rect::from_min_size(
                pos2(rect.left() + g("left"), rect.bottom() - g("bottom") - g("height")),
                vec2(g("width"), g("height")),
            );
            painter.rect_filled(bar, g("height") / 2.0, colour("reflection").gamma_multiply(g("opacity")));
        }
    }
    gradient_rect(painter, rect, &stops("fadeTop"), true);
    gradient_rect(painter, rect, &stops("fadeSide"), false);
    if fade_in < 1.0 {
        painter.rect_filled(rect, 0.0, colour("night").gamma_multiply(1.0 - fade_in));
    }
}

/// The fonts of the tokens' type stacks, from the system: no font file is in
/// the repository. On Windows Impact stands in for the display face (the
/// condensed Bahnschrift is a variation egui cannot pick) and Segoe UI is the
/// text; elsewhere the first of a few common files, else egui's own.
pub fn install_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let mut dirs: Vec<std::path::PathBuf> =
        std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts")).into_iter().collect();
    dirs.extend(
        ["/usr/share/fonts/truetype", "/usr/share/fonts/TTF", "/usr/share/fonts", "/usr/local/share/fonts"]
            .map(std::path::PathBuf::from),
    );
    let find = |names: &[&str]| -> Option<Vec<u8>> {
        for name in names {
            for d in &dirs {
                for p in [d.join(name), d.join("dejavu").join(name), d.join("inter").join(name)] {
                    if let Ok(bytes) = std::fs::read(&p) {
                        return Some(bytes);
                    }
                }
            }
        }
        None
    };
    let roles: [(&str, FontFamily, &[&str]); 3] = [
        ("text", FontFamily::Proportional, &["segoeui.ttf", "Inter-Regular.ttf", "DejaVuSans.ttf"]),
        ("semibold", FontFamily::Name("semibold".into()), &["seguisb.ttf", "Inter-SemiBold.ttf", "DejaVuSans-Bold.ttf"]),
        ("display", FontFamily::Name("display".into()), &["impact.ttf", "Anton-Regular.ttf", "DejaVuSans-Bold.ttf"]),
    ];
    for (key, family, names) in roles {
        if let Some(bytes) = find(names) {
            defs.font_data.insert(key.to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            defs.families.entry(family).or_default().insert(0, key.to_owned());
        }
    }
    // A family with no file found falls back to the text face.
    let proportional = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for name in ["semibold", "display"] {
        defs.families.entry(FontFamily::Name(name.into())).or_default().extend(proportional.iter().cloned());
    }
    ctx.set_fonts(defs);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = colour("panelStrong");
    v.extreme_bg_color = colour("field");
    v.faint_bg_color = colour("rowLine");
    v.override_text_color = Some(colour("text"));
    v.hyperlink_color = colour("textSoft");
    v.selection.bg_fill = colour("accent").gamma_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0, colour("text"));
    v.disabled_alpha = num("layout", "disabledOpacity");
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.weak_bg_fill = colour("field");
        w.bg_fill = colour("field");
        w.bg_stroke = Stroke::new(1.0, colour("strokeStrong"));
        w.fg_stroke = Stroke::new(1.0, colour("text"));
        w.corner_radius = CornerRadius::same(radius("control") as u8);
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, colour("textMuted"));
    v.widgets.active.bg_stroke = Stroke::new(2.0, colour("focus"));
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colour("rowLine"));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, colour("textSoft"));
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(14.0, 8.0);
        s.spacing.interact_size.y = num("layout", "selectHeight");
        s.spacing.combo_width = num("layout", "selectWidth");
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(type_size("body")));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(type_size("small")));
        s.text_styles.insert(egui::TextStyle::Small, FontId::proportional(type_size("caption")));
        s.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(type_size("small")));
    });
}

/// A screen title: the display face in capitals, spaced as the tokens say.
pub fn title(text: &str, size: f32) -> RichText {
    let spacing = tokens()["type"]["display"]["letterSpacingEm"].as_f64().unwrap_or(0.04) as f32;
    RichText::new(text.to_uppercase())
        .font(FontId::new(size, FontFamily::Name("display".into())))
        .extra_letter_spacing(size * spacing)
        .color(colour("text"))
}

/// A group's caption: small semibold capitals, widely spaced, muted.
pub fn caption(text: &str) -> RichText {
    let size = type_size("caption");
    let spacing = tokens()["type"]["caption"]["letterSpacingEm"].as_f64().unwrap_or(0.08) as f32;
    RichText::new(text.to_uppercase())
        .font(FontId::new(size, FontFamily::Name("semibold".into())))
        .extra_letter_spacing(size * spacing)
        .color(colour("textMuted"))
}

/// An option's label: semibold body.
pub fn label(text: &str) -> RichText {
    RichText::new(text).font(FontId::new(type_size("body"), FontFamily::Name("semibold".into()))).color(colour("text"))
}

pub fn small(text: &str, colour_name: &str) -> RichText {
    RichText::new(text).size(type_size("small")).color(colour(colour_name))
}

/// A dark panel over the sky (the options panel).
pub fn panel() -> egui::Frame {
    egui::Frame::new()
        .fill(colour("panel"))
        .stroke(Stroke::new(1.0, colour("stroke")))
        .corner_radius(radius("panel"))
        .inner_margin(num("layout", "panelPadding"))
}

/// A large card over the lighter sky (Preparing, More options).
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(colour("panelStrong"))
        .stroke(Stroke::new(1.0, colour("stroke")))
        .corner_radius(radius("card"))
}

fn focus_ring(ui: &Ui, rect: Rect, r: f32, response: &Response) {
    if response.has_focus() {
        let off = num("size", "focusOffset");
        ui.painter().rect_stroke(
            rect.expand(off),
            r + off,
            Stroke::new(num("size", "focusThickness"), colour("focus")),
            StrokeKind::Outside,
        );
    }
}

fn faded(ui: &Ui, c: Color32) -> Color32 {
    if ui.is_enabled() { c } else { c.gamma_multiply(num("layout", "disabledOpacity")) }
}

/// The one gradient button (Play, and the button that ends a view), with
/// the accent glow under it.
pub fn gradient_button(ui: &mut Ui, text: &str, size: Vec2, font: FontId, icon: bool, glow: bool, enabled: bool) -> Response {
    let enabled = enabled && ui.is_enabled();
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    let r = if size.y > 60.0 { radius("play") } else { radius("control") };
    let painter = ui.painter();
    if glow && enabled {
        let g = &tokens()["glow"]["play"];
        let c = colour_value(&g["colour"]).gamma_multiply(g["opacity"].as_f64().unwrap_or(0.45) as f32);
        let blur = g["blur"].as_f64().unwrap_or(40.0) as f32;
        let dy = g["offsetY"].as_f64().unwrap_or(10.0) as f32;
        for i in 1..=8 {
            let f = i as f32 / 8.0;
            let spread = blur * f * 0.6;
            painter.rect_filled(rect.translate(vec2(0.0, dy)).expand(spread), r + spread, c.gamma_multiply(0.12 * (1.0 - f)));
        }
    }
    // Off, it stays solid (the scene must not show through) but mixed toward
    // the switch's dim violet, with muted ink.
    let mut stops = stops("play");
    let off = !enabled;
    if off {
        let amount = 1.0 - num("layout", "disabledOpacity");
        for s in &mut stops {
            s.1 = lerp(s.1, colour("switchOff"), amount);
        }
    }
    let lift = if response.hovered() && !off { -1.0 } else { 0.0 };
    let rect = rect.translate(vec2(0.0, lift));
    gradient_polygon(painter, &rounded_points(rect, r), rect, &stops, false);
    let ink = if off { colour("textMuted") } else { colour("onAccent") };
    let icon_w = if icon { num("layout", "iconPlay") } else { 0.0 };
    let gap = if icon { 16.0 } else { 0.0 };
    // A longer text than the button was sized for gets a smaller font, never
    // runs past its sides.
    let mut font = font;
    let mut galley = painter.layout_no_wrap(text.to_owned(), font.clone(), ink);
    let room = rect.width() - 2.0 * 28.0 - icon_w - gap;
    if galley.size().x > room && room > 0.0 {
        font.size *= room / galley.size().x;
        galley = painter.layout_no_wrap(text.to_owned(), font, ink);
    }
    let total = icon_w + gap + galley.size().x;
    let x0 = rect.center().x - total / 2.0;
    if icon {
        let s = icon_w;
        let c = pos2(x0 + s / 2.0, rect.center().y);
        let tri = vec![c + vec2(-s * 0.3, -s * 0.4), c + vec2(s * 0.42, 0.0), c + vec2(-s * 0.3, s * 0.4)];
        painter.add(Shape::convex_polygon(tri, ink, Stroke::NONE));
    }
    painter.galley(pos2(x0 + icon_w + gap, rect.center().y - galley.size().y / 2.0), galley, ink);
    focus_ring(ui, rect, r, &response);
    response
}

/// A button with an outline and no fill (Cancel, Reset, More options); an
/// optional chevron on its right.
pub fn outline_button(ui: &mut Ui, text: &str, width: Option<f32>, chevron: bool) -> Response {
    let h = num("size", "controlHeight");
    let font = FontId::new(type_size("small"), FontFamily::Name("semibold".into()));
    let ink = faded(ui, colour("text"));
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, ink);
    let w = width.unwrap_or(galley.size().x + 36.0 + if chevron { 24.0 } else { 0.0 });
    let (rect, response) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), text));
    let r = radius("control");
    let hovered = response.hovered() && ui.is_enabled();
    if hovered {
        ui.painter().rect_filled(rect, r, colour("rowLine"));
    }
    let stroke = if hovered { colour("textMuted") } else { colour("strokeStrong") };
    ui.painter().rect_stroke(rect, r, Stroke::new(1.0, faded(ui, stroke)), StrokeKind::Inside);
    let text_x = if chevron { rect.left() + 14.0 } else { rect.center().x - galley.size().x / 2.0 };
    ui.painter().galley(pos2(text_x, rect.center().y - galley.size().y / 2.0), galley, ink);
    if chevron {
        let c = pos2(rect.right() - 20.0, rect.center().y);
        let s = num("layout", "iconChevron") / 2.0;
        ui.painter().line(
            vec![c + vec2(-s * 0.4, -s * 0.7), c + vec2(s * 0.3, 0.0), c + vec2(-s * 0.4, s * 0.7)],
            Stroke::new(2.2, ink),
        );
    }
    focus_ring(ui, rect, r, &response);
    response
}

/// A square outline button with a drawn cross (close, remove): the text
/// faces have no cross glyph.
pub fn close_button(ui: &mut Ui, name: &str) -> Response {
    let h = num("size", "controlHeight");
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(h), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), name));
    let r = radius("control");
    let hovered = response.hovered() && ui.is_enabled();
    if hovered {
        ui.painter().rect_filled(rect, r, colour("rowLine"));
    }
    let stroke = if hovered { colour("textMuted") } else { colour("strokeStrong") };
    ui.painter().rect_stroke(rect, r, Stroke::new(1.0, faded(ui, stroke)), StrokeKind::Inside);
    let c = rect.center();
    let d = h * 0.15;
    let ink = Stroke::new(2.2, faded(ui, colour("text")));
    ui.painter().line_segment([c + vec2(-d, -d), c + vec2(d, d)], ink);
    ui.painter().line_segment([c + vec2(-d, d), c + vec2(d, -d)], ink);
    focus_ring(ui, rect, r, &response);
    response.on_hover_text(name)
}

/// One segment of a segmented choice: white when chosen, outlined if not.
pub fn segment(ui: &mut Ui, text: &str, chosen: bool, width: f32) -> Response {
    let h = num("size", "controlHeight");
    let (rect, response) = ui.allocate_exact_size(vec2(width, h), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, ui.is_enabled(), chosen, text));
    let r = radius("control");
    if chosen {
        ui.painter().rect_filled(rect, r, faded(ui, colour("text")));
    } else {
        if response.hovered() && ui.is_enabled() {
            ui.painter().rect_filled(rect, r, colour("rowLine"));
        }
        ui.painter().rect_stroke(rect, r, Stroke::new(1.0, faded(ui, colour("strokeStrong"))), StrokeKind::Inside);
    }
    let ink = faded(ui, if chosen { colour("onAccent") } else { colour("text") });
    let font = FontId::new(type_size("body"), FontFamily::Name("semibold".into()));
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, ink);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, ink);
    focus_ring(ui, rect, r, &response);
    response
}

/// The switch's track and knob in `rect`.
fn paint_switch(ui: &Ui, rect: Rect, on: bool) {
    let track = if on { colour("accent") } else { colour("switchOff") };
    ui.painter().rect_filled(rect, rect.height() / 2.0, faded(ui, track));
    let knob = num("size", "knob");
    let x = if on { rect.right() - 3.0 - knob / 2.0 } else { rect.left() + 3.0 + knob / 2.0 };
    let knob_colour = if on { colour("text") } else { colour("switchKnobOff") };
    ui.painter().circle_filled(pos2(x, rect.center().y), knob / 2.0, faded(ui, knob_colour));
}

/// A whole clickable row: the label (and a hint under it) on the left, the
/// switch on the right; a click anywhere on it flips it.
pub fn switch_row(ui: &mut Ui, label_text: &str, hint: &str, on: &mut bool, height: f32) -> Response {
    let width = ui.available_width();
    let sw = vec2(num("size", "switchWidth"), num("size", "switchHeight"));
    // The text is laid out first (wrapping short of the switch), so the row
    // grows with it instead of running into the next one.
    let wrap = (width - sw.x - 16.0).max(60.0);
    let label_font = FontId::new(type_size("body"), FontFamily::Name("semibold".into()));
    let label_g = ui.painter().layout(label_text.to_owned(), label_font, faded(ui, colour("text")), wrap);
    let hint_g = (!hint.is_empty()).then(|| {
        ui.painter().layout(hint.to_owned(), FontId::proportional(type_size("caption")), faded(ui, colour("textMuted")), wrap)
    });
    let lh = label_g.size().y;
    let total = lh + hint_g.as_ref().map_or(0.0, |g| g.size().y + 3.0);
    let height = height.max(total + 16.0);
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, ui.is_enabled(), *on, label_text));
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let track = Rect::from_center_size(pos2(rect.right() - sw.x / 2.0, rect.center().y), sw);
    paint_switch(ui, track, *on);
    let y = rect.center().y - total / 2.0;
    ui.painter().galley(pos2(rect.left(), y), label_g, colour("text"));
    if let Some(g) = hint_g {
        ui.painter().galley(pos2(rect.left(), y + lh + 3.0), g, colour("textMuted"));
    }
    focus_ring(ui, track, sw.y / 2.0, &response);
    response
}

/// The line under an option row.
pub fn row_line(ui: &Ui, rect: Rect) {
    ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, colour("rowLine")));
}

/// The progress bar of the Preparing view.
pub fn progress(ui: &mut Ui, fraction: f32) {
    let h = num("layout", "progressHeight");
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    ui.painter().rect_filled(rect, h / 2.0, colour("switchOff"));
    let done = Rect::from_min_size(rect.min, vec2(rect.width() * fraction.clamp(0.0, 1.0), h));
    ui.painter().rect_filled(done, h / 2.0, colour("accent"));
}

/// The small dot before the status line.
pub fn dot(ui: &mut Ui, colour_name: &str) {
    let s = num("layout", "statusDot");
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(s), Sense::hover());
    ui.painter().circle_filled(rect.center(), s / 2.0, colour(colour_name));
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StepMark {
    Done,
    Current,
    Waiting,
    Skipped,
    Failed,
}

/// A step's mark: done (a gold check), current (an accent ring), waiting
/// (a dim ring), skipped (a dash), failed (a cross).
pub fn step_mark(ui: &mut Ui, kind: StepMark) {
    let s = num("layout", "iconStep");
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(s), Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    match kind {
        StepMark::Done => {
            p.line(
                vec![c + vec2(-s * 0.36, 0.0), c + vec2(-s * 0.08, s * 0.28), c + vec2(s * 0.38, -s * 0.3)],
                Stroke::new(3.0, colour("gold")),
            );
        }
        StepMark::Current => {
            p.circle_stroke(c, s / 2.0 - 1.5, Stroke::new(3.0, colour("accent")));
        }
        StepMark::Waiting => {
            p.circle_stroke(c, s / 2.0 - 1.0, Stroke::new(2.0, colour("stepWaiting")));
        }
        StepMark::Skipped => {
            p.hline((c.x - s * 0.3)..=(c.x + s * 0.3), c.y, Stroke::new(2.0, colour("textMuted")));
        }
        StepMark::Failed => {
            let d = s * 0.3;
            p.line_segment([c + vec2(-d, -d), c + vec2(d, d)], Stroke::new(3.0, colour("rose")));
            p.line_segment([c + vec2(-d, d), c + vec2(d, -d)], Stroke::new(3.0, colour("rose")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_the_window_reads_is_there() {
        for c in [
            "night", "accent", "onAccent", "text", "textSoft", "textMuted", "field", "switchOff", "switchKnobOff",
            "stepWaiting", "reflection", "focus", "panel", "panelStrong", "stroke", "strokeStrong", "rowLine", "gold",
            "rose", "sunMid",
        ] {
            assert!(tokens()["colour"][c] != Value::Null, "colour.{c}");
        }
        for g in ["sky", "sea", "sun", "play", "fadeTop", "fadeSide"] {
            assert!(!stops(g).is_empty(), "gradient.{g}");
        }
        for (s, n) in [("size", "playWidth"), ("size", "controlHeight"), ("layout", "rowHeight"), ("radius", "card")] {
            assert!(num(s, n) > 0.0, "{s}.{n}");
        }
    }

    #[test]
    fn gradients_hold_past_their_ends() {
        let s = stops("play");
        assert_eq!(at(&s, -1.0), s[0].1);
        assert_eq!(at(&s, 2.0), s.last().unwrap().1);
    }
}
