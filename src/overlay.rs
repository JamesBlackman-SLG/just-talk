use anyhow::{Context, Result};
use cosmic_text::{
    Attrs, Buffer as TextBuffer, Color as CColor, FontSystem, Metrics, Shaping, SwashCache,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::sync::mpsc;
use std::time::Instant;
use tracing::{info, warn};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

// ---- Constants ----

const FLY_DURATION_SECS: f32 = 0.35;
const DISPLAY_FONT_SIZE: f32 = 64.0;
const END_FONT_SIZE: f32 = 14.0;
const DISPLAY_LINE_HEIGHT: f32 = 72.0;
const END_LINE_HEIGHT: f32 = 18.0;
const RECORDING_DOT_RADIUS: f32 = 8.0;
const RECORDING_DOT_MARGIN: f32 = 24.0;

// Panel styling
const PANEL_PADDING: f32 = 24.0;
const PANEL_CORNER_RADIUS: f32 = 16.0;
const PANEL_BG_R: u8 = 0x1A;
const PANEL_BG_G: u8 = 0x1A;
const PANEL_BG_B: u8 = 0x2E;
const PANEL_BG_ALPHA: u8 = 0xE0;
const BORDER_R: u8 = 0x58;
const BORDER_G: u8 = 0x58;
const BORDER_B: u8 = 0x80;
const BORDER_ALPHA: u8 = 0xCC;
const BORDER_WIDTH: f32 = 2.0;

// Speech bubble tail
const TAIL_HALF_BASE: f32 = 20.0;
const TAIL_MIN_LENGTH: f32 = 40.0;

// Fly-out animation
const TRAIL_COUNT: usize = 8;
const TRAIL_SPACING: f32 = 0.04;
const SPIRAL_FREQ: f32 = 2.5;
const SPIRAL_AMP: f32 = 25.0;
const BEZIER_ARC: f32 = 0.25;

// Cursor polling
const CURSOR_POLL_MS: u128 = 50;

// Per-character grow animation
const CHAR_GROW_DURATION: f32 = 0.25;
const CHAR_STAGGER: f32 = 0.025;

// ---- Public API ----

/// Commands sent to the overlay thread.
pub enum OverlayCommand {
    UpdateText(String),
    Finish(String, f32, f32),
    Close,
}

/// Handle to a running overlay thread.
pub struct OverlayHandle {
    pub tx: mpsc::Sender<OverlayCommand>,
    join: std::thread::JoinHandle<()>,
}

impl OverlayHandle {
    pub fn send(&self, cmd: OverlayCommand) {
        let _ = self.tx.send(cmd);
    }
    pub fn join(self) {
        let _ = self.join.join();
    }
}

pub fn spawn_overlay() -> Result<OverlayHandle> {
    let (tx, rx) = mpsc::channel();
    let join = std::thread::spawn(move || {
        if let Err(e) = run_overlay_thread(rx) {
            warn!(error = %e, "overlay thread failed");
        }
    });
    Ok(OverlayHandle { tx, join })
}

// ---- Private types ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Recording,
    FlyOut,
}

struct OverlayState {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    font_system: FontSystem,
    swash_cache: SwashCache,
    rx: mpsc::Receiver<OverlayCommand>,
    text: String,
    cursor_x: f32,
    cursor_y: f32,
    width: u32,
    height: u32,
    first_configure: bool,
    phase: Phase,
    fly_start: Instant,
    recording_start: Instant,
    last_cursor_poll: Instant,
    /// Per-character animation birth times (indexed by char index).
    char_birth_times: Vec<Instant>,
    done: bool,
}

// ---- Overlay thread ----

fn run_overlay_thread(rx: mpsc::Receiver<OverlayCommand>) -> Result<()> {
    info!("overlay thread starting");

    let conn = Connection::connect_to_env().context("failed to connect to Wayland")?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).context("wl_compositor not available")?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).context("wlr-layer-shell not available")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("justspeak"), None);

    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);

    // Empty input region — overlay is visual only, pointer events pass through
    // so Hyprland can still change window focus while the overlay is visible.
    let empty_region = Region::new(&compositor).context("failed to create region")?;
    layer.wl_surface().set_input_region(Some(empty_region.wl_region()));

    layer.commit();

    let font_system = FontSystem::new();
    let swash_cache = SwashCache::new();
    let pool = SlotPool::new(256 * 256 * 4, &shm)?;

    let now = Instant::now();
    let (cx, cy) = read_cursor_position();

    let mut state = OverlayState {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        font_system,
        swash_cache,
        rx,
        text: String::new(),
        cursor_x: cx,
        cursor_y: cy,
        width: 0,
        height: 0,
        first_configure: true,
        phase: Phase::Recording,
        fly_start: now,
        recording_start: now,
        last_cursor_poll: now,
        char_birth_times: Vec::new(),
        done: false,
    };

    while !state.done {
        event_queue.blocking_dispatch(&mut state)?;
    }

    info!("overlay thread finished");
    Ok(())
}

// ---- Primitive drawing helpers ----

fn put_pixel(canvas: &mut [u8], cw: usize, ch: usize, px: usize, py: usize, pixel: u32) {
    if px < cw && py < ch {
        let idx = (py * cw + px) * 4;
        if idx + 3 < canvas.len() {
            canvas[idx..idx + 4].copy_from_slice(&pixel.to_le_bytes());
        }
    }
}

fn premul_argb(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let a32 = a as u32;
    (a32 << 24) | (r as u32 * a32 / 255) << 16 | (g as u32 * a32 / 255) << 8 | (b as u32 * a32 / 255)
}

fn draw_circle(canvas: &mut [u8], cw: usize, ch: usize, cx: f32, cy: f32, radius: f32, color: u32) {
    let r2 = radius * radius;
    let x0 = (cx - radius).max(0.0) as usize;
    let x1 = ((cx + radius) as usize + 1).min(cw);
    let y0 = (cy - radius).max(0.0) as usize;
    let y1 = ((cy + radius) as usize + 1).min(ch);
    for py in y0..y1 {
        for px in x0..x1 {
            let dx = px as f32 - cx;
            let dy = py as f32 - cy;
            if dx * dx + dy * dy <= r2 {
                put_pixel(canvas, cw, ch, px, py, color);
            }
        }
    }
}

fn draw_filled_triangle(
    canvas: &mut [u8], cw: usize, ch: usize,
    x0: f32, y0: f32, x1: f32, y1: f32, x2: f32, y2: f32,
    color: u32,
) {
    let min_x = x0.min(x1).min(x2).max(0.0) as usize;
    let max_x = (x0.max(x1).max(x2) as usize + 1).min(cw);
    let min_y = y0.min(y1).min(y2).max(0.0) as usize;
    let max_y = (y0.max(y1).max(y2) as usize + 1).min(ch);

    for py in min_y..max_y {
        for px in min_x..max_x {
            let fpx = px as f32 + 0.5;
            let fpy = py as f32 + 0.5;
            // Barycentric sign test
            let d1 = (fpx - x1) * (y0 - y1) - (x0 - x1) * (fpy - y1);
            let d2 = (fpx - x2) * (y1 - y2) - (x1 - x2) * (fpy - y2);
            let d3 = (fpx - x0) * (y2 - y0) - (x2 - x0) * (fpy - y0);
            let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            if !(has_neg && has_pos) {
                put_pixel(canvas, cw, ch, px, py, color);
            }
        }
    }
}

fn corner_center(lx: f32, ly: f32, rw: f32, rh: f32, radius: f32) -> (Option<f32>, Option<f32>) {
    let in_left = lx < radius;
    let in_right = lx >= rw - radius;
    let in_top = ly < radius;
    let in_bottom = ly >= rh - radius;
    match (in_left || in_right, in_top || in_bottom) {
        (true, true) => {
            let cx = if in_left { radius } else { rw - radius };
            let cy = if in_top { radius } else { rh - radius };
            (Some(cx), Some(cy))
        }
        _ => (None, None),
    }
}

fn draw_rounded_rect(
    canvas: &mut [u8], cw: usize, ch: usize,
    rx: i32, ry: i32, rw: u32, rh: u32,
    radius: f32, fill: u32, border: u32, bw: f32,
) {
    let x0 = rx.max(0) as usize;
    let y0 = ry.max(0) as usize;
    let x1 = ((rx + rw as i32) as usize).min(cw);
    let y1 = ((ry + rh as i32) as usize).min(ch);
    let fw = rw as f32;
    let fh = rh as f32;
    let frx = rx as f32;
    let fry = ry as f32;

    for py in y0..y1 {
        for px in x0..x1 {
            let lx = px as f32 - frx;
            let ly = py as f32 - fry;
            let (ccx, ccy) = corner_center(lx, ly, fw, fh, radius);
            let inside = match (ccx, ccy) {
                (Some(cx), Some(cy)) => {
                    let dx = lx - cx;
                    let dy = ly - cy;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist > radius + 0.5 {
                        continue;
                    }
                    dist <= radius - bw
                }
                _ => lx >= bw && lx < fw - bw && ly >= bw && ly < fh - bw,
            };
            put_pixel(canvas, cw, ch, px, py, if inside { fill } else { border });
        }
    }
}

// ---- Easing and math ----

fn ease_in_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

fn bezier(t: f32, p0: f32, p1: f32, p2: f32) -> f32 {
    let mt = 1.0 - t;
    mt * mt * p0 + 2.0 * mt * t * p1 + t * t * p2
}

fn bezier_deriv(t: f32, p0: f32, p1: f32, p2: f32) -> f32 {
    2.0 * (1.0 - t) * (p1 - p0) + 2.0 * t * (p2 - p1)
}

// ---- Cursor position via hyprctl ----

fn read_cursor_position() -> (f32, f32) {
    if let Ok(output) = std::process::Command::new("hyprctl")
        .args(["cursorpos", "-j"])
        .output()
    {
        if let Ok(text) = String::from_utf8(output.stdout) {
            if let (Some(x), Some(y)) = (json_num(&text, "x"), json_num(&text, "y")) {
                return (x, y);
            }
        }
    }
    (960.0, 800.0)
}

fn json_num(json: &str, key: &str) -> Option<f32> {
    let pat = format!("\"{}\":", key);
    let start = json.find(&pat)? + pat.len();
    let rest = json[start..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')?;
    rest[..end].parse().ok()
}

// ---- OverlayState impl ----

impl OverlayState {
    fn poll_commands(&mut self) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                OverlayCommand::UpdateText(text) => {
                    if self.phase == Phase::Recording && text != self.text {
                        let now = Instant::now();
                        let old_chars: Vec<char> = self.text.chars().collect();
                        let new_chars: Vec<char> = text.chars().collect();

                        // Character-level common prefix
                        let common_count = old_chars.iter().zip(new_chars.iter())
                            .take_while(|(a, b)| a == b)
                            .count();

                        // Preserve birth times for matching prefix, fresh for new/changed
                        let mut new_times = Vec::with_capacity(new_chars.len());
                        for i in 0..common_count {
                            new_times.push(
                                self.char_birth_times.get(i).copied().unwrap_or(now),
                            );
                        }
                        for _ in common_count..new_chars.len() {
                            new_times.push(now);
                        }

                        self.char_birth_times = new_times;
                        self.text = text;
                    }
                }
                OverlayCommand::Finish(text, cx, cy) => {
                    self.text = text;
                    self.cursor_x = cx;
                    self.cursor_y = cy;
                    self.phase = Phase::FlyOut;
                    self.fly_start = Instant::now();
                }
                OverlayCommand::Close => {
                    self.done = true;
                }
            }
        }
    }

    fn poll_cursor(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_cursor_poll).as_millis() >= CURSOR_POLL_MS {
            self.last_cursor_poll = now;
            let (cx, cy) = read_cursor_position();
            self.cursor_x = cx;
            self.cursor_y = cy;
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        self.poll_commands();
        if self.done {
            return;
        }
        let width = self.width;
        let height = self.height;
        if width == 0 || height == 0 {
            return;
        }
        match self.phase {
            Phase::Recording => self.draw_recording(qh, width, height),
            Phase::FlyOut => self.draw_flyout(qh, width, height),
        }
    }

    fn layout_text(
        fs: &mut FontSystem, text: &str, font_size: f32, line_height: f32,
        max_w: f32, max_h: f32,
    ) -> (f32, f32, TextBuffer) {
        let metrics = Metrics::new(font_size, line_height);
        let mut buf = TextBuffer::new(fs, metrics);
        buf.set_size(fs, Some(max_w), Some(max_h));
        buf.set_text(fs, text, Attrs::new().family(cosmic_text::Family::SansSerif), Shaping::Advanced);
        buf.shape_until_scroll(fs, false);
        let mut tw = 0.0_f32;
        let mut th = 0.0_f32;
        for run in buf.layout_runs() {
            th = th.max(run.line_y + line_height);
            tw = tw.max(run.line_w);
        }
        (tw, th, buf)
    }

    fn render_text(
        fs: &mut FontSystem, sc: &mut SwashCache, buf: &mut TextBuffer,
        canvas: &mut [u8], cw: usize, ch: usize, ox: i32, oy: i32, alpha: u8,
    ) {
        let color = CColor::rgba(0xFF, 0xFF, 0xFF, alpha);
        buf.draw(fs, sc, color, |x, y, _w, _h, c| {
            let px = x + ox;
            let py = y + oy;
            if px < 0 || py < 0 { return; }
            let px = px as usize;
            let py = py as usize;
            if px >= cw || py >= ch { return; }
            let a = c.a();
            if a == 0 { return; }
            put_pixel(canvas, cw, ch, px, py, premul_argb(c.r(), c.g(), c.b(), a));
        });
    }

    /// Draw the speech bubble tail from the nearest panel edge to the cursor.
    fn draw_tail(
        canvas: &mut [u8], cw: usize, ch: usize,
        panel_x: i32, panel_y: i32, panel_w: u32, panel_h: u32,
        cursor_x: f32, cursor_y: f32, fill: u32, alpha: u8,
    ) {
        let pl = panel_x as f32;
        let pt = panel_y as f32;
        let pr = pl + panel_w as f32;
        let pb = pt + panel_h as f32;
        let cx_mid = (pl + pr) / 2.0;
        let cy_mid = (pt + pb) / 2.0;

        // Distance from cursor to each edge (positive = outside that edge)
        let dist_bottom = cursor_y - pb;
        let dist_top = pt - cursor_y;
        let dist_right = cursor_x - pr;
        let dist_left = pl - cursor_x;

        // Pick the edge the cursor is furthest beyond
        let max_dist = dist_bottom.max(dist_top).max(dist_right).max(dist_left);

        // Don't draw if cursor is inside the panel or too close
        if max_dist < TAIL_MIN_LENGTH {
            return;
        }

        let margin = PANEL_CORNER_RADIUS + TAIL_HALF_BASE;

        // (base_point_0, base_point_1) on the panel edge, tip at cursor
        let (bx0, by0, bx1, by1) = if max_dist == dist_bottom {
            // Cursor below — base on bottom edge, spread horizontally
            let h_left = pl + margin;
            let h_right = pr - margin;
            if h_left >= h_right { return; }
            let base_cx = cursor_x.clamp(h_left, h_right);
            (base_cx - TAIL_HALF_BASE, pb, base_cx + TAIL_HALF_BASE, pb)
        } else if max_dist == dist_top {
            // Cursor above — base on top edge, spread horizontally
            let h_left = pl + margin;
            let h_right = pr - margin;
            if h_left >= h_right { return; }
            let base_cx = cursor_x.clamp(h_left, h_right);
            (base_cx - TAIL_HALF_BASE, pt, base_cx + TAIL_HALF_BASE, pt)
        } else if max_dist == dist_right {
            // Cursor to the right — base on right edge, spread vertically
            let v_top = pt + margin;
            let v_bot = pb - margin;
            if v_top >= v_bot { return; }
            let base_cy = cursor_y.clamp(v_top, v_bot);
            (pr, base_cy - TAIL_HALF_BASE, pr, base_cy + TAIL_HALF_BASE)
        } else {
            // Cursor to the left — base on left edge, spread vertically
            let v_top = pt + margin;
            let v_bot = pb - margin;
            if v_top >= v_bot { return; }
            let base_cy = cursor_y.clamp(v_top, v_bot);
            (pl, base_cy - TAIL_HALF_BASE, pl, base_cy + TAIL_HALF_BASE)
        };

        // If cursor is inside the panel bounds on the base axis, skip
        // (handles the corner case where cursor is diagonally close)
        let _ = (cx_mid, cy_mid); // suppress unused warning

        draw_filled_triangle(canvas, cw, ch, bx0, by0, bx1, by1, cursor_x, cursor_y, fill);

        if alpha > 0 {
            let border_col = premul_argb(BORDER_R, BORDER_G, BORDER_B,
                (BORDER_ALPHA as u32 * alpha as u32 / 255) as u8);
            draw_line(canvas, cw, ch, bx0, by0, cursor_x, cursor_y, BORDER_WIDTH, border_col);
            draw_line(canvas, cw, ch, bx1, by1, cursor_x, cursor_y, BORDER_WIDTH, border_col);
        }
    }

    fn draw_recording(&mut self, qh: &QueueHandle<Self>, width: u32, height: u32) {
        self.poll_cursor();
        let rec_elapsed = self.rec_dot_elapsed();

        let stride = width as i32 * 4;
        let buf_size = (stride * height as i32) as usize;
        if self.pool.len() < buf_size {
            self.pool.resize(buf_size).ok();
        }

        let (buffer, canvas) = self.pool
            .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");
        canvas.fill(0);

        let cw = width as usize;
        let ch = height as usize;
        let max_tw = (width as f32 * 0.8).max(200.0);
        let fill = premul_argb(PANEL_BG_R, PANEL_BG_G, PANEL_BG_B, PANEL_BG_ALPHA);
        let border = premul_argb(BORDER_R, BORDER_G, BORDER_B, BORDER_ALPHA);

        if !self.text.is_empty() {
            // Layout at full size to get positions of all glyphs
            let (tw, th, text_buf) = Self::layout_text(
                &mut self.font_system, &self.text,
                DISPLAY_FONT_SIZE, DISPLAY_LINE_HEIGHT, max_tw, height as f32,
            );

            let pw = (tw + PANEL_PADDING * 2.0).ceil() as u32;
            let ph = (th + PANEL_PADDING * 2.0).ceil() as u32;
            let px = (width as f32 / 2.0 - pw as f32 / 2.0) as i32;
            let py = (height as f32 / 3.0 - ph as f32 / 2.0) as i32;
            let text_ox = px as f32 + PANEL_PADDING;
            let text_oy = py as f32 + PANEL_PADDING;

            // Draw tail
            Self::draw_tail(canvas, cw, ch, px, py, pw, ph,
                self.cursor_x, self.cursor_y, fill, 0xFF);

            // Draw panel
            draw_rounded_rect(canvas, cw, ch, px, py, pw, ph,
                PANEL_CORNER_RADIUS, fill, border, BORDER_WIDTH);

            // Collect glyph info with per-character birth-time animation
            let now = Instant::now();
            let birth_times = &self.char_birth_times;
            let mut glyph_infos: Vec<GlyphDrawInfo> = Vec::new();

            for run in text_buf.layout_runs() {
                for glyph in run.glyphs.iter() {
                    let char_idx = self.text[..glyph.start].chars().count();
                    let birth = birth_times.get(char_idx).copied().unwrap_or(now);
                    let elapsed = now.duration_since(birth).as_secs_f32();

                    // Stagger within batch (chars born at the same instant)
                    let batch_start = (0..char_idx)
                        .rev()
                        .find(|&i| birth_times.get(i).copied() != Some(birth))
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    let stagger_delay = (char_idx - batch_start) as f32 * CHAR_STAGGER;

                    let t = ((elapsed - stagger_delay) / CHAR_GROW_DURATION).clamp(0.0, 1.0);
                    let scale = 1.0 - (1.0 - t) * (1.0 - t); // ease-out-quad

                    glyph_infos.push(GlyphDrawInfo {
                        x: glyph.x + text_ox,
                        y: run.line_y + text_oy,
                        w: glyph.w,
                        start: glyph.start,
                        end: glyph.end,
                        scale,
                    });
                }
            }

            // Draw each glyph
            let text = self.text.clone();
            for info in &glyph_infos {
                if info.scale <= 0.001 {
                    continue; // invisible, skip
                }

                let char_text = &text[info.start..info.end];
                let font_size = DISPLAY_FONT_SIZE * info.scale;
                let line_height = DISPLAY_LINE_HEIGHT * info.scale;

                if font_size < 1.0 {
                    continue;
                }

                // Layout this single character
                let metrics = Metrics::new(font_size, line_height);
                let mut char_buf = TextBuffer::new(&mut self.font_system, metrics);
                char_buf.set_size(&mut self.font_system, Some(info.w + 20.0), Some(DISPLAY_LINE_HEIGHT + 20.0));
                char_buf.set_text(&mut self.font_system, char_text,
                    Attrs::new().family(cosmic_text::Family::SansSerif), Shaping::Advanced);
                char_buf.shape_until_scroll(&mut self.font_system, false);

                // Position: center the scaled character on where it should be at full size
                // Vertical: align baseline; the glyph should sit at the same baseline
                let y_offset = DISPLAY_LINE_HEIGHT * (1.0 - info.scale) * 0.5;
                let x_offset = info.w * (1.0 - info.scale) * 0.5;
                let ox = (info.x + x_offset) as i32;
                let oy = (info.y + y_offset) as i32;

                let alpha = (info.scale * 255.0) as u8;
                Self::render_text(
                    &mut self.font_system, &mut self.swash_cache, &mut char_buf,
                    canvas, cw, ch, ox, oy, alpha,
                );
            }

            // Recording dot
            draw_rec_dot(canvas, cw, ch,
                (px + pw as i32) as f32 - RECORDING_DOT_MARGIN,
                py as f32 + RECORDING_DOT_MARGIN, rec_elapsed);
        } else {
            // Minimal pill with just the recording dot
            let pw = (RECORDING_DOT_MARGIN * 2.0 + RECORDING_DOT_RADIUS * 2.0 + PANEL_PADDING) as u32;
            let ph = (RECORDING_DOT_MARGIN * 2.0) as u32;
            let px = (width as f32 / 2.0 - pw as f32 / 2.0) as i32;
            let py = (height as f32 / 3.0 - ph as f32 / 2.0) as i32;

            Self::draw_tail(canvas, cw, ch, px, py, pw, ph,
                self.cursor_x, self.cursor_y, fill, 0xFF);

            draw_rounded_rect(canvas, cw, ch, px, py, pw, ph,
                (ph as f32 / 2.0).min(PANEL_CORNER_RADIUS), fill, border, BORDER_WIDTH);

            draw_rec_dot(canvas, cw, ch,
                width as f32 / 2.0,
                py as f32 + ph as f32 / 2.0, rec_elapsed);
        }

        self.commit_frame(qh, buffer, width, height);
    }

    fn draw_flyout(&mut self, qh: &QueueHandle<Self>, width: u32, height: u32) {
        if self.text.is_empty() {
            self.done = true;
            return;
        }

        let elapsed = self.fly_start.elapsed().as_secs_f32();
        let t = (elapsed / FLY_DURATION_SECS).clamp(0.0, 1.0);
        if t >= 1.0 {
            self.done = true;
            return;
        }

        let eased = ease_in_cubic(t);

        // Bezier curve from panel center to cursor with an arc
        let start_x = width as f32 / 2.0;
        let start_y = height as f32 / 3.0;
        let end_x = self.cursor_x;
        let end_y = self.cursor_y;

        // Control point: perpendicular offset from midpoint for curved arc
        let dx = end_x - start_x;
        let dy = end_y - start_y;
        let ctrl_x = (start_x + end_x) / 2.0 - dy * BEZIER_ARC;
        let ctrl_y = (start_y + end_y) / 2.0 + dx * BEZIER_ARC;

        // Position along bezier
        let mut current_x = bezier(eased, start_x, ctrl_x, end_x);
        let mut current_y = bezier(eased, start_y, ctrl_y, end_y);

        // Spiral oscillation perpendicular to path
        let tang_x = bezier_deriv(eased, start_x, ctrl_x, end_x);
        let tang_y = bezier_deriv(eased, start_y, ctrl_y, end_y);
        let tang_len = (tang_x * tang_x + tang_y * tang_y).sqrt().max(0.001);
        let perp_x = -tang_y / tang_len;
        let perp_y = tang_x / tang_len;
        let spiral_decay = (1.0 - eased) * SPIRAL_AMP;
        let spiral_offset = (eased * SPIRAL_FREQ * std::f32::consts::TAU).sin() * spiral_decay;
        current_x += perp_x * spiral_offset;
        current_y += perp_y * spiral_offset;

        // Interpolate sizes
        let font_size = DISPLAY_FONT_SIZE + (END_FONT_SIZE - DISPLAY_FONT_SIZE) * eased;
        let line_height = DISPLAY_LINE_HEIGHT + (END_LINE_HEIGHT - DISPLAY_LINE_HEIGHT) * eased;
        let padding = PANEL_PADDING * (1.0 - eased * 0.7);
        let corner_r = PANEL_CORNER_RADIUS * (1.0 - eased * 0.6);

        // Alpha: start fading at 60% through
        let alpha = if t > 0.6 {
            ((1.0 - t) / 0.4 * 255.0) as u8
        } else {
            255u8
        };

        let max_tw = (width as f32 * 0.8).max(200.0);
        let (tw, th, mut text_buf) = Self::layout_text(
            &mut self.font_system, &self.text,
            font_size, line_height, max_tw, height as f32,
        );

        let pw = (tw + padding * 2.0).ceil() as u32;
        let ph = (th + padding * 2.0).ceil() as u32;
        let panel_x = (current_x - pw as f32 / 2.0) as i32;
        let panel_y = (current_y - ph as f32 / 2.0) as i32;

        let stride = width as i32 * 4;
        let buf_size = (stride * height as i32) as usize;
        if self.pool.len() < buf_size {
            self.pool.resize(buf_size).ok();
        }

        let (buffer, canvas) = self.pool
            .create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
            .expect("create buffer");
        canvas.fill(0);

        let cw = width as usize;
        let ch = height as usize;

        // Draw comet trail dots along the bezier behind the panel
        for i in 1..=TRAIL_COUNT {
            let trail_t = (eased - i as f32 * TRAIL_SPACING).max(0.0);
            if trail_t <= 0.0 { continue; }

            let mut tx = bezier(trail_t, start_x, ctrl_x, end_x);
            let mut ty = bezier(trail_t, start_y, ctrl_y, end_y);

            // Apply spiral to trail too
            let tt_x = bezier_deriv(trail_t, start_x, ctrl_x, end_x);
            let tt_y = bezier_deriv(trail_t, start_y, ctrl_y, end_y);
            let tl = (tt_x * tt_x + tt_y * tt_y).sqrt().max(0.001);
            let tp_x = -tt_y / tl;
            let tp_y = tt_x / tl;
            let td = (1.0 - trail_t) * SPIRAL_AMP;
            let to = (trail_t * SPIRAL_FREQ * std::f32::consts::TAU).sin() * td;
            tx += tp_x * to;
            ty += tp_y * to;

            let fade = 1.0 - i as f32 / (TRAIL_COUNT as f32 + 1.0);
            let ta = (alpha as f32 * fade * 0.5) as u8;
            let tr = (4.0 - i as f32 * 0.3).max(1.5);
            draw_circle(canvas, cw, ch, tx, ty, tr, premul_argb(0xAA, 0xBB, 0xFF, ta));
        }

        // Draw panel
        let bg_a = (PANEL_BG_ALPHA as u32 * alpha as u32 / 255) as u8;
        let bd_a = (BORDER_ALPHA as u32 * alpha as u32 / 255) as u8;
        let fill = premul_argb(PANEL_BG_R, PANEL_BG_G, PANEL_BG_B, bg_a);
        let bdr = premul_argb(BORDER_R, BORDER_G, BORDER_B, bd_a);
        draw_rounded_rect(canvas, cw, ch, panel_x, panel_y, pw, ph, corner_r, fill, bdr, BORDER_WIDTH);

        // Text
        Self::render_text(
            &mut self.font_system, &mut self.swash_cache, &mut text_buf,
            canvas, cw, ch,
            panel_x + padding as i32, panel_y + padding as i32, alpha,
        );

        self.commit_frame(qh, buffer, width, height);
    }

    fn rec_dot_elapsed(&self) -> f32 {
        self.recording_start.elapsed().as_secs_f32()
    }

    fn commit_frame(
        &self, qh: &QueueHandle<Self>,
        buffer: smithay_client_toolkit::shm::slot::Buffer,
        width: u32, height: u32,
    ) {
        self.layer.wl_surface().damage_buffer(0, 0, width as i32, height as i32);
        self.layer.wl_surface().frame(qh, self.layer.wl_surface().clone());
        buffer.attach_to(self.layer.wl_surface()).expect("buffer attach");
        self.layer.commit();
    }
}

/// Draw a pulsing red recording dot.
fn draw_rec_dot(canvas: &mut [u8], cw: usize, ch: usize, cx: f32, cy: f32, elapsed: f32) {
    let pulse = ((elapsed * 3.0).sin() * 0.5 + 0.5).clamp(0.0, 1.0);
    let a = (100.0 + pulse * 155.0) as u8;
    draw_circle(canvas, cw, ch, cx, cy, RECORDING_DOT_RADIUS, premul_argb(0xFF, 0x30, 0x30, a));
}

/// Info about a glyph's position and animation scale for per-character grow.
struct GlyphDrawInfo {
    x: f32,
    y: f32,
    w: f32,
    start: usize,
    end: usize,
    scale: f32,
}

/// Draw a thick line between two points.
fn draw_line(
    canvas: &mut [u8], cw: usize, ch: usize,
    x0: f32, y0: f32, x1: f32, y1: f32,
    thickness: f32, color: u32,
) {
    let min_x = x0.min(x1).max(0.0) as usize;
    let max_x = (x0.max(x1) as usize + 1).min(cw);
    let min_y = y0.min(y1).max(0.0) as usize;
    let max_y = (y0.max(y1) as usize + 1).min(ch);

    let dx = x1 - x0;
    let dy = y1 - y0;
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let half = thickness / 2.0;

    for py in min_y..max_y {
        for px in min_x..max_x {
            let fpx = px as f32 + 0.5;
            let fpy = py as f32 + 0.5;
            // Distance from point to line segment
            let t = ((fpx - x0) * dx + (fpy - y0) * dy) / (len * len);
            let t = t.clamp(0.0, 1.0);
            let proj_x = x0 + t * dx;
            let proj_y = y0 + t * dy;
            let dist = ((fpx - proj_x).powi(2) + (fpy - proj_y).powi(2)).sqrt();
            if dist <= half {
                put_pixel(canvas, cw, ch, px, py, color);
            }
        }
    }
}

// ---- Trait implementations ----

impl CompositorHandler for OverlayState {
    fn scale_factor_changed(
        &mut self, _conn: &Connection, _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface, _new_factor: i32,
    ) {}
    fn transform_changed(
        &mut self, _conn: &Connection, _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface, _new_transform: wl_output::Transform,
    ) {}
    fn frame(
        &mut self, _conn: &Connection, qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface, _time: u32,
    ) {
        self.draw(qh);
    }
    fn surface_enter(
        &mut self, _conn: &Connection, _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface, _output: &wl_output::WlOutput,
    ) {}
    fn surface_leave(
        &mut self, _conn: &Connection, _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface, _output: &wl_output::WlOutput,
    ) {}
}

impl OutputHandler for OverlayState {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for OverlayState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.done = true;
    }
    fn configure(
        &mut self, _conn: &Connection, qh: &QueueHandle<Self>, _layer: &LayerSurface,
        configure: LayerSurfaceConfigure, _serial: u32,
    ) {
        if configure.new_size.0 != 0 && configure.new_size.1 != 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }
        if self.first_configure {
            self.first_configure = false;
            self.recording_start = Instant::now();
            self.draw(qh);
        }
    }
}

impl ShmHandler for OverlayState {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

delegate_compositor!(OverlayState);
delegate_output!(OverlayState);
delegate_shm!(OverlayState);
delegate_layer!(OverlayState);
delegate_registry!(OverlayState);

impl ProvidesRegistryState for OverlayState {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState];
}
