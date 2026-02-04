use anyhow::{Context, Result};
use std::process::Command;
use tracing::{info, warn};

/// Type text at the current cursor position using wtype.
pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        warn!("empty text, nothing to paste");
        return Ok(());
    }

    info!(len = text.len(), "pasting text via wtype");

    // Small delay to let focus settle back after overlay closes
    std::thread::sleep(std::time::Duration::from_millis(50));

    let status = Command::new("wtype")
        .arg("--")
        .arg(text)
        .status()
        .context("failed to run wtype - is it installed?")?;

    if !status.success() {
        anyhow::bail!("wtype exited with status: {status}");
    }

    info!("paste complete");
    Ok(())
}

/// Check that wtype is available.
pub fn check_wtype() -> Result<()> {
    Command::new("wtype")
        .arg("--help")
        .output()
        .context("wtype not found - install with: pacman -S wtype")?;
    Ok(())
}
