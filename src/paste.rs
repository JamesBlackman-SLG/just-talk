use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

/// Paste text at the current cursor position.
///
/// Detects whether the focused window is XWayland or native Wayland via
/// `hyprctl activewindow` and chooses the appropriate method:
/// - Native Wayland: `wtype -- text` (virtual keyboard protocol)
/// - XWayland (Electron, etc.): `xdotool type` (X11 protocol)
///
/// Also copies text to clipboard via `wl-copy` as a backup.
pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        warn!("empty text, nothing to paste");
        return Ok(());
    }

    // Wait for focus to settle after overlay closes
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Always copy to clipboard as a backup
    let _ = Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .status();

    if is_xwayland_focused() {
        info!(len = text.len(), "XWayland window detected, using xdotool");
        xdotool_paste(text)
    } else {
        info!(len = text.len(), "native Wayland window, using wtype");
        wtype_paste(text)
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

/// Paste via xdotool for XWayland windows (Electron, Chromium, etc.).
fn xdotool_paste(text: &str) -> Result<()> {
    let status = Command::new("xdotool")
        .arg("type")
        .arg("--clearmodifiers")
        .arg("--")
        .arg(text)
        .status()
        .context("failed to run xdotool - is it installed? (pacman -S xdotool)")?;

    if !status.success() {
        anyhow::bail!("xdotool exited with status: {status}");
    }

    info!("xdotool paste complete");
    Ok(())
}

/// Paste via direct wtype character simulation for native Wayland windows.
fn wtype_paste(text: &str) -> Result<()> {
    let status = Command::new("wtype")
        .arg("--")
        .arg(text)
        .status()
        .context("failed to run wtype - is it installed? (pacman -S wtype)")?;

    if !status.success() {
        anyhow::bail!("wtype exited with status: {status}");
    }

    info!("wtype paste complete");
    Ok(())
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
