use anyhow::Result;
use evdev::{Device, InputEventKind, Key};
use inotify::{Inotify, WatchMask};
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
fn find_keyboards() -> Vec<PathBuf> {
    let mut keyboards = Vec::new();
    let entries = match std::fs::read_dir("/dev/input") {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "failed to read /dev/input");
            return keyboards;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if !name.starts_with("event") {
            continue;
        }
        if let Ok(device) = Device::open(&path)
            && device
                .supported_keys()
                .is_some_and(|keys| keys.contains(Key::KEY_RIGHTALT))
        {
            info!(path = %path.display(), name = ?device.name(), "found keyboard");
            keyboards.push(path);
        }
    }
    keyboards
}

/// Returns true if the error indicates the device was disconnected.
fn is_device_gone(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENODEV) | Some(libc::ENXIO) | Some(libc::ENOTTY)
    ) || e.kind() == ErrorKind::NotFound
}

/// Spawn a listener thread for a single keyboard device.
/// The thread exits when the device disconnects or the channel closes.
fn spawn_device_listener(
    path: PathBuf,
    tx: mpsc::UnboundedSender<KeyEvent>,
    tracked: Arc<Mutex<HashSet<PathBuf>>>,
) {
    std::thread::spawn(move || {
        let mut device = match Device::open(&path) {
            Ok(d) => d,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "failed to open device");
                return;
            }
        };
        info!(path = %path.display(), name = ?device.name(), "listening for AltGr on device");

        let mut last_press_time: Option<Instant> = None;
        let mut last_release_time: Option<Instant> = None;
        let mut suppress_release = false;
        let mut ctrl_held = false;

        loop {
            match device.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        if let InputEventKind::Key(Key::KEY_LEFTCTRL | Key::KEY_RIGHTCTRL) =
                            ev.kind()
                        {
                            match ev.value() {
                                1 => ctrl_held = true,
                                0 => ctrl_held = false,
                                _ => {}
                            }
                            continue;
                        }

                        if let InputEventKind::Key(Key::KEY_RIGHTALT) = ev.kind() {
                            if ctrl_held && ev.value() == 1 {
                                debug!(?ev, "Ctrl+AltGr detected, opening config");
                                suppress_release = true;
                                let _ = tx.send(KeyEvent::ConfigOpen);
                                continue;
                            }

                            let event = match ev.value() {
                                1 => {
                                    let now = Instant::now();
                                    let is_double_tap =
                                        match (last_press_time, last_release_time) {
                                            (Some(prev_press), Some(prev_release)) => {
                                                let prev_tap_duration =
                                                    prev_release.duration_since(prev_press);
                                                let gap = now.duration_since(prev_release);
                                                prev_tap_duration.as_millis() < 300
                                                    && gap.as_millis() < 400
                                            }
                                            _ => false,
                                        };
                                    last_press_time = Some(now);
                                    if is_double_tap {
                                        suppress_release = true;
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
                                _ => None,
                            };
                            if let Some(event) = event {
                                debug!(?event, "key event");
                                if tx.send(event).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    if is_device_gone(&e) {
                        info!(path = %path.display(), "keyboard disconnected");
                        tracked.lock().unwrap().remove(&path);
                        return;
                    }
                    warn!(path = %path.display(), error = %e, "error reading events, retrying");
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    });
}

/// Spawn a watcher thread that monitors /dev/input/ for new devices via inotify.
/// When a new keyboard appears, it spawns a listener thread for it.
fn spawn_hotplug_watcher(tx: mpsc::UnboundedSender<KeyEvent>, tracked: Arc<Mutex<HashSet<PathBuf>>>) {
    std::thread::spawn(move || {
        let mut inotify = match Inotify::init() {
            Ok(i) => i,
            Err(e) => {
                warn!(error = %e, "failed to init inotify, hotplug detection disabled");
                return;
            }
        };

        if let Err(e) = inotify.watches().add("/dev/input", WatchMask::CREATE) {
            warn!(error = %e, "failed to watch /dev/input, hotplug detection disabled");
            return;
        }

        info!("watching /dev/input for new keyboards");

        let mut buffer = [0; 1024];
        loop {
            let events = match inotify.read_events_blocking(&mut buffer) {
                Ok(events) => events,
                Err(e) => {
                    warn!(error = %e, "inotify read error");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }
            };

            for event in events {
                let name = match event.name {
                    Some(n) => n,
                    None => continue,
                };
                let name_str = name.to_string_lossy();
                if !name_str.starts_with("event") {
                    continue;
                }

                let path = PathBuf::from("/dev/input").join(name);

                // Small delay to let the device fully initialize
                std::thread::sleep(std::time::Duration::from_millis(200));

                if let Ok(device) = Device::open(&path)
                    && device
                        .supported_keys()
                        .is_some_and(|keys| keys.contains(Key::KEY_RIGHTALT))
                {
                    let mut set = tracked.lock().unwrap();
                    if set.insert(path.clone()) {
                        info!(path = %path.display(), name = ?device.name(), "new keyboard detected");
                        drop(device); // close before spawning listener which re-opens
                        spawn_device_listener(path, tx.clone(), tracked.clone());
                    }
                }
            }
        }
    });
}

/// Spawn keyboard listeners and a hotplug watcher.
/// Returns immediately.
pub fn spawn_listener(tx: mpsc::UnboundedSender<KeyEvent>) -> Result<()> {
    let keyboards = find_keyboards();
    if keyboards.is_empty() {
        warn!(
            "no keyboard devices found at startup - will watch for hotplug. \
             Are you in the 'input' group? Try: sudo usermod -aG input $USER"
        );
    }

    let tracked: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));

    for path in keyboards {
        tracked.lock().unwrap().insert(path.clone());
        spawn_device_listener(path, tx.clone(), tracked.clone());
    }

    spawn_hotplug_watcher(tx, tracked);

    Ok(())
}
