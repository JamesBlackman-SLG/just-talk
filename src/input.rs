use anyhow::Result;
use evdev::{Device, InputEventKind, Key};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    AltGrPressed,
    AltGrReleased,
    DoubleTap,
    ConfigOpen,
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
        if let Ok(device) = Device::open(&path)
            && device.supported_keys().is_some_and(|keys| keys.contains(Key::KEY_RIGHTALT)) {
                info!(path = %path.display(), name = ?device.name(), "found keyboard");
                keyboards.push(path);
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

            let mut last_press_time: Option<Instant> = None;
            let mut last_release_time: Option<Instant> = None;
            let mut suppress_release = false;
            let mut ctrl_held = false;

            loop {
                match device.fetch_events() {
                    Ok(events) => {
                        for ev in events {
                            // Track Ctrl key state (both left and right)
                            if let InputEventKind::Key(Key::KEY_LEFTCTRL | Key::KEY_RIGHTCTRL) =
                                ev.kind()
                            {
                                match ev.value() {
                                    1 => ctrl_held = true,
                                    0 => ctrl_held = false,
                                    _ => {} // repeat
                                }
                                continue;
                            }

                            if let InputEventKind::Key(Key::KEY_RIGHTALT) = ev.kind() {
                                // Ctrl+AltGr → config screen
                                if ctrl_held && ev.value() == 1 {
                                    debug!(?ev, "Ctrl+AltGr detected, opening config");
                                    suppress_release = true;
                                    let _ = tx.send(KeyEvent::ConfigOpen);
                                    continue;
                                }

                                let event = match ev.value() {
                                    1 => {
                                        // Press: check for double-tap
                                        let now = Instant::now();
                                        let is_double_tap = match (last_press_time, last_release_time) {
                                            (Some(prev_press), Some(prev_release)) => {
                                                let prev_tap_duration = prev_release.duration_since(prev_press);
                                                let gap = now.duration_since(prev_release);
                                                prev_tap_duration.as_millis() < 300
                                                    && gap.as_millis() < 400
                                            }
                                            _ => false,
                                        };
                                        last_press_time = Some(now);
                                        if is_double_tap {
                                            suppress_release = true;
                                            // Clear timing so triple-tap doesn't re-trigger
                                            last_press_time = None;
                                            last_release_time = None;
                                            Some(KeyEvent::DoubleTap)
                                        } else {
                                            Some(KeyEvent::AltGrPressed)
                                        }
                                    }
                                    0 => {
                                        if suppress_release {
                                            suppress_release = false;
                                            None
                                        } else {
                                            last_release_time = Some(Instant::now());
                                            Some(KeyEvent::AltGrReleased)
                                        }
                                    }
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
