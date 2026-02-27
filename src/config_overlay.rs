use anyhow::{Context, Result};
use cosmic_text::{
    Attrs, Buffer as TextBuffer, Color as CColor, FontSystem, Metrics, Shaping, SwashCache,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent as KbKeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT},
        Capability, SeatHandler, SeatState,
    },
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
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use cpal::traits::{DeviceTrait, HostTrait};

use crate::config::{Config, ServerConfig};

// ---- Styling constants ----

const PANEL_WIDTH: f32 = 560.0;
const PANEL_HEIGHT: f32 = 420.0;
const PANEL_CORNER_RADIUS: f32 = 16.0;
const PANEL_PADDING: f32 = 24.0;
const PANEL_BG_R: u8 = 0x1A;
const PANEL_BG_G: u8 = 0x1A;
const PANEL_BG_B: u8 = 0x2E;
const PANEL_BG_ALPHA: u8 = 0xFF;
const BORDER_R: u8 = 0x58;
const BORDER_G: u8 = 0x58;
const BORDER_B: u8 = 0x80;
const BORDER_ALPHA: u8 = 0xFF;
const BORDER_WIDTH: f32 = 2.0;

const TITLE_FONT_SIZE: f32 = 32.0;
const TITLE_LINE_HEIGHT: f32 = 40.0;

const SEPARATOR_GAP: f32 = 12.0;

const ROW_HEIGHT: f32 = 44.0;
const LABEL_WIDTH: f32 = 150.0;
const FIELD_FONT_SIZE: f32 = 15.0;
const FIELD_LINE_HEIGHT: f32 = 20.0;

// Row highlight colors (opaque, slightly lighter than panel bg)
const ROW_HOVER_R: u8 = 0x24;
const ROW_HOVER_G: u8 = 0x24;
const ROW_HOVER_B: u8 = 0x3A;
const ROW_FOCUS_R: u8 = 0x2E;
const ROW_FOCUS_G: u8 = 0x2E;
const ROW_FOCUS_B: u8 = 0x48;

// Toggle badge
const TOGGLE_WIDTH: u32 = 52;
const TOGGLE_HEIGHT: u32 = 26;
const TOGGLE_CORNER_RADIUS: f32 = 13.0;
const TOGGLE_ON_R: u8 = 0x28;
const TOGGLE_ON_G: u8 = 0x6B;
const TOGGLE_ON_B: u8 = 0x3A;
const TOGGLE_OFF_R: u8 = 0x5A;
const TOGGLE_OFF_G: u8 = 0x28;
const TOGGLE_OFF_B: u8 = 0x28;

// Cursor
const CURSOR_WIDTH: f32 = 2.0;

// Dismiss button
const BTN_WIDTH: u32 = 120;
const BTN_HEIGHT: u32 = 36;
const BTN_CORNER_RADIUS: f32 = 10.0;
const BTN_BG_R: u8 = 0x2A;
const BTN_BG_G: u8 = 0x2A;
const BTN_BG_B: u8 = 0x42;
const BTN_HOVER_R: u8 = 0x3A;
const BTN_HOVER_G: u8 = 0x3A;
const BTN_HOVER_B: u8 = 0x5A;
const BTN_FONT_SIZE: f32 = 15.0;
const BTN_LINE_HEIGHT: f32 = 20.0;

// Edit config button (smaller, inline with path)
const EDIT_BTN_WIDTH: u32 = 56;
const EDIT_BTN_HEIGHT: u32 = 26;
const CONFIG_PATH_FONT_SIZE: f32 = 12.0;
const CONFIG_PATH_LINE_HEIGHT: f32 = 16.0;

const BACKDROP_ALPHA: u8 = 0x60;

// ---- Field definitions ----

const FIELD_INPUT_DEVICE: usize = 0;
const FIELD_SERVER_URL: usize = 1;
const FIELD_BLIP_DEVICE: usize = 2;
const FIELD_BLIPS_ENABLED: usize = 3;
const FIELD_MIDI_CC: usize = 4;
const FIELD_COUNT: usize = 5;

const FIELD_LABELS: [&str; FIELD_COUNT] = [
    "Input Device",
    "Server URL",
    "Blip Device",
    "Blips",
    "MIDI CC",
];

// ---- Field kind + dropdown ----

#[derive(Clone, Copy, PartialEq)]
enum FieldKind {
    Text,
    Toggle,
    DevicePicker,
}

fn field_kind(index: usize) -> FieldKind {
    match index {
        FIELD_INPUT_DEVICE | FIELD_BLIP_DEVICE => FieldKind::DevicePicker,
        FIELD_BLIPS_ENABLED => FieldKind::Toggle,
        _ => FieldKind::Text,
    }
}

struct DropdownState {
    field_index: usize,
    items: Vec<String>,
    highlighted: usize,
    scroll_offset: usize,
}

const DROPDOWN_MAX_VISIBLE: usize = 6;

// ---- Public API ----

#[allow(dead_code)]
pub enum ConfigCommand {
    Close,
}

pub struct ConfigHandle {
    #[allow(dead_code)]
    tx: mpsc::Sender<ConfigCommand>,
    join: std::thread::JoinHandle<()>,
}

impl ConfigHandle {
    pub fn wait(self) {
        let _ = self.join.join();
    }
}

pub fn spawn_config_overlay() -> Result<ConfigHandle> {
    let (tx, rx) = mpsc::channel();
    let join = std::thread::spawn(move || {
        if let Err(e) = run_config_thread(rx) {
            warn!(error = %e, "config overlay thread failed");
        }
    });
    Ok(ConfigHandle { tx, join })
}

// ---- Internal state ----

#[allow(dead_code)]
struct ConfigState {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    font_system: FontSystem,
    swash_cache: SwashCache,
    rx: mpsc::Receiver<ConfigCommand>,
    width: u32,
    height: u32,
    first_configure: bool,
    done: bool,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,

    // Config field values
    server_url: String,
    blip_device: String,
    blips_enabled: bool,
    midi_cc_str: String,
    input_device: String,

    // Device lists (enumerated once at startup)
    input_device_list: Vec<String>,
    output_device_list: Vec<String>,

    // Config file path (resolved once at startup)
    config_path: String,

    // UI state
    focused_field: Option<usize>,
    cursor_pos: usize,
    hover_row: Option<usize>,
    btn_hover: bool,
    edit_btn_hover: bool,
    cursor_blink_start: Instant,
    dropdown: Option<DropdownState>,
}

// ---- Layout helpers ----

struct PanelLayout {
    px: f32,
    py: f32,
    title_y: f32,
    sep_y: f32,
    rows_y: f32,
    config_path_y: f32,
    edit_btn_x: f32,
    edit_btn_y: f32,
    btn_x: f32,
    btn_y: f32,
}

fn panel_layout(width: u32, height: u32) -> PanelLayout {
    let px = (width as f32 - PANEL_WIDTH) / 2.0;
    let py = (height as f32 - PANEL_HEIGHT) / 2.0;
    let title_y = py + PANEL_PADDING;
    let sep_y = title_y + TITLE_LINE_HEIGHT + SEPARATOR_GAP;
    let rows_y = sep_y + 1.0 + SEPARATOR_GAP;
    let btn_x = px + (PANEL_WIDTH - BTN_WIDTH as f32) / 2.0;
    let btn_y = py + PANEL_HEIGHT - PANEL_PADDING - BTN_HEIGHT as f32;
    let config_path_y = rows_y + FIELD_COUNT as f32 * ROW_HEIGHT + 8.0;
    let edit_btn_x = px + PANEL_WIDTH - PANEL_PADDING - EDIT_BTN_WIDTH as f32;
    let edit_btn_y = config_path_y;
    PanelLayout {
        px,
        py,
        title_y,
        sep_y,
        rows_y,
        config_path_y,
        edit_btn_x,
        edit_btn_y,
        btn_x,
        btn_y,
    }
}

fn run_config_thread(rx: mpsc::Receiver<ConfigCommand>) -> Result<()> {
    info!("config overlay thread starting");

    let conn = Connection::connect_to_env().context("failed to connect to Wayland")?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).context("wl_compositor not available")?;
    let layer_shell =
        LayerShell::bind(&globals, &qh).context("wlr-layer-shell not available")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;
    let seat_state = SeatState::new(&globals, &qh);

    let surface = compositor.create_surface(&qh);
    let layer = layer_shell.create_layer_surface(
        &qh,
        surface,
        Layer::Overlay,
        Some("justspeak-config"),
        None,
    );

    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    layer.commit();

    let font_system = FontSystem::new();
    let swash_cache = SwashCache::new();
    let pool = SlotPool::new(256 * 256 * 4, &shm)?;

    // Enumerate audio devices once at startup.
    // Input devices: cpal names (used by audio.rs to match via cpal).
    // Output/blip devices: PipeWire sink node names (used via PIPEWIRE_NODE env var).
    let audio_host = cpal::default_host();
    let input_device_list: Vec<String> = audio_host
        .input_devices()
        .map(|devs| {
            devs.filter_map(|d| d.name().ok())
                .collect()
        })
        .unwrap_or_default();
    let output_device_list: Vec<String> = enumerate_pipewire_sinks();
    info!(
        input_count = input_device_list.len(),
        output_count = output_device_list.len(),
        "enumerated audio devices"
    );

    let config = Config::load();

    let mut state = ConfigState {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        seat_state,
        shm,
        pool,
        layer,
        font_system,
        swash_cache,
        rx,
        width: 0,
        height: 0,
        first_configure: true,
        done: false,
        pointer: None,
        keyboard: None,

        server_url: config.server.url,
        blip_device: config.blip_device.unwrap_or_default(),
        blips_enabled: config.blips_enabled,
        midi_cc_str: config.midi_cc.map(|v| v.to_string()).unwrap_or_default(),
        input_device: config.input_device.unwrap_or_default(),

        input_device_list,
        output_device_list,

        config_path: Config::config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),

        focused_field: None,
        cursor_pos: 0,
        hover_row: None,
        btn_hover: false,
        edit_btn_hover: false,
        cursor_blink_start: Instant::now(),
        dropdown: None,
    };

    while !state.done {
        event_queue.blocking_dispatch(&mut state)?;
    }

    info!("config overlay thread finished");
    Ok(())
}

// ---- Drawing and input ----

impl ConfigState {
    fn poll_commands(&mut self) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                ConfigCommand::Close => self.done = true,
            }
        }
    }

    fn field_value(&self, index: usize) -> &str {
        match index {
            FIELD_SERVER_URL => &self.server_url,
            FIELD_BLIP_DEVICE => &self.blip_device,
            FIELD_MIDI_CC => &self.midi_cc_str,
            FIELD_INPUT_DEVICE => &self.input_device,
            _ => "",
        }
    }

    fn field_value_mut(&mut self, index: usize) -> Option<&mut String> {
        match index {
            FIELD_SERVER_URL => Some(&mut self.server_url),
            FIELD_BLIP_DEVICE => Some(&mut self.blip_device),
            FIELD_MIDI_CC => Some(&mut self.midi_cc_str),
            FIELD_INPUT_DEVICE => Some(&mut self.input_device),
            _ => None,
        }
    }

    fn is_text_field(index: usize) -> bool {
        field_kind(index) == FieldKind::Text
    }

    fn save_config(&self) {
        let config = Config {
            server: ServerConfig {
                url: self.server_url.clone(),
            },
            blip_device: if self.blip_device.is_empty() {
                None
            } else {
                Some(self.blip_device.clone())
            },
            blips_enabled: self.blips_enabled,
            midi_cc: self.midi_cc_str.parse().ok(),
            input_device: if self.input_device.is_empty() {
                None
            } else {
                Some(self.input_device.clone())
            },
        };
        config.save();
        Config::request_reload();
    }

    fn focus_field(&mut self, index: usize) {
        if Self::is_text_field(index) {
            self.focused_field = Some(index);
            self.cursor_pos = self.field_value(index).chars().count();
            self.cursor_blink_start = Instant::now();
        }
    }

    fn unfocus(&mut self) {
        if self.focused_field.is_some() {
            self.focused_field = None;
            self.save_config();
        }
    }

    fn open_dropdown(&mut self, field_index: usize) {
        let device_list = match field_index {
            FIELD_INPUT_DEVICE => &self.input_device_list,
            FIELD_BLIP_DEVICE => &self.output_device_list,
            _ => return,
        };
        let placeholder = if field_index == FIELD_INPUT_DEVICE {
            "(default)"
        } else {
            "(none)"
        };
        let mut items = vec![placeholder.to_string()];
        items.extend(device_list.iter().cloned());

        let current_value = self.field_value(field_index);
        let highlighted = if current_value.is_empty() {
            0
        } else {
            items
                .iter()
                .position(|s| s == current_value)
                .unwrap_or(0)
        };
        let scroll_offset = highlighted.saturating_sub(DROPDOWN_MAX_VISIBLE / 2);

        self.unfocus();
        self.dropdown = Some(DropdownState {
            field_index,
            items,
            highlighted,
            scroll_offset,
        });
    }

    fn close_dropdown(&mut self) {
        self.dropdown = None;
    }

    fn select_dropdown_item(&mut self) {
        if let Some(dd) = self.dropdown.take() {
            let value = &dd.items[dd.highlighted];
            let is_placeholder = dd.highlighted == 0;
            let new_val = if is_placeholder {
                String::new()
            } else {
                value.clone()
            };
            match dd.field_index {
                FIELD_INPUT_DEVICE => self.input_device = new_val,
                FIELD_BLIP_DEVICE => self.blip_device = new_val,
                _ => {}
            }
            self.save_config();
        }
    }

    fn hit_test_row(&self, x: f64, y: f64) -> Option<usize> {
        let l = panel_layout(self.width, self.height);
        let fy = y as f32;
        let fx = x as f32;
        if fx < l.px || fx > l.px + PANEL_WIDTH {
            return None;
        }
        for i in 0..FIELD_COUNT {
            let ry = l.rows_y + i as f32 * ROW_HEIGHT;
            if fy >= ry && fy < ry + ROW_HEIGHT {
                return Some(i);
            }
        }
        None
    }

    fn hit_test_btn(&self, x: f64, y: f64) -> bool {
        let l = panel_layout(self.width, self.height);
        let fx = x as f32;
        let fy = y as f32;
        fx >= l.btn_x
            && fx < l.btn_x + BTN_WIDTH as f32
            && fy >= l.btn_y
            && fy < l.btn_y + BTN_HEIGHT as f32
    }

    fn hit_test_dropdown(&self, x: f64, y: f64) -> Option<usize> {
        let dd = self.dropdown.as_ref()?;
        let l = panel_layout(self.width, self.height);
        let fx = x as f32;
        let fy = y as f32;

        let anchor_y = l.rows_y + dd.field_index as f32 * ROW_HEIGHT + ROW_HEIGHT;
        let dd_x = l.px + PANEL_PADDING + LABEL_WIDTH;
        let dd_w = PANEL_WIDTH - PANEL_PADDING - LABEL_WIDTH - PANEL_PADDING;
        let visible_count = dd.items.len().min(DROPDOWN_MAX_VISIBLE);

        if fx < dd_x || fx > dd_x + dd_w {
            return None;
        }
        if fy < anchor_y || fy >= anchor_y + visible_count as f32 * ROW_HEIGHT {
            return None;
        }
        let row = ((fy - anchor_y) / ROW_HEIGHT) as usize;
        Some(dd.scroll_offset + row)
    }

    fn hit_test_edit_btn(&self, x: f64, y: f64) -> bool {
        let l = panel_layout(self.width, self.height);
        let fx = x as f32;
        let fy = y as f32;
        fx >= l.edit_btn_x
            && fx < l.edit_btn_x + EDIT_BTN_WIDTH as f32
            && fy >= l.edit_btn_y
            && fy < l.edit_btn_y + EDIT_BTN_HEIGHT as f32
    }

    fn open_config_in_editor(&self) {
        if self.config_path.is_empty() {
            return;
        }
        let path = self.config_path.clone();
        std::thread::spawn(move || {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "xdg-open".to_string());
            info!(editor = %editor, path = %path, "opening config in editor");
            if let Err(e) = std::process::Command::new(&editor).arg(&path).spawn() {
                warn!(error = %e, editor = %editor, "failed to open editor");
            }
        });
    }

    fn is_over_panel(&self, x: f64, y: f64) -> bool {
        let l = panel_layout(self.width, self.height);
        let fx = x as f32;
        let fy = y as f32;
        fx >= l.px
            && fx < l.px + PANEL_WIDTH
            && fy >= l.py
            && fy < l.py + PANEL_HEIGHT
    }

    fn handle_click(&mut self, x: f64, y: f64) {
        // If dropdown is open, check it first
        if self.dropdown.is_some() {
            if let Some(item_idx) = self.hit_test_dropdown(x, y) {
                if let Some(dd) = self.dropdown.as_mut() {
                    if item_idx < dd.items.len() {
                        dd.highlighted = item_idx;
                    }
                }
                self.select_dropdown_item();
                return;
            }
            // Click outside dropdown closes it
            self.close_dropdown();
            // Fall through to handle the click normally
        }

        if self.hit_test_edit_btn(x, y) {
            self.save_config();
            self.open_config_in_editor();
            self.done = true;
            return;
        }

        if self.hit_test_btn(x, y) || !self.is_over_panel(x, y) {
            self.unfocus();
            self.save_config();
            self.done = true;
            return;
        }

        if let Some(row) = self.hit_test_row(x, y) {
            match field_kind(row) {
                FieldKind::Toggle => {
                    self.unfocus();
                    self.blips_enabled = !self.blips_enabled;
                    self.save_config();
                }
                FieldKind::DevicePicker => {
                    self.open_dropdown(row);
                }
                FieldKind::Text => {
                    if self.focused_field != Some(row) {
                        self.unfocus();
                        self.focus_field(row);
                    }
                }
            }
        } else {
            self.unfocus();
        }
    }

    fn handle_key(&mut self, event: &KbKeyEvent) {
        let keysym = event.keysym;

        // Dropdown keyboard handling takes priority
        if self.dropdown.is_some() {
            match keysym {
                Keysym::Escape => {
                    self.close_dropdown();
                    return;
                }
                Keysym::Return | Keysym::KP_Enter => {
                    self.select_dropdown_item();
                    return;
                }
                Keysym::Up => {
                    if let Some(dd) = self.dropdown.as_mut() {
                        if dd.highlighted > 0 {
                            dd.highlighted -= 1;
                            if dd.highlighted < dd.scroll_offset {
                                dd.scroll_offset = dd.highlighted;
                            }
                        }
                    }
                    return;
                }
                Keysym::Down => {
                    if let Some(dd) = self.dropdown.as_mut() {
                        if dd.highlighted + 1 < dd.items.len() {
                            dd.highlighted += 1;
                            if dd.highlighted >= dd.scroll_offset + DROPDOWN_MAX_VISIBLE {
                                dd.scroll_offset = dd.highlighted + 1 - DROPDOWN_MAX_VISIBLE;
                            }
                        }
                    }
                    return;
                }
                Keysym::Home => {
                    if let Some(dd) = self.dropdown.as_mut() {
                        dd.highlighted = 0;
                        dd.scroll_offset = 0;
                    }
                    return;
                }
                Keysym::End => {
                    if let Some(dd) = self.dropdown.as_mut() {
                        dd.highlighted = dd.items.len().saturating_sub(1);
                        dd.scroll_offset = dd
                            .items
                            .len()
                            .saturating_sub(DROPDOWN_MAX_VISIBLE);
                    }
                    return;
                }
                _ => return,
            }
        }

        if keysym == Keysym::Escape {
            self.unfocus();
            self.done = true;
            return;
        }

        let Some(field_idx) = self.focused_field else {
            if keysym == Keysym::Tab {
                self.focus_field(FIELD_SERVER_URL);
            }
            return;
        };

        if keysym == Keysym::Tab {
            self.unfocus();
            // Skip non-text fields (DevicePicker, Toggle)
            let mut next = (field_idx + 1) % FIELD_COUNT;
            let mut attempts = 0;
            while field_kind(next) != FieldKind::Text && attempts < FIELD_COUNT {
                next = (next + 1) % FIELD_COUNT;
                attempts += 1;
            }
            self.focus_field(next);
            return;
        }

        if keysym == Keysym::Return || keysym == Keysym::KP_Enter {
            self.unfocus();
            return;
        }

        let value_len = self.field_value(field_idx).chars().count();

        match keysym {
            Keysym::BackSpace => {
                if self.cursor_pos > 0 {
                    let byte_pos = self
                        .field_value(field_idx)
                        .char_indices()
                        .nth(self.cursor_pos - 1)
                        .map(|(i, _)| i);
                    let byte_end = self
                        .field_value(field_idx)
                        .char_indices()
                        .nth(self.cursor_pos)
                        .map(|(i, _)| i)
                        .unwrap_or(self.field_value(field_idx).len());
                    if let (Some(start), Some(val)) = (byte_pos, self.field_value_mut(field_idx)) {
                        val.replace_range(start..byte_end, "");
                    }
                    self.cursor_pos -= 1;
                    self.cursor_blink_start = Instant::now();
                }
            }
            Keysym::Delete => {
                if self.cursor_pos < value_len {
                    let byte_start = self
                        .field_value(field_idx)
                        .char_indices()
                        .nth(self.cursor_pos)
                        .map(|(i, _)| i);
                    let byte_end = self
                        .field_value(field_idx)
                        .char_indices()
                        .nth(self.cursor_pos + 1)
                        .map(|(i, _)| i)
                        .unwrap_or(self.field_value(field_idx).len());
                    if let (Some(start), Some(val)) =
                        (byte_start, self.field_value_mut(field_idx))
                    {
                        val.replace_range(start..byte_end, "");
                    }
                    self.cursor_blink_start = Instant::now();
                }
            }
            Keysym::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.cursor_blink_start = Instant::now();
                }
            }
            Keysym::Right => {
                if self.cursor_pos < value_len {
                    self.cursor_pos += 1;
                    self.cursor_blink_start = Instant::now();
                }
            }
            Keysym::Home => {
                self.cursor_pos = 0;
                self.cursor_blink_start = Instant::now();
            }
            Keysym::End => {
                self.cursor_pos = value_len;
                self.cursor_blink_start = Instant::now();
            }
            _ => {
                if let Some(ref utf8) = event.utf8 {
                    // Filter control characters
                    if !utf8.is_empty() && utf8.chars().all(|c| !c.is_control()) {
                        let byte_pos = self
                            .field_value(field_idx)
                            .char_indices()
                            .nth(self.cursor_pos)
                            .map(|(i, _)| i)
                            .unwrap_or(self.field_value(field_idx).len());
                        if let Some(val) = self.field_value_mut(field_idx) {
                            val.insert_str(byte_pos, utf8);
                        }
                        self.cursor_pos += utf8.chars().count();
                        self.cursor_blink_start = Instant::now();
                    }
                }
            }
        }
    }

    fn text_width(&mut self, text: &str) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let metrics = Metrics::new(FIELD_FONT_SIZE, FIELD_LINE_HEIGHT);
        let mut buf = TextBuffer::new(&mut self.font_system, metrics);
        buf.set_size(
            &mut self.font_system,
            Some(10000.0),
            Some(FIELD_LINE_HEIGHT + 10.0),
        );
        buf.set_text(
            &mut self.font_system,
            text,
            Attrs::new().family(cosmic_text::Family::SansSerif),
            Shaping::Advanced,
        );
        buf.shape_until_scroll(&mut self.font_system, false);
        buf.layout_runs()
            .next()
            .map(|r| r.line_w)
            .unwrap_or(0.0)
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

        // Snapshot UI state and pre-compute values before borrowing pool/canvas
        let focused_field = self.focused_field;
        let hover_row = self.hover_row;
        let blips_enabled = self.blips_enabled;
        let cursor_pos = self.cursor_pos;
        let cursor_visible = if focused_field.is_some() {
            (Instant::now()
                .duration_since(self.cursor_blink_start)
                .as_millis()
                / 530)
                % 2
                == 0
        } else {
            false
        };

        let field_values: Vec<String> = (0..FIELD_COUNT)
            .map(|i| match field_kind(i) {
                FieldKind::Toggle => String::new(),
                _ => match i {
                    FIELD_SERVER_URL => self.server_url.clone(),
                    FIELD_BLIP_DEVICE => self.blip_device.clone(),
                    FIELD_MIDI_CC => self.midi_cc_str.clone(),
                    FIELD_INPUT_DEVICE => self.input_device.clone(),
                    _ => String::new(),
                },
            })
            .collect();

        // Snapshot dropdown state for rendering
        let dd_snapshot: Option<(usize, Vec<String>, usize, usize)> =
            self.dropdown.as_ref().map(|dd| {
                (
                    dd.field_index,
                    dd.items.clone(),
                    dd.highlighted,
                    dd.scroll_offset,
                )
            });

        // Pre-measure cursor offset for the focused text field
        let cursor_x_offset = if let Some(fi) = focused_field {
            if Self::is_text_field(fi) && cursor_visible {
                let val = &field_values[fi];
                let byte_offset: usize = val
                    .char_indices()
                    .nth(cursor_pos)
                    .map(|(idx, _)| idx)
                    .unwrap_or(val.len());
                let prefix = &val[..byte_offset];
                self.text_width(prefix)
            } else {
                0.0
            }
        } else {
            0.0
        };

        let stride = width as i32 * 4;
        let buf_size = (stride * height as i32) as usize;
        if self.pool.len() < buf_size {
            self.pool.resize(buf_size).ok();
        }

        let (buffer, canvas) = self
            .pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .expect("create buffer");

        let cw = width as usize;
        let ch = height as usize;

        // Dimmed backdrop
        let backdrop = premul_argb(0x00, 0x00, 0x00, BACKDROP_ALPHA);
        let backdrop_bytes = backdrop.to_le_bytes();
        for pixel in canvas.chunks_exact_mut(4) {
            pixel.copy_from_slice(&backdrop_bytes);
        }

        let l = panel_layout(width, height);
        let fill = premul_argb(PANEL_BG_R, PANEL_BG_G, PANEL_BG_B, PANEL_BG_ALPHA);
        let border = premul_argb(BORDER_R, BORDER_G, BORDER_B, BORDER_ALPHA);

        // Panel background
        draw_rounded_rect(
            canvas,
            cw,
            ch,
            l.px as i32,
            l.py as i32,
            PANEL_WIDTH as u32,
            PANEL_HEIGHT as u32,
            PANEL_CORNER_RADIUS,
            fill,
            border,
            BORDER_WIDTH,
        );

        // Title
        {
            let metrics = Metrics::new(TITLE_FONT_SIZE, TITLE_LINE_HEIGHT);
            let mut title_buf = TextBuffer::new(&mut self.font_system, metrics);
            title_buf.set_size(
                &mut self.font_system,
                Some(PANEL_WIDTH),
                Some(TITLE_LINE_HEIGHT + 10.0),
            );
            title_buf.set_text(
                &mut self.font_system,
                "Just Talk",
                Attrs::new().family(cosmic_text::Family::SansSerif),
                Shaping::Advanced,
            );
            title_buf.shape_until_scroll(&mut self.font_system, false);

            let mut tw = 0.0_f32;
            for run in title_buf.layout_runs() {
                tw = tw.max(run.line_w);
            }
            let text_ox = l.px as i32 + ((PANEL_WIDTH - tw) / 2.0) as i32;

            render_text(
                &mut self.font_system,
                &mut self.swash_cache,
                &mut title_buf,
                canvas,
                cw,
                ch,
                text_ox,
                l.title_y as i32,
                0xFF,
            );
        }

        // Separator line
        {
            let sep_x0 = (l.px + PANEL_PADDING) as usize;
            let sep_x1 = (l.px + PANEL_WIDTH - PANEL_PADDING) as usize;
            let sep_y = l.sep_y as usize;
            let sep_color = premul_argb(BORDER_R, BORDER_G, BORDER_B, 0x80);
            for x in sep_x0..sep_x1.min(cw) {
                put_pixel(canvas, cw, ch, x, sep_y, sep_color);
            }
        }

        // Field rows
        for i in 0..FIELD_COUNT {
            let ry = l.rows_y + i as f32 * ROW_HEIGHT;
            let row_x = l.px + PANEL_PADDING;
            let value_x = l.px + PANEL_PADDING + LABEL_WIDTH;

            let is_dropdown_active = dd_snapshot
                .as_ref()
                .map(|(fi, _, _, _)| *fi == i)
                .unwrap_or(false);
            let is_focused = focused_field == Some(i) || is_dropdown_active;
            let is_hovered = hover_row == Some(i) && !is_focused;

            // Row highlight (opaque)
            if is_focused || is_hovered {
                let highlight = if is_focused {
                    premul_argb(ROW_FOCUS_R, ROW_FOCUS_G, ROW_FOCUS_B, 0xFF)
                } else {
                    premul_argb(ROW_HOVER_R, ROW_HOVER_G, ROW_HOVER_B, 0xFF)
                };
                fill_rect(
                    canvas,
                    cw,
                    ch,
                    (l.px + PANEL_PADDING / 2.0) as i32,
                    ry as i32,
                    (PANEL_WIDTH - PANEL_PADDING) as u32,
                    ROW_HEIGHT as u32,
                    highlight,
                );
            }

            // Label
            let label_y = ry + (ROW_HEIGHT - FIELD_LINE_HEIGHT) / 2.0;
            {
                let metrics = Metrics::new(FIELD_FONT_SIZE, FIELD_LINE_HEIGHT);
                let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                buf.set_size(
                    &mut self.font_system,
                    Some(LABEL_WIDTH),
                    Some(FIELD_LINE_HEIGHT + 10.0),
                );
                buf.set_text(
                    &mut self.font_system,
                    FIELD_LABELS[i],
                    Attrs::new().family(cosmic_text::Family::SansSerif),
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut self.font_system, false);

                render_text(
                    &mut self.font_system,
                    &mut self.swash_cache,
                    &mut buf,
                    canvas,
                    cw,
                    ch,
                    row_x as i32,
                    label_y as i32,
                    0xAA,
                );
            }

            // Value
            if i == FIELD_BLIPS_ENABLED {
                // Toggle badge
                let toggle_x = value_x as i32;
                let toggle_y = (ry + (ROW_HEIGHT - TOGGLE_HEIGHT as f32) / 2.0) as i32;
                let (tr, tg, tb) = if blips_enabled {
                    (TOGGLE_ON_R, TOGGLE_ON_G, TOGGLE_ON_B)
                } else {
                    (TOGGLE_OFF_R, TOGGLE_OFF_G, TOGGLE_OFF_B)
                };
                let toggle_fill = premul_argb(tr, tg, tb, PANEL_BG_ALPHA);
                draw_rounded_rect(
                    canvas,
                    cw,
                    ch,
                    toggle_x,
                    toggle_y,
                    TOGGLE_WIDTH,
                    TOGGLE_HEIGHT,
                    TOGGLE_CORNER_RADIUS,
                    toggle_fill,
                    border,
                    BORDER_WIDTH,
                );

                let toggle_text = if blips_enabled { "ON" } else { "OFF" };
                let metrics = Metrics::new(FIELD_FONT_SIZE, FIELD_LINE_HEIGHT);
                let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                buf.set_size(
                    &mut self.font_system,
                    Some(TOGGLE_WIDTH as f32),
                    Some(TOGGLE_HEIGHT as f32),
                );
                buf.set_text(
                    &mut self.font_system,
                    toggle_text,
                    Attrs::new().family(cosmic_text::Family::SansSerif),
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
                let mut tw = 0.0_f32;
                for run in buf.layout_runs() {
                    tw = tw.max(run.line_w);
                }
                let tx = toggle_x + ((TOGGLE_WIDTH as f32 - tw) / 2.0) as i32;
                let ty = toggle_y + ((TOGGLE_HEIGHT as f32 - FIELD_FONT_SIZE) / 2.0) as i32;
                render_text(
                    &mut self.font_system,
                    &mut self.swash_cache,
                    &mut buf,
                    canvas,
                    cw,
                    ch,
                    tx,
                    ty,
                    0xFF,
                );
            } else if field_kind(i) == FieldKind::DevicePicker {
                // Device picker: read-only value + dropdown indicator
                let val = &field_values[i];
                let placeholder = if i == FIELD_INPUT_DEVICE {
                    "(default)"
                } else {
                    "(none)"
                };
                let display_text = if val.is_empty() {
                    placeholder
                } else {
                    val.as_str()
                };
                let alpha = if val.is_empty() { 0x66 } else { 0xFF };

                let field_w = PANEL_WIDTH - PANEL_PADDING - LABEL_WIDTH - PANEL_PADDING;

                // Render value text
                {
                    let metrics = Metrics::new(FIELD_FONT_SIZE, FIELD_LINE_HEIGHT);
                    let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                    buf.set_size(
                        &mut self.font_system,
                        Some(field_w - 20.0), // leave room for indicator
                        Some(FIELD_LINE_HEIGHT + 10.0),
                    );
                    buf.set_text(
                        &mut self.font_system,
                        display_text,
                        Attrs::new().family(cosmic_text::Family::SansSerif),
                        Shaping::Advanced,
                    );
                    buf.shape_until_scroll(&mut self.font_system, false);

                    render_text(
                        &mut self.font_system,
                        &mut self.swash_cache,
                        &mut buf,
                        canvas,
                        cw,
                        ch,
                        value_x as i32,
                        label_y as i32,
                        alpha,
                    );
                }

                // Dropdown indicator "▾"
                {
                    let indicator_x = (value_x + field_w - 18.0) as i32;
                    let metrics = Metrics::new(FIELD_FONT_SIZE, FIELD_LINE_HEIGHT);
                    let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                    buf.set_size(
                        &mut self.font_system,
                        Some(20.0),
                        Some(FIELD_LINE_HEIGHT + 10.0),
                    );
                    buf.set_text(
                        &mut self.font_system,
                        "\u{25BE}",
                        Attrs::new().family(cosmic_text::Family::SansSerif),
                        Shaping::Advanced,
                    );
                    buf.shape_until_scroll(&mut self.font_system, false);
                    render_text(
                        &mut self.font_system,
                        &mut self.swash_cache,
                        &mut buf,
                        canvas,
                        cw,
                        ch,
                        indicator_x,
                        label_y as i32,
                        0x88,
                    );
                }
            } else {
                // Text field with editable cursor
                let val = &field_values[i];
                let display_text = if val.is_empty() { "(none)" } else { val.as_str() };
                let alpha = if val.is_empty() && !is_focused {
                    0x66
                } else {
                    0xFF
                };

                // When focused on empty field, show cursor without placeholder
                let render_val = if is_focused && val.is_empty() {
                    ""
                } else {
                    display_text
                };

                if !render_val.is_empty() {
                    let metrics = Metrics::new(FIELD_FONT_SIZE, FIELD_LINE_HEIGHT);
                    let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                    buf.set_size(
                        &mut self.font_system,
                        Some(PANEL_WIDTH - PANEL_PADDING - LABEL_WIDTH - PANEL_PADDING),
                        Some(FIELD_LINE_HEIGHT + 10.0),
                    );
                    buf.set_text(
                        &mut self.font_system,
                        render_val,
                        Attrs::new().family(cosmic_text::Family::SansSerif),
                        Shaping::Advanced,
                    );
                    buf.shape_until_scroll(&mut self.font_system, false);

                    render_text(
                        &mut self.font_system,
                        &mut self.swash_cache,
                        &mut buf,
                        canvas,
                        cw,
                        ch,
                        value_x as i32,
                        label_y as i32,
                        alpha,
                    );
                }

                // Cursor (uses pre-computed cursor_x_offset)
                if is_focused && cursor_visible {
                    let cx = value_x + cursor_x_offset;
                    let cursor_color = premul_argb(0xFF, 0xFF, 0xFF, 0xDD);
                    fill_rect(
                        canvas,
                        cw,
                        ch,
                        cx as i32,
                        label_y as i32,
                        CURSOR_WIDTH as u32,
                        FIELD_LINE_HEIGHT as u32,
                        cursor_color,
                    );
                }
            }
        }

        // Dropdown overlay (rendered on top of subsequent rows)
        if let Some((dd_field, dd_items, dd_highlighted, dd_scroll)) = dd_snapshot {
            let anchor_y = l.rows_y + dd_field as f32 * ROW_HEIGHT + ROW_HEIGHT;
            let dd_x = l.px + PANEL_PADDING + LABEL_WIDTH;
            let dd_w = PANEL_WIDTH - PANEL_PADDING - LABEL_WIDTH - PANEL_PADDING;
            let visible_count = dd_items.len().min(DROPDOWN_MAX_VISIBLE);
            let dd_h = visible_count as f32 * ROW_HEIGHT;

            // Background
            let dd_bg = premul_argb(0x22, 0x22, 0x38, 0xFF);
            let dd_border = premul_argb(BORDER_R, BORDER_G, BORDER_B, 0xFF);
            draw_rounded_rect(
                canvas,
                cw,
                ch,
                dd_x as i32,
                anchor_y as i32,
                dd_w as u32,
                dd_h as u32 + 2, // +2 for border
                4.0,
                dd_bg,
                dd_border,
                1.0,
            );

            // Items
            for vi in 0..visible_count {
                let item_idx = dd_scroll + vi;
                if item_idx >= dd_items.len() {
                    break;
                }
                let item_y = anchor_y + vi as f32 * ROW_HEIGHT;
                let is_highlighted = item_idx == dd_highlighted;

                if is_highlighted {
                    let hl = premul_argb(0x3A, 0x3A, 0x5A, 0xFF);
                    fill_rect(
                        canvas,
                        cw,
                        ch,
                        (dd_x + 1.0) as i32,
                        item_y as i32,
                        (dd_w - 2.0) as u32,
                        ROW_HEIGHT as u32,
                        hl,
                    );
                }

                let item_text = &dd_items[item_idx];
                let alpha = if item_idx == 0 { 0x88 } else { 0xFF }; // placeholder dimmer
                let text_y = item_y + (ROW_HEIGHT - FIELD_LINE_HEIGHT) / 2.0;
                let metrics = Metrics::new(FIELD_FONT_SIZE, FIELD_LINE_HEIGHT);
                let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                buf.set_size(
                    &mut self.font_system,
                    Some(dd_w - 12.0),
                    Some(FIELD_LINE_HEIGHT + 10.0),
                );
                buf.set_text(
                    &mut self.font_system,
                    item_text,
                    Attrs::new().family(cosmic_text::Family::SansSerif),
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
                render_text(
                    &mut self.font_system,
                    &mut self.swash_cache,
                    &mut buf,
                    canvas,
                    cw,
                    ch,
                    (dd_x + 6.0) as i32,
                    text_y as i32,
                    alpha,
                );
            }
        }

        // Config file path + Edit button
        if !self.config_path.is_empty() {
            // Path text (dimmed, small)
            {
                let metrics = Metrics::new(CONFIG_PATH_FONT_SIZE, CONFIG_PATH_LINE_HEIGHT);
                let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                let max_path_w = PANEL_WIDTH - PANEL_PADDING * 2.0 - EDIT_BTN_WIDTH as f32 - 12.0;
                buf.set_size(
                    &mut self.font_system,
                    Some(max_path_w),
                    Some(CONFIG_PATH_LINE_HEIGHT + 10.0),
                );
                buf.set_text(
                    &mut self.font_system,
                    &self.config_path,
                    Attrs::new().family(cosmic_text::Family::SansSerif),
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut self.font_system, false);

                let text_y =
                    l.config_path_y + (EDIT_BTN_HEIGHT as f32 - CONFIG_PATH_LINE_HEIGHT) / 2.0;
                render_text(
                    &mut self.font_system,
                    &mut self.swash_cache,
                    &mut buf,
                    canvas,
                    cw,
                    ch,
                    (l.px + PANEL_PADDING) as i32,
                    text_y as i32,
                    0x66,
                );
            }

            // Edit button
            {
                let edit_fill = if self.edit_btn_hover {
                    premul_argb(BTN_HOVER_R, BTN_HOVER_G, BTN_HOVER_B, PANEL_BG_ALPHA)
                } else {
                    premul_argb(BTN_BG_R, BTN_BG_G, BTN_BG_B, PANEL_BG_ALPHA)
                };
                draw_rounded_rect(
                    canvas,
                    cw,
                    ch,
                    l.edit_btn_x as i32,
                    l.edit_btn_y as i32,
                    EDIT_BTN_WIDTH,
                    EDIT_BTN_HEIGHT,
                    6.0,
                    edit_fill,
                    border,
                    1.0,
                );

                let metrics = Metrics::new(CONFIG_PATH_FONT_SIZE, CONFIG_PATH_LINE_HEIGHT);
                let mut buf = TextBuffer::new(&mut self.font_system, metrics);
                buf.set_size(
                    &mut self.font_system,
                    Some(EDIT_BTN_WIDTH as f32),
                    Some(EDIT_BTN_HEIGHT as f32),
                );
                buf.set_text(
                    &mut self.font_system,
                    "Edit",
                    Attrs::new().family(cosmic_text::Family::SansSerif),
                    Shaping::Advanced,
                );
                buf.shape_until_scroll(&mut self.font_system, false);
                let mut tw = 0.0_f32;
                for run in buf.layout_runs() {
                    tw = tw.max(run.line_w);
                }
                let tx = l.edit_btn_x as i32 + ((EDIT_BTN_WIDTH as f32 - tw) / 2.0) as i32;
                let ty = l.edit_btn_y as i32
                    + ((EDIT_BTN_HEIGHT as f32 - CONFIG_PATH_FONT_SIZE) / 2.0) as i32;
                render_text(
                    &mut self.font_system,
                    &mut self.swash_cache,
                    &mut buf,
                    canvas,
                    cw,
                    ch,
                    tx,
                    ty,
                    0xCC,
                );
            }
        }

        // Dismiss button
        let btn_fill = if self.btn_hover {
            premul_argb(BTN_HOVER_R, BTN_HOVER_G, BTN_HOVER_B, PANEL_BG_ALPHA)
        } else {
            premul_argb(BTN_BG_R, BTN_BG_G, BTN_BG_B, PANEL_BG_ALPHA)
        };
        draw_rounded_rect(
            canvas,
            cw,
            ch,
            l.btn_x as i32,
            l.btn_y as i32,
            BTN_WIDTH,
            BTN_HEIGHT,
            BTN_CORNER_RADIUS,
            btn_fill,
            border,
            BORDER_WIDTH,
        );

        {
            let metrics = Metrics::new(BTN_FONT_SIZE, BTN_LINE_HEIGHT);
            let mut btn_buf = TextBuffer::new(&mut self.font_system, metrics);
            btn_buf.set_size(
                &mut self.font_system,
                Some(BTN_WIDTH as f32),
                Some(BTN_HEIGHT as f32),
            );
            btn_buf.set_text(
                &mut self.font_system,
                "Dismiss",
                Attrs::new().family(cosmic_text::Family::SansSerif),
                Shaping::Advanced,
            );
            btn_buf.shape_until_scroll(&mut self.font_system, false);
            let mut tw = 0.0_f32;
            for run in btn_buf.layout_runs() {
                tw = tw.max(run.line_w);
            }
            let text_ox = l.btn_x as i32 + ((BTN_WIDTH as f32 - tw) / 2.0) as i32;
            let text_oy = l.btn_y as i32 + ((BTN_HEIGHT as f32 - BTN_FONT_SIZE) / 2.0) as i32;

            render_text(
                &mut self.font_system,
                &mut self.swash_cache,
                &mut btn_buf,
                canvas,
                cw,
                ch,
                text_ox,
                text_oy,
                0xFF,
            );
        }

        self.commit_frame(qh, buffer, width, height);
    }

    fn commit_frame(
        &self,
        qh: &QueueHandle<Self>,
        buffer: smithay_client_toolkit::shm::slot::Buffer,
        width: u32,
        height: u32,
    ) {
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        self.layer
            .wl_surface()
            .frame(qh, self.layer.wl_surface().clone());
        buffer
            .attach_to(self.layer.wl_surface())
            .expect("buffer attach");
        self.layer.commit();
    }
}

// ---- Device enumeration ----

/// Enumerate PipeWire audio sinks by node name.
/// Blip device config uses PipeWire node names (routed via PIPEWIRE_NODE env var),
/// so we query `pactl list sinks short` which returns PipeWire/PulseAudio sink names.
fn enumerate_pipewire_sinks() -> Vec<String> {
    let output = match std::process::Command::new("pactl")
        .args(["list", "sinks", "short"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "failed to run pactl for sink enumeration");
            return Vec::new();
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            // Format: ID\tNAME\tDRIVER\tFORMAT\tSTATE
            let mut cols = line.split('\t');
            cols.next(); // skip ID
            cols.next().map(|name| name.to_string())
        })
        .collect()
}

// ---- Helper functions ----

fn premul_argb(r: u8, g: u8, b: u8, a: u8) -> u32 {
    let a32 = a as u32;
    (a32 << 24)
        | (r as u32 * a32 / 255) << 16
        | (g as u32 * a32 / 255) << 8
        | (b as u32 * a32 / 255)
}

fn put_pixel(canvas: &mut [u8], cw: usize, ch: usize, px: usize, py: usize, pixel: u32) {
    if px < cw && py < ch {
        let idx = (py * cw + px) * 4;
        if idx + 3 < canvas.len() {
            canvas[idx..idx + 4].copy_from_slice(&pixel.to_le_bytes());
        }
    }
}

fn fill_rect(
    canvas: &mut [u8],
    cw: usize,
    ch: usize,
    rx: i32,
    ry: i32,
    rw: u32,
    rh: u32,
    color: u32,
) {
    let x0 = rx.max(0) as usize;
    let y0 = ry.max(0) as usize;
    let x1 = ((rx + rw as i32) as usize).min(cw);
    let y1 = ((ry + rh as i32) as usize).min(ch);
    for py in y0..y1 {
        for px in x0..x1 {
            put_pixel(canvas, cw, ch, px, py, color);
        }
    }
}

fn render_text(
    fs: &mut FontSystem,
    sc: &mut SwashCache,
    buf: &mut TextBuffer,
    canvas: &mut [u8],
    cw: usize,
    ch: usize,
    ox: i32,
    oy: i32,
    alpha: u8,
) {
    let color = CColor::rgba(0xFF, 0xFF, 0xFF, alpha);
    buf.draw(fs, sc, color, |x, y, _w, _h, c| {
        let px = x + ox;
        let py = y + oy;
        if px < 0 || py < 0 {
            return;
        }
        let px = px as usize;
        let py = py as usize;
        if px >= cw || py >= ch {
            return;
        }
        let a = c.a();
        if a == 0 {
            return;
        }
        put_pixel(canvas, cw, ch, px, py, premul_argb(c.r(), c.g(), c.b(), a));
    });
}

fn corner_center(lx: f32, ly: f32, w: f32, h: f32, r: f32) -> (Option<f32>, Option<f32>) {
    let cx = if lx < r {
        Some(r)
    } else if lx > w - r {
        Some(w - r)
    } else {
        None
    };
    let cy = if ly < r {
        Some(r)
    } else if ly > h - r {
        Some(h - r)
    } else {
        None
    };
    (cx, cy)
}

#[allow(clippy::too_many_arguments)]
fn draw_rounded_rect(
    canvas: &mut [u8],
    cw: usize,
    ch: usize,
    rx: i32,
    ry: i32,
    rw: u32,
    rh: u32,
    radius: f32,
    fill: u32,
    border: u32,
    bw: f32,
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
            put_pixel(
                canvas,
                cw,
                ch,
                px,
                py,
                if inside { fill } else { border },
            );
        }
    }
}

// ---- Wayland trait impls ----

impl CompositorHandler for ConfigState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.draw(qh);
    }
    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for ConfigState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for ConfigState {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.done = true;
    }
    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if configure.new_size.0 != 0 && configure.new_size.1 != 0 {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }
        if self.first_configure {
            self.first_configure = false;
            self.draw(qh);
        }
    }
}

impl ShmHandler for ConfigState {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl SeatHandler for ConfigState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
    ) {
    }
    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none()
            && let Ok(ptr) = self.seat_state.get_pointer(qh, &seat)
        {
            self.pointer = Some(ptr);
        }
        if capability == Capability::Keyboard && self.keyboard.is_none()
            && let Ok(kbd) = self.seat_state.get_keyboard(qh, &seat, None)
        {
            self.keyboard = Some(kbd);
        }
    }
    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
        if capability == Capability::Keyboard {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
        }
    }
    fn remove_seat(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
    ) {
    }
}

impl PointerHandler for ConfigState {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    // Update dropdown highlight on hover
                    if let Some(item_idx) =
                        self.hit_test_dropdown(event.position.0, event.position.1)
                    {
                        if let Some(dd) = self.dropdown.as_mut() {
                            if item_idx < dd.items.len() {
                                dd.highlighted = item_idx;
                            }
                        }
                    }
                    self.hover_row = self.hit_test_row(event.position.0, event.position.1);
                    self.btn_hover = self.hit_test_btn(event.position.0, event.position.1);
                    self.edit_btn_hover =
                        self.hit_test_edit_btn(event.position.0, event.position.1);
                }
                PointerEventKind::Leave { .. } => {
                    self.hover_row = None;
                    self.btn_hover = false;
                    self.edit_btn_hover = false;
                }
                PointerEventKind::Press { button, .. } => {
                    if button == BTN_LEFT {
                        self.handle_click(event.position.0, event.position.1);
                    }
                }
                _ => {}
            }
        }
    }
}

impl KeyboardHandler for ConfigState {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KbKeyEvent,
    ) {
        self.handle_key(&event);
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KbKeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _modifiers: Modifiers,
        _layout: u32,
    ) {
    }
}

delegate_compositor!(ConfigState);
delegate_output!(ConfigState);
delegate_shm!(ConfigState);
delegate_layer!(ConfigState);
delegate_seat!(ConfigState);
delegate_pointer!(ConfigState);
delegate_keyboard!(ConfigState);
delegate_registry!(ConfigState);

impl ProvidesRegistryState for ConfigState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
