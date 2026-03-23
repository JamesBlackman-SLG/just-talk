use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

use crate::blip;

/// Paste text using clipboard (wl-copy + paste keystroke).
///
/// Virtual keyboard character simulation (wtype/xdotool type) creates and destroys
/// a keyboard device per invocation, causing race conditions — dropped characters,
/// missing spaces, reordered text. Clipboard paste is atomic and reliable.
///
/// Detects terminal vs GUI apps to use the correct paste shortcut:
/// - Terminals: Ctrl+Shift+V
/// - GUI apps: Ctrl+V
/// - XWayland: xdotool equivalents
pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        warn!("empty text, nothing to paste");
        return Ok(());
    }

    // Sanitize: replace newlines with spaces — ASR models sometimes produce them,
    // and wtype/xdotool would press Return, which sends messages in chat apps
    let text = text.replace('\n', " ").replace('\r', " ");

    // Collapse multiple spaces that may result from newline replacement
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");

    // Ensure text ends with a space so consecutive transcriptions don't run together
    if !text.ends_with(' ') {
        text.push(' ');
    }
    let text = text.as_str();

    // Wait for focus to settle after overlay closes
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Save current clipboard so we can restore it after pasting
    let saved_clipboard = Command::new("wl-paste")
        .arg("--no-newline")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout);

    // Copy to PRIMARY selection (middle-click paste) as backup
    let _ = Command::new("wl-copy")
        .arg("--primary")
        .arg("--")
        .arg(text)
        .status();

    let window = get_focused_window();
    info!(
        len = text.len(),
        xwayland = window.xwayland,
        terminal = window.terminal,
        class = %window.class,
        "pasting via clipboard"
    );

    // Blips are opt-in: only play when blip_device is configured and blips are enabled
    let config = crate::config::Config::load();
    if config.blip_device.is_none() || !config.blips_enabled {
        let result = clipboard_paste(text, &window);
        restore_clipboard(saved_clipboard);
        return result;
    }
    let player = match blip::BlipPlayer::new() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "blip audio unavailable, pasting instantly");
            let result = clipboard_paste(text, &window);
            restore_clipboard(saved_clipboard);
            return result;
        }
    };

    // Split into words, keeping trailing whitespace attached to each word
    for chunk in word_chunks(text) {
        if let Err(e) = clipboard_paste(chunk, &window) {
            warn!(error = %e, "clipboard paste failed, falling back to bulk");
            let result = clipboard_paste(text, &window);
            restore_clipboard(saved_clipboard);
            return result;
        }

        // Play blips for this chunk's length, then wait — blips ARE the delay
        player.blip_word(chunk.chars().count());
        player.wait_for_drain();
    }

    restore_clipboard(saved_clipboard);
    info!("paste complete");
    Ok(())
}

/// Split text into chunks: each word plus its trailing whitespace.
/// "Hello, world!\nHow are you?" → ["Hello, ", "world!\n", "How ", "are ", "you?"]
fn word_chunks(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();

    while let Some(&(i, ch)) = chars.peek() {
        if start == i && ch.is_whitespace() {
            // Leading whitespace — skip into the word
            chars.next();
            continue;
        }

        chars.next();

        // After a whitespace char, if the next char is non-whitespace (or end),
        // that's a chunk boundary
        let at_end = chars.peek().is_none();
        let next_is_word = chars.peek().is_some_and(|&(_, next)| !next.is_whitespace());

        if ch.is_whitespace() && (next_is_word || at_end) {
            let end = chars.peek().map_or(text.len(), |&(idx, _)| idx);
            chunks.push(&text[start..end]);
            start = end;
        } else if at_end {
            chunks.push(&text[start..text.len()]);
        }
    }

    // Handle text that's entirely whitespace or a single chunk
    if chunks.is_empty() && !text.is_empty() {
        chunks.push(text);
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_word_chunks_basic() {
        assert_eq!(word_chunks("hello world"), vec!["hello ", "world"]);
    }

    #[test]
    fn test_word_chunks_with_punctuation() {
        assert_eq!(
            word_chunks("Hello, world! How are you?"),
            vec!["Hello, ", "world! ", "How ", "are ", "you?"]
        );
    }

    #[test]
    fn test_word_chunks_single_word() {
        assert_eq!(word_chunks("hello"), vec!["hello"]);
    }

    #[test]
    fn test_word_chunks_with_leading_whitespace() {
        assert_eq!(word_chunks("  hello"), vec!["  ", "hello"]);
    }

    #[test]
    fn test_word_chunks_empty() {
        assert_eq!(word_chunks(""), Vec::<&str>::new());
    }

    #[test]
    fn test_word_chunks_only_whitespace() {
        assert_eq!(word_chunks("   "), vec!["   "]);
    }
}

struct FocusedWindow {
    xwayland: bool,
    terminal: bool,
    class: String,
}

/// Copy text to clipboard and simulate the paste keystroke.
/// Terminals use Ctrl+Shift+V, GUI apps use Ctrl+V.
fn clipboard_paste(text: &str, window: &FocusedWindow) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    // Copy to clipboard
    let status = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .status()
        .context("wl-copy failed")?;
    if !status.success() {
        anyhow::bail!("wl-copy exited with status: {status}");
    }

    // Let the clipboard propagate through the compositor
    std::thread::sleep(std::time::Duration::from_millis(20));

    if window.xwayland {
        if window.terminal {
            run("xdotool", &["key", "--clearmodifiers", "ctrl+shift+v"])
        } else {
            run("xdotool", &["key", "--clearmodifiers", "ctrl+v"])
        }
    } else if window.terminal {
        // Terminals: Ctrl+Shift+V
        run("wtype", &["-M", "ctrl", "-M", "shift", "-k", "v"])
    } else {
        // GUI apps: Ctrl+V
        run("wtype", &["-M", "ctrl", "-k", "v"])
    }
}

/// Restore the user's clipboard after pasting.
fn restore_clipboard(saved: Option<Vec<u8>>) {
    if let Some(data) = saved {
        let _ = Command::new("wl-copy")
            .arg("--")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    let _ = stdin.write_all(&data);
                }
                child.wait()
            });
    }
}

/// Run a command, check exit status.
fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .with_context(|| format!("{cmd} failed to execute"))?;
    if !status.success() {
        anyhow::bail!("{cmd} exited with status: {status}");
    }
    Ok(())
}

/// Query Hyprland for the focused window's properties.
/// Returns xwayland status, whether it's a terminal, and the window class.
fn get_focused_window() -> FocusedWindow {
    let output = match Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "failed to run hyprctl, assuming native Wayland GUI");
            return FocusedWindow { xwayland: false, terminal: false, class: "unknown".into() };
        }
    };

    let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to parse hyprctl output, assuming native Wayland GUI");
            return FocusedWindow { xwayland: false, terminal: false, class: "unknown".into() };
        }
    };

    let xwayland = json.get("xwayland").and_then(|v| v.as_bool()).unwrap_or(false);
    let class = json.get("class").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

    // Check Hyprland tags for "terminal" — set by window rules
    let terminal = json.get("tags")
        .and_then(|v| v.as_array())
        .is_some_and(|tags| {
            tags.iter().any(|t| t.as_str().is_some_and(|s| s.starts_with("terminal")))
        });

    if xwayland {
        info!(class = %class, terminal, "focused window is XWayland");
    }

    FocusedWindow { xwayland, terminal, class }
}

/// Check that required tools are available.
pub fn check_wtype() -> Result<()> {
    Command::new("wtype")
        .arg("--help")
        .output()
        .context("wtype not found - install with: pacman -S wtype")?;
    // wl-copy/wl-paste for clipboard-based pasting
    Command::new("wl-copy")
        .args(["--primary", "--help"])
        .output()
        .context("wl-copy not found - install with: pacman -S wl-clipboard")?;
    Command::new("wl-paste")
        .arg("--help")
        .output()
        .context("wl-paste not found - install with: pacman -S wl-clipboard")?;
    Command::new("xdotool")
        .arg("--version")
        .output()
        .context("xdotool not found - install with: pacman -S xdotool")?;
    Ok(())
}
