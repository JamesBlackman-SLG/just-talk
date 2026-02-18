use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

use crate::blip;

/// Paste text word-by-word with blip sounds — like Mother from Alien.
///
/// Each word is pasted instantly, then blips play for the word's character count.
/// The blips serve as the natural pacer between words.
pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        warn!("empty text, nothing to paste");
        return Ok(());
    }

    // Wait for focus to settle after overlay closes
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Always copy to clipboard as a backup
    let _ = Command::new("wl-copy").arg("--").arg(text).status();

    let xwayland = is_xwayland_focused();
    info!(len = text.len(), xwayland, "pasting word-by-word with blips");

    // Blips are opt-in: only play when blip_device is configured and blips are enabled
    let config = crate::config::Config::load();
    if config.blip_device.is_none() || !config.blips_enabled {
        return bulk_paste(text, xwayland);
    }
    let player = match blip::BlipPlayer::new() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "blip audio unavailable, pasting instantly");
            return bulk_paste(text, xwayland);
        }
    };

    // Split into words, keeping trailing whitespace attached to each word
    for chunk in word_chunks(text) {
        // Paste the whole chunk at once
        if let Err(e) = paste_chunk(chunk, xwayland) {
            warn!(error = %e, "paste failed, falling back to bulk");
            return bulk_paste(text, xwayland);
        }

        // Play blips for this chunk's length, then wait — blips ARE the delay
        player.blip_word(chunk.chars().count());
        player.wait_for_drain();
    }

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
        let next_is_word = chars
            .peek()
            .is_some_and(|&(_, next)| !next.is_whitespace());

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

/// Paste a chunk that may contain mixed text and whitespace.
fn paste_chunk(chunk: &str, xwayland: bool) -> Result<()> {
    // Split on newlines — wtype needs -k Return for those
    let parts: Vec<&str> = chunk.split('\n').collect();
    for (i, part) in parts.iter().enumerate() {
        if !part.is_empty() {
            if xwayland {
                run("xdotool", &["type", "--clearmodifiers", "--", part])?;
            } else {
                run("wtype", &["--", part])?;
            }
        }
        // Type newline between parts (not after the last one)
        if i < parts.len() - 1 {
            if xwayland {
                run("xdotool", &["key", "Return"])?;
            } else {
                run("wtype", &["-k", "Return"])?;
            }
        }
    }
    Ok(())
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

/// Bulk-paste fallback (no blips, no delay).
fn bulk_paste(text: &str, xwayland: bool) -> Result<()> {
    if xwayland {
        run("xdotool", &["type", "--clearmodifiers", "--", text])
    } else {
        run("wtype", &["--", text])
    }
}

/// Check if the currently focused window is an XWayland client.
fn is_xwayland_focused() -> bool {
    let output = match Command::new("hyprctl")
        .args(["activewindow", "-j"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "failed to run hyprctl, assuming native Wayland");
            return false;
        }
    };

    let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "failed to parse hyprctl output, assuming native Wayland");
            return false;
        }
    };

    let xwayland = json.get("xwayland").and_then(|v| v.as_bool()).unwrap_or(false);
    if xwayland {
        let class = json.get("class").and_then(|v| v.as_str()).unwrap_or("unknown");
        info!(class, "focused window is XWayland");
    }
    xwayland
}

/// Check that required tools are available.
pub fn check_wtype() -> Result<()> {
    Command::new("wtype")
        .arg("--help")
        .output()
        .context("wtype not found - install with: pacman -S wtype")?;
    Command::new("wl-copy")
        .arg("--help")
        .output()
        .context("wl-copy not found - install with: pacman -S wl-clipboard")?;
    Command::new("xdotool")
        .arg("--version")
        .output()
        .context("xdotool not found - install with: pacman -S xdotool")?;
    Ok(())
}
