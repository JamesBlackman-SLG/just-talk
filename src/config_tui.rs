use crate::config::Config;
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    execute,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::fs::File;

const FIELD_COUNT: usize = 4;
const FIELD_INPUT_DEVICE: usize = 0;
const FIELD_SERVER_URL: usize = 1;
const FIELD_BLIP_DEVICE: usize = 2;
const FIELD_BLIPS_ENABLED: usize = 3;

const FIELD_LABELS: [&str; FIELD_COUNT] = [
    "Input Device",
    "Server URL",
    "Blip Device",
    "Blips Enabled",
];

struct App {
    selected: usize,
    editing: bool,
    input_device: String,
    server_url: String,
    blip_device: String,
    blips_enabled: bool,
    input_device_list: Vec<String>,
    output_device_list: Vec<String>,
    dropdown_open: bool,
    dropdown_state: ListState,
    dropdown_items: Vec<String>,
    status: Option<String>,
}

impl App {
    fn from_config(config: &Config) -> Self {
        let input_device_list = enumerate_pipewire_sources();
        let output_device_list = enumerate_pipewire_sinks();

        Self {
            selected: 0,
            editing: false,
            input_device: config.input_device.clone().unwrap_or_default(),
            server_url: config.server.url.clone(),
            blip_device: config.blip_device.clone().unwrap_or_default(),
            blips_enabled: config.blips_enabled,
            input_device_list,
            output_device_list,
            dropdown_open: false,
            dropdown_state: ListState::default(),
            dropdown_items: Vec::new(),
            status: None,
        }
    }

    fn is_picker_field(&self) -> bool {
        self.selected == FIELD_INPUT_DEVICE || self.selected == FIELD_BLIP_DEVICE
    }

    fn is_toggle_field(&self) -> bool {
        self.selected == FIELD_BLIPS_ENABLED
    }

    fn is_text_field(&self) -> bool {
        self.selected == FIELD_SERVER_URL
    }

    fn field_value_mut(&mut self) -> &mut String {
        match self.selected {
            FIELD_INPUT_DEVICE => &mut self.input_device,
            FIELD_SERVER_URL => &mut self.server_url,
            FIELD_BLIP_DEVICE => &mut self.blip_device,
            _ => unreachable!(),
        }
    }

    fn open_dropdown(&mut self) {
        let items = match self.selected {
            FIELD_INPUT_DEVICE => self.input_device_list.clone(),
            FIELD_BLIP_DEVICE => {
                let mut items = vec!["(none)".to_string()];
                items.extend(self.output_device_list.clone());
                items
            }
            _ => return,
        };
        if items.is_empty() {
            return;
        }
        self.dropdown_items = items;
        self.dropdown_state = ListState::default();
        self.dropdown_state.select(Some(0));
        self.dropdown_open = true;
    }

    fn select_dropdown_item(&mut self) {
        if let Some(idx) = self.dropdown_state.selected()
            && let Some(item) = self.dropdown_items.get(idx)
        {
            let value = if item == "(none)" {
                String::new()
            } else {
                item.clone()
            };
            match self.selected {
                FIELD_INPUT_DEVICE => self.input_device = value,
                FIELD_BLIP_DEVICE => self.blip_device = value,
                _ => {}
            }
        }
        self.dropdown_open = false;
    }

    fn save(&mut self) {
        let config = Config {
            server: crate::config::ServerConfig {
                url: self.server_url.clone(),
            },
            blip_device: if self.blip_device.is_empty() {
                None
            } else {
                Some(self.blip_device.clone())
            },
            blips_enabled: self.blips_enabled,
            input_device: if self.input_device.is_empty() {
                None
            } else {
                Some(self.input_device.clone())
            },
        };
        config.save();

        // Send SIGHUP to running daemon
        if let Ok(output) = std::process::Command::new("pidof")
            .arg("just-talk")
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            let my_pid = std::process::id();
            for tok in text.split_whitespace() {
                if let Ok(pid) = tok.parse::<u32>()
                    && pid != my_pid
                {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGHUP);
                    }
                }
            }
        }

        self.status = Some("Saved!".into());
    }
}

pub fn run_config_tui() -> Result<()> {
    // Open /dev/tty FIRST — this is our dedicated channel to the terminal.
    // Then redirect stdout/stderr to /dev/null so that any library noise
    // (ALSA, fontconfig, etc.) written to fd 1 or fd 2 goes nowhere.
    // The TUI renders exclusively through the /dev/tty fd.
    let mut tty = File::options().read(true).write(true).open("/dev/tty")?;
    unsafe {
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
        if devnull >= 0 {
            libc::dup2(devnull, 1);
            libc::dup2(devnull, 2);
            libc::close(devnull);
        }
    }

    let config = Config::load();
    let mut app = App::from_config(&config);

    // Panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        // LeaveAlternateScreen can't easily be sent to tty from here,
        // but disable_raw_mode uses /dev/tty internally so terminal resets.
        original_hook(info);
    }));

    enable_raw_mode()?;
    execute!(tty, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(tty);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Clear status on any keypress
            app.status = None;

            if app.dropdown_open {
                match key.code {
                    KeyCode::Up => {
                        let i = app.dropdown_state.selected().unwrap_or(0);
                        app.dropdown_state.select(Some(i.saturating_sub(1)));
                    }
                    KeyCode::Down => {
                        let i = app.dropdown_state.selected().unwrap_or(0);
                        let max = app.dropdown_items.len().saturating_sub(1);
                        app.dropdown_state.select(Some((i + 1).min(max)));
                    }
                    KeyCode::Enter => app.select_dropdown_item(),
                    KeyCode::Esc => app.dropdown_open = false,
                    _ => {}
                }
                continue;
            }

            if app.editing {
                match key.code {
                    KeyCode::Esc => app.editing = false,
                    KeyCode::Enter => app.editing = false,
                    KeyCode::Backspace => {
                        app.field_value_mut().pop();
                    }
                    KeyCode::Char(c) => {
                        app.field_value_mut().push(c);
                    }
                    _ => {}
                }
                continue;
            }

            // Normal navigation
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Up | KeyCode::Char('k') => {
                    app.selected = app.selected.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.selected = (app.selected + 1).min(FIELD_COUNT - 1);
                }
                KeyCode::Enter => {
                    if app.is_picker_field() {
                        app.open_dropdown();
                    } else if app.is_toggle_field() {
                        app.blips_enabled = !app.blips_enabled;
                    } else if app.is_text_field() {
                        app.editing = true;
                    }
                }
                KeyCode::Char('s') => app.save(),
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // Full-screen block — fills every cell so nothing from underlying
    // terminal output can bleed through ratatui's diff-based rendering.
    let block = Block::default()
        .title(" just-talk config ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::vertical(
        std::iter::repeat_n(Constraint::Length(3), FIELD_COUNT)
            .chain(std::iter::once(Constraint::Min(1)))
            .collect::<Vec<_>>(),
    )
    .split(inner);

    for i in 0..FIELD_COUNT {
        let is_selected = i == app.selected;
        let label = FIELD_LABELS[i];
        let value = app.field_value_for(i);

        let style = if is_selected {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let cursor_indicator = if is_selected { "> " } else { "  " };

        let editing_hint = if is_selected && app.editing {
            " (editing)"
        } else if is_selected && app.is_picker_field() {
            " [Enter: pick]"
        } else if is_selected && app.is_toggle_field() {
            " [Enter: toggle]"
        } else if is_selected && app.is_text_field() {
            " [Enter: edit]"
        } else {
            ""
        };

        let value_style = if is_selected && app.editing {
            Style::default().fg(Color::Green).add_modifier(Modifier::UNDERLINED)
        } else if value.is_empty() || value == "(default)" {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };

        let display_value = if value.is_empty() && !app.editing {
            "(default)".to_string()
        } else if app.editing && is_selected {
            format!("{}_", value)
        } else {
            value
        };

        let line = Line::from(vec![
            Span::styled(cursor_indicator, style),
            Span::styled(format!("{:<15}", label), style),
            Span::styled(display_value, value_style),
            Span::styled(editing_hint, Style::default().fg(Color::DarkGray)),
        ]);

        let paragraph = Paragraph::new(line)
            .block(Block::default().borders(Borders::NONE));
        f.render_widget(paragraph, chunks[i]);
    }

    // Footer with keybindings and status
    let status_text = app.status.as_deref().unwrap_or("");
    let footer_line = if !status_text.is_empty() {
        Line::from(vec![
            Span::styled(status_text, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled("q: quit  s: save  Enter: edit/toggle", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(Span::styled(
            "q: quit  s: save  Enter: edit/toggle  Esc: cancel",
            Style::default().fg(Color::DarkGray),
        ))
    };
    let footer = Paragraph::new(footer_line);
    f.render_widget(footer, chunks[FIELD_COUNT]);

    // Dropdown overlay
    if app.dropdown_open {
        let dropdown_area = dropdown_rect(chunks[app.selected], f.area(), app.dropdown_items.len());
        f.render_widget(Clear, dropdown_area);

        let items: Vec<ListItem> = app
            .dropdown_items
            .iter()
            .map(|s| ListItem::new(s.as_str()))
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" Select "),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        f.render_stateful_widget(list, dropdown_area, &mut app.dropdown_state);
    }
}

impl App {
    fn field_value_for(&self, idx: usize) -> String {
        match idx {
            FIELD_INPUT_DEVICE => self.input_device.clone(),
            FIELD_SERVER_URL => self.server_url.clone(),
            FIELD_BLIP_DEVICE => self.blip_device.clone(),
            FIELD_BLIPS_ENABLED => {
                if self.blips_enabled { "ON".into() } else { "OFF".into() }
            }
            _ => String::new(),
        }
    }
}

fn dropdown_rect(field_area: Rect, screen: Rect, item_count: usize) -> Rect {
    let max_items = 10usize;
    let visible = item_count.min(max_items) as u16;
    let h = visible + 2; // borders
    let w = field_area.width.min(50);
    let x = field_area.x + 17; // offset past label
    let y = field_area.y + 1;
    // Clamp to screen
    let x = x.min(screen.x + screen.width.saturating_sub(w));
    let y = y.min(screen.y + screen.height.saturating_sub(h));
    Rect::new(x, y, w, h)
}

/// Enumerate PipeWire input sources (microphones) via pactl.
/// Filters out .monitor sources which are loopbacks of output sinks.
fn enumerate_pipewire_sources() -> Vec<String> {
    let output = match std::process::Command::new("pactl")
        .args(["list", "sources", "short"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            cols.next(); // skip ID
            let name = cols.next()?;
            // Skip monitor sources (loopbacks of output sinks)
            if name.ends_with(".monitor") {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

/// Enumerate PipeWire output sinks via pactl.
fn enumerate_pipewire_sinks() -> Vec<String> {
    let output = match std::process::Command::new("pactl")
        .args(["list", "sinks", "short"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            cols.next(); // skip ID
            cols.next().map(|name| name.to_string())
        })
        .collect()
}
