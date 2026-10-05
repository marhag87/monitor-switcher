//! Drawing the flyout: layout, colours, and an anti-aliased rasteriser for the
//! handful of rounded shapes it is made of.
//!
//! Plain std Rust with no Windows in it, like the tray icon's drawing, so it can
//! be tested. GDI cannot draw anti-aliased shapes or anything with per-pixel
//! alpha, and a layered window wants exactly that, so the shapes are rasterised
//! here from signed distances. Only the text comes from GDI, as a coverage mask.

/// Straight (not premultiplied) RGBA, 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    fn hex(rgb: u32, a: f32) -> Self {
        let c = |shift: u32| ((rgb >> shift) & 0xFF) as f32 / 255.0;
        Rgba(c(16), c(8), c(0), a)
    }

    fn from_bytes([r, g, b]: [u8; 3]) -> Self {
        Rgba(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
    }
}

/// Colours following Windows 11's own controls, in its light or dark mode.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub shadow: Rgba,
    pub panel: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    /// A switched-on toggle's fill.
    pub accent: Rgba,
    /// The knob on a switched-on toggle.
    pub on_accent: Rgba,
    /// A switched-off toggle's outline and knob.
    pub off: Rgba,
}

impl Theme {
    /// `accent` is the system accent colour already picked for this mode — see
    /// [`accent_from_palette`] — or `None` for Windows' default blue.
    pub fn new(light: bool, accent: Option<[u8; 3]>) -> Self {
        if light {
            Theme {
                shadow: Rgba::hex(0x000000, 0.16),
                panel: Rgba::hex(0xF3F3F3, 1.0),
                border: Rgba::hex(0x000000, 0.10),
                text: Rgba::hex(0x1A1A1A, 1.0),
                accent: accent.map_or(Rgba::hex(0x0067C0, 1.0), Rgba::from_bytes),
                on_accent: Rgba::hex(0xFFFFFF, 1.0),
                off: Rgba::hex(0x000000, 0.55),
            }
        } else {
            Theme {
                shadow: Rgba::hex(0x000000, 0.35),
                panel: Rgba::hex(0x2C2C2C, 1.0),
                border: Rgba::hex(0xFFFFFF, 0.08),
                text: Rgba::hex(0xFFFFFF, 1.0),
                accent: accent.map_or(Rgba::hex(0x4CC2FF, 1.0), Rgba::from_bytes),
                on_accent: Rgba::hex(0x000000, 1.0),
                off: Rgba::hex(0xFFFFFF, 0.60),
            }
        }
    }
}

/// Pick the accent shade Windows itself uses for a toggle in this mode, from
/// the `AccentPalette` registry value: eight RGBA entries running from lightest
/// to darkest. Dark mode uses the second-lightest, light mode the first shade
/// darker than the base colour.
pub fn accent_from_palette(palette: &[u8], light: bool) -> Option<[u8; 3]> {
    let entry = if light { 4 } else { 1 };
    let at = entry * 4;
    let rgb = palette.get(at..at + 3)?;
    Some([rgb[0], rgb[1], rgb[2]])
}

/// Sizes at 100% scaling, in pixels. Scaled by the monitor's DPI at draw time.
mod metric {
    /// Room around the panel for its shadow.
    pub const MARGIN: f32 = 16.0;
    pub const HEIGHT: f32 = 48.0;
    pub const RADIUS: f32 = 8.0;
    pub const PAD: f32 = 20.0;
    /// Between the label and the On/Off word.
    pub const GAP: f32 = 32.0;
    /// Between the On/Off word and the toggle.
    pub const STATE_GAP: f32 = 12.0;
    pub const TOGGLE_W: f32 = 40.0;
    pub const TOGGLE_H: f32 = 20.0;
    pub const KNOB: f32 = 6.0;
    pub const SHADOW_BLUR: f32 = 12.0;
    pub const SHADOW_DROP: f32 = 4.0;
    /// The least the panel floats above the bottom of the work area, so that
    /// even at 0% it is clear of the edge.
    pub const LIFT: f32 = 24.0;
    /// Text height, as GDI's (negative) character height.
    pub const FONT: f32 = 14.0;
}

pub fn font_px(scale: f32) -> i32 {
    (metric::FONT * scale).round() as i32
}

pub fn lift_px(scale: f32) -> i32 {
    (metric::LIFT * scale).round() as i32
}

/// Where the panel's bottom edge sits when nothing is configured, as a
/// percentage of the screen's height up from its bottom. Well clear of the
/// bottom of the screen, where a game keeps its action bars.
pub const DEFAULT_FROM_BOTTOM_PERCENT: f32 = 10.0;

/// The screen y of the panel's top edge: `percent` of the work area's height
/// up from its bottom, plus the minimum lift — kept entirely on screen however
/// large the percentage.
pub fn panel_top(work_top: i32, work_bottom: i32, lift: i32, percent: f32, panel_h: i32) -> i32 {
    let span = (work_bottom - work_top) as f32;
    let up = (span * percent.clamp(0.0, 100.0) / 100.0).round() as i32;
    let top = work_bottom - lift - up - panel_h;
    top.clamp(work_top, (work_bottom - panel_h).max(work_top))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Where everything goes, in bitmap pixels.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub width: i32,
    pub height: i32,
    pub panel: Rect,
    pub toggle: Rect,
    /// Top-left of the label text.
    pub label: (i32, i32),
    /// Right edge and top of the On/Off word, which is right-aligned against
    /// the toggle the way Settings does it.
    pub state_right: i32,
    pub state_top: i32,
}

/// Lay out the panel around already-measured text, all sizes `(w, h)` in
/// pixels. `state_w` is the width to reserve for the On/Off word — the wider of
/// the two, so the panel is the same size whichever way it went.
pub fn layout(scale: f32, label: (i32, i32), state_w: i32, text_h: i32) -> Layout {
    use metric::*;
    let s = |v: f32| (v * scale).round();

    let margin = s(MARGIN);
    let height = s(HEIGHT);
    let panel_w =
        s(PAD) + label.0 as f32 + s(GAP) + state_w as f32 + s(STATE_GAP) + s(TOGGLE_W) + s(PAD);

    let panel = Rect {
        x0: margin,
        y0: margin,
        x1: margin + panel_w,
        y1: margin + height,
    };
    let mid = margin + height / 2.0;
    let toggle = Rect {
        x0: panel.x1 - s(PAD) - s(TOGGLE_W),
        y0: mid - s(TOGGLE_H) / 2.0,
        x1: panel.x1 - s(PAD),
        y1: mid + s(TOGGLE_H) / 2.0,
    };
    let text_top = (mid - text_h as f32 / 2.0).round() as i32;

    Layout {
        width: (panel.x1 + margin) as i32,
        height: (panel.y1 + margin) as i32,
        panel,
        toggle,
        label: ((panel.x0 + s(PAD)) as i32, text_top),
        state_right: (toggle.x0 - s(STATE_GAP)) as i32,
        state_top: text_top,
    }
}

/// How opaque the flyout is, `elapsed` after it was shown — or `None` once it
/// has faded out and should be hidden.
pub fn opacity(elapsed_ms: u64) -> Option<f32> {
    let t = elapsed_ms as f32;
    let (fade_in, hold, fade_out) = (FADE_IN_MS as f32, HOLD_MS as f32, FADE_OUT_MS as f32);
    if t < fade_in {
        Some(t / fade_in)
    } else if t < fade_in + hold {
        Some(1.0)
    } else if t < fade_in + hold + fade_out {
        Some(1.0 - (t - fade_in - hold) / fade_out)
    } else {
        None
    }
}

pub const FADE_IN_MS: u64 = 80;
pub const HOLD_MS: u64 = 1100;
pub const FADE_OUT_MS: u64 = 250;

/// Draw everything but the text, which the caller blends in afterwards with
/// [`Canvas::blend_mask`].
pub fn draw(layout: &Layout, theme: &Theme, scale: f32, on: bool) -> Canvas {
    use metric::*;
    let mut c = Canvas::new(layout.width as usize, layout.height as usize);
    let radius = RADIUS * scale;

    let p = layout.panel;
    let drop = SHADOW_DROP * scale;
    c.shadow(
        Rect {
            y0: p.y0 + drop,
            y1: p.y1 + drop,
            ..p
        },
        radius,
        SHADOW_BLUR * scale,
        theme.shadow,
    );
    c.fill(p, radius, theme.panel);
    c.stroke(p, radius, scale.max(1.0), theme.border);

    let t = layout.toggle;
    let pill = (t.y1 - t.y0) / 2.0;
    let knob = KNOB * scale;
    let cy = (t.y0 + t.y1) / 2.0;
    if on {
        c.fill(t, pill, theme.accent);
        c.circle(t.x1 - pill, cy, knob, theme.on_accent);
    } else {
        c.stroke(t, pill, scale.max(1.0), theme.off);
        c.circle(t.x0 + pill, cy, knob, theme.off);
    }
    c
}

/// Premultiplied RGBA pixels, composited with "over".
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    px: Vec<[f32; 4]>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Canvas {
            width,
            height,
            px: vec![[0.0; 4]; width * height],
        }
    }

    /// Composite `color` at `coverage` over every pixel, where `coverage` maps a
    /// pixel centre to how much of the shape covers it.
    fn paint(&mut self, color: Rgba, coverage: impl Fn(f32, f32) -> f32) {
        for y in 0..self.height {
            for x in 0..self.width {
                let cov = coverage(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.over(x, y, color, cov);
                }
            }
        }
    }

    fn over(&mut self, x: usize, y: usize, color: Rgba, cov: f32) {
        let a = color.3 * cov;
        let d = &mut self.px[y * self.width + x];
        let keep = 1.0 - a;
        d[0] = color.0 * a + d[0] * keep;
        d[1] = color.1 * a + d[1] * keep;
        d[2] = color.2 * a + d[2] * keep;
        d[3] = a + d[3] * keep;
    }

    pub fn fill(&mut self, r: Rect, radius: f32, color: Rgba) {
        self.paint(color, |x, y| 0.5 - rounded_rect(x, y, r, radius));
    }

    /// An outline `width` thick, inside the shape's edge.
    pub fn stroke(&mut self, r: Rect, radius: f32, width: f32, color: Rgba) {
        let inner = Rect {
            x0: r.x0 + width,
            y0: r.y0 + width,
            x1: r.x1 - width,
            y1: r.y1 - width,
        };
        let inner_radius = (radius - width).max(0.0);
        self.paint(color, |x, y| {
            let outer = (0.5 - rounded_rect(x, y, r, radius)).clamp(0.0, 1.0);
            let hole = (0.5 - rounded_rect(x, y, inner, inner_radius)).clamp(0.0, 1.0);
            outer - hole
        });
    }

    pub fn circle(&mut self, cx: f32, cy: f32, radius: f32, color: Rgba) {
        self.paint(color, |x, y| {
            0.5 - (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - radius)
        });
    }

    /// A soft shadow: full strength well inside the shape, fading to nothing
    /// `blur` pixels outside it.
    pub fn shadow(&mut self, r: Rect, radius: f32, blur: f32, color: Rgba) {
        self.paint(color, |x, y| {
            let d = rounded_rect(x, y, r, radius);
            let t = ((d + blur * 0.5) / (blur * 1.5)).clamp(0.0, 1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        });
    }

    /// Blend `color` through an 8-bit coverage mask — rendered text — placed
    /// with its top-left at `(x, y)`. Parts falling outside are clipped.
    pub fn blend_mask(
        &mut self,
        x: i32,
        y: i32,
        mask: &[u8],
        mask_w: usize,
        mask_h: usize,
        color: Rgba,
    ) {
        for my in 0..mask_h {
            for mx in 0..mask_w {
                let (px, py) = (x + mx as i32, y + my as i32);
                if px < 0 || py < 0 || px as usize >= self.width || py as usize >= self.height {
                    continue;
                }
                let cov = mask[my * mask_w + mx] as f32 / 255.0;
                if cov > 0.0 {
                    self.over(px as usize, py as usize, color, cov);
                }
            }
        }
    }

    /// Premultiplied BGRA bytes, top row first — what `UpdateLayeredWindow`
    /// wants from a 32-bit top-down DIB.
    pub fn to_bgra(&self) -> Vec<u8> {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        self.px
            .iter()
            .flat_map(|p| [byte(p[2]), byte(p[1]), byte(p[0]), byte(p[3])])
            .collect()
    }

    #[cfg(test)]
    fn alpha(&self, x: usize, y: usize) -> f32 {
        self.px[y * self.width + x][3]
    }
}

/// Signed distance from a point to a rounded rectangle: negative inside.
fn rounded_rect(x: f32, y: f32, r: Rect, radius: f32) -> f32 {
    let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
    let (hw, hh) = ((r.x1 - r.x0) / 2.0, (r.y1 - r.y0) / 2.0);
    let radius = radius.min(hw).min(hh);
    let qx = (x - cx).abs() - (hw - radius);
    let qy = (y - cy).abs() - (hh - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_does_not_change_size_between_on_and_off() {
        // The caller reserves the wider word; the layout must not depend on
        // which one is actually drawn.
        let a = layout(1.0, (120, 19), 24, 19);
        let b = layout(1.0, (120, 19), 24, 19);
        assert_eq!((a.width, a.height), (b.width, b.height));
    }

    #[test]
    fn layout_scales_with_dpi() {
        let one = layout(1.0, (100, 19), 20, 19);
        let two = layout(2.0, (200, 38), 40, 38);
        assert_eq!(two.width, one.width * 2);
        assert_eq!(two.height, one.height * 2);
    }

    #[test]
    fn everything_sits_inside_the_panel() {
        let l = layout(1.5, (150, 28), 30, 28);
        let p = l.panel;
        assert!(l.toggle.x0 > p.x0 && l.toggle.x1 < p.x1);
        assert!(l.toggle.y0 > p.y0 && l.toggle.y1 < p.y1);
        assert!(l.label.0 as f32 > p.x0);
        assert!((l.state_right as f32) < l.toggle.x0);
        assert!((l.label.1 as f32) > p.y0 && ((l.label.1 + 28) as f32) < p.y1);
    }

    #[test]
    fn the_panel_sits_the_given_share_of_the_screen_up() {
        // A 1000-pixel work area, 20 of lift, a 50-pixel panel.
        assert_eq!(panel_top(0, 1000, 20, 0.0, 50), 930);
        assert_eq!(panel_top(0, 1000, 20, 25.0, 50), 680);
        // Offset work areas, as on a second monitor or above a taskbar.
        assert_eq!(panel_top(500, 1500, 20, 25.0, 50), 1180);
    }

    #[test]
    fn the_panel_never_leaves_the_screen() {
        assert_eq!(panel_top(0, 1000, 20, 100.0, 50), 0);
        assert_eq!(panel_top(0, 1000, 20, 250.0, 50), 0);
        assert_eq!(panel_top(0, 1000, 20, -10.0, 50), 930);
    }

    #[test]
    fn opacity_fades_in_holds_and_fades_out() {
        assert_eq!(opacity(0), Some(0.0));
        assert_eq!(opacity(FADE_IN_MS), Some(1.0));
        assert_eq!(opacity(FADE_IN_MS + HOLD_MS - 1), Some(1.0));
        let fading = opacity(FADE_IN_MS + HOLD_MS + FADE_OUT_MS / 2).unwrap();
        assert!(fading > 0.0 && fading < 1.0, "{fading}");
        assert_eq!(opacity(FADE_IN_MS + HOLD_MS + FADE_OUT_MS), None);
    }

    #[test]
    fn a_filled_shape_is_solid_inside_and_clear_outside() {
        let mut c = Canvas::new(20, 20);
        let r = Rect {
            x0: 4.0,
            y0: 4.0,
            x1: 16.0,
            y1: 16.0,
        };
        c.fill(r, 3.0, Rgba(1.0, 1.0, 1.0, 1.0));
        assert_eq!(c.alpha(10, 10), 1.0);
        assert_eq!(c.alpha(1, 1), 0.0);
        // The corner is rounded off: the pixel at the very corner is outside.
        assert_eq!(c.alpha(4, 4), 0.0);
    }

    #[test]
    fn edges_are_anti_aliased() {
        let mut c = Canvas::new(20, 20);
        c.circle(10.0, 10.0, 5.25, Rgba(1.0, 1.0, 1.0, 1.0));
        // Some pixel along the rim is partly covered rather than all or nothing.
        let partial = (0..20).any(|x| {
            let a = c.alpha(x, 10);
            a > 0.0 && a < 1.0
        });
        assert!(partial);
    }

    #[test]
    fn a_stroke_leaves_the_middle_empty() {
        let mut c = Canvas::new(40, 20);
        let r = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 40.0,
            y1: 20.0,
        };
        c.stroke(r, 10.0, 1.0, Rgba(1.0, 1.0, 1.0, 1.0));
        assert_eq!(c.alpha(20, 10), 0.0);
        assert!(c.alpha(20, 0) > 0.9);
    }

    #[test]
    fn output_is_premultiplied_bgra() {
        let mut c = Canvas::new(1, 1);
        c.fill(
            Rect {
                x0: -1.0,
                y0: -1.0,
                x1: 2.0,
                y1: 2.0,
            },
            0.0,
            Rgba(1.0, 0.0, 0.0, 0.5),
        );
        // Red at half alpha: premultiplied red is half, in the third byte.
        assert_eq!(c.to_bgra(), [0, 0, 128, 128]);
    }

    #[test]
    fn text_masks_are_clipped_to_the_canvas() {
        let mut c = Canvas::new(4, 4);
        c.blend_mask(-2, 2, &[255; 16], 4, 4, Rgba(1.0, 1.0, 1.0, 1.0));
        assert_eq!(c.alpha(0, 3), 1.0);
        assert_eq!(c.alpha(3, 0), 0.0);
    }

    /// Windows' default blue, as it appears in a real `AccentPalette`.
    #[test]
    fn accent_comes_from_the_right_palette_entry() {
        let palette = [
            0x99, 0xEB, 0xFF, 0, 0x4C, 0xC2, 0xFF, 0, 0x00, 0x91, 0xF8, 0, 0x00, 0x78, 0xD4, 0,
            0x00, 0x67, 0xC0, 0, 0x00, 0x3E, 0x92, 0, 0x00, 0x1A, 0x68, 0, 0xF7, 0x63, 0x0C, 0,
        ];
        assert_eq!(
            accent_from_palette(&palette, false),
            Some([0x4C, 0xC2, 0xFF])
        );
        assert_eq!(
            accent_from_palette(&palette, true),
            Some([0x00, 0x67, 0xC0])
        );
        assert_eq!(accent_from_palette(&palette[..8], true), None);
    }
}
