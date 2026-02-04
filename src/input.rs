use anyhow::Result;
use evdev::{Device, InputEventKind, Key};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    AltGrPressed,
    AltGrReleased,
}

/// Find all keyboard devices in /dev/input/
fn find_keyboards() -> Result<Vec<PathBuf>> {
    let mut keyboards = Vec::new();
    for entry in std::fs::read_dir("/dev/input")? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        if let Ok(device) = Device::open(&path) {
            if device.supported_keys().is_some_and(|keys| keys.contains(Key::KEY_RIGHTALT)) {
                info!(path = %path.display(), name = ?device.name(), "found keyboard");
                keyboards.push(path);
            }
        }
    }
    if keyboards.is_empty() {
        anyhow::bail!(
            "no keyboard devices found - are you in the 'input' group? \
             Try: sudo usermod -aG input $USER"
        );
    }
    Ok(keyboards)
}

/// Spawn a blocking thread that reads evdev events and sends AltGr press/release
/// over a channel. Returns immediately.
pub fn spawn_listener(tx: mpsc::UnboundedSender<KeyEvent>) -> Result<()> {
    let keyboards = find_keyboards()?;

    for path in keyboards {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut device = match Device::open(&path) {
                Ok(d) => d,
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to open device");
                    return;
                }
            };
            info!(path = %path.display(), "listening for AltGr on device");
            loop {
                match device.fetch_events() {
                    Ok(events) => {
                        for ev in events {
                            if let InputEventKind::Key(Key::KEY_RIGHTALT) = ev.kind() {
                                let event = match ev.value() {
                                    1 => Some(KeyEvent::AltGrPressed),
                                    0 => Some(KeyEvent::AltGrReleased),
                                    _ => None, // repeat events (value=2) ignored
                                };
                                if let Some(event) = event {
                                    debug!(?event, "key event");
                                    if tx.send(event).is_err() {
                                        return; // receiver dropped
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "error reading events, retrying");
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
            }
        });
    }
    Ok(())
}
