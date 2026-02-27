use crate::config::Config;
use crate::input::KeyEvent;
use midir::{Ignore, MidiInput};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

const DEFAULT_TARGET_CONTROLLER: u8 = 85;
const PORT_NAME_MATCH: &str = "FS-1-WL";

/// Spawn a thread that listens for MIDI foot pedal events.
/// Sends the same KeyEvent types as the keyboard listener.
/// If no MIDI device is found, logs a message and returns without error.
pub fn spawn_listener(tx: mpsc::UnboundedSender<KeyEvent>) {
    std::thread::spawn(move || {
        if let Err(e) = midi_listen(tx) {
            warn!(error = %e, "MIDI listener error");
        }
    });
}

fn midi_listen(tx: mpsc::UnboundedSender<KeyEvent>) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    let target_controller = config.midi_cc.unwrap_or(DEFAULT_TARGET_CONTROLLER);

    let mut midi_in = MidiInput::new("justspeak_midi")?;
    midi_in.ignore(Ignore::None);

    let in_ports = midi_in.ports();
    let mut selected_port = None;

    for port in &in_ports {
        let name = midi_in.port_name(port)?;
        if name.contains(PORT_NAME_MATCH) {
            selected_port = Some(port.clone());
            info!(name = %name, "MIDI foot pedal connected");
            break;
        }
    }

    let Some(port) = selected_port else {
        info!("no MIDI foot pedal ({PORT_NAME_MATCH}) found - keyboard-only mode");
        return Ok(());
    };

    let _conn = midi_in.connect(
        &port,
        "justspeak_midi_read",
        move |_stamp, message, _| {
            if message.len() >= 3 && (message[0] & 0xF0) == 0xB0 {
                let controller = message[1];
                let value = message[2];

                if controller == target_controller {
                    if value == 127 {
                        debug!("MIDI foot pedal pressed");
                        let _ = tx.send(KeyEvent::AltGrPressed);
                    } else if value == 0 {
                        debug!("MIDI foot pedal released");
                        let _ = tx.send(KeyEvent::AltGrReleased);
                    }
                }
            }
        },
        (),
    )?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
