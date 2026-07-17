use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::thread;

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
use ashpd::desktop::PersistMode;
use futures_util::StreamExt;

use crate::PasteChord;

pub fn start_shortcut_listener() {
    if std::env::var_os("WAYLAND_DISPLAY").is_none()
        || std::env::var_os("GIGATYPE_NO_PORTAL_SHORTCUTS").is_some()
        || (std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktop| desktop.to_ascii_lowercase().contains("kde"))
            && std::env::var_os("GIGATYPE_FORCE_PORTAL_SHORTCUTS").is_none())
    {
        return;
    }
    thread::spawn(|| {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("GigaType portal shortcuts: {error}");
                return;
            }
        };
        if let Err(error) = runtime.block_on(shortcut_loop()) {
            eprintln!("GigaType portal shortcuts unavailable: {error}");
        }
    });
}

async fn shortcut_loop() -> Result<(), String> {
    let portal = GlobalShortcuts::new()
        .await
        .map_err(|error| error.to_string())?;
    let session = portal
        .create_session()
        .await
        .map_err(|error| error.to_string())?;
    let shortcuts = [
        NewShortcut::new("toggle", "Start or stop Russian dictation").preferred_trigger(Some("F9")),
        NewShortcut::new("cancel", "Cancel Russian dictation").preferred_trigger(Some("<Shift>F9")),
    ];
    portal
        .bind_shortcuts(&session, &shortcuts, None)
        .await
        .map_err(|error| error.to_string())?
        .response()
        .map_err(|error| error.to_string())?;
    let mut activated = portal
        .receive_activated()
        .await
        .map_err(|error| error.to_string())?;
    while let Some(event) = activated.next().await {
        let command = match event.shortcut_id() {
            "toggle" => "toggle",
            "cancel" => "cancel",
            _ => continue,
        };
        let _ = crate::send_daemon(command);
    }
    Ok(())
}

fn token_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/gigatype/portal-input-token")
}

async fn use_input_session(chord: Option<PasteChord>) -> Result<(), String> {
    let portal = RemoteDesktop::new()
        .await
        .map_err(|error| error.to_string())?;
    let session = portal
        .create_session()
        .await
        .map_err(|error| error.to_string())?;
    let token = fs::read_to_string(token_path()).ok();
    portal
        .select_devices(
            &session,
            DeviceType::Keyboard.into(),
            token.as_deref(),
            PersistMode::ExplicitlyRevoked,
        )
        .await
        .map_err(|error| error.to_string())?
        .response()
        .map_err(|error| error.to_string())?;
    let selected = portal
        .start(&session, None)
        .await
        .map_err(|error| error.to_string())?
        .response()
        .map_err(|error| error.to_string())?;
    if !selected.devices().contains(DeviceType::Keyboard) {
        return Err("keyboard access was not granted".into());
    }
    if let Some(token) = selected.restore_token() {
        let path = token_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, token).map_err(|error| error.to_string())?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    if let Some(chord) = chord {
        for &(keycode, value) in chord.events() {
            let state = if value == 0 {
                KeyState::Released
            } else {
                KeyState::Pressed
            };
            portal
                .notify_keyboard_keycode(&session, keycode as i32, state)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

pub fn authorize_input() -> Result<(), String> {
    run_portal(use_input_session(None))
}

pub fn paste(chord: PasteChord, probe_only: bool) -> Result<(), String> {
    if probe_only && !token_path().is_file() {
        return Err("portal input permission has not been granted".into());
    }
    run_portal(use_input_session((!probe_only).then_some(chord)))
}

fn run_portal<F>(future: F) -> Result<(), String>
where
    F: std::future::Future<Output = Result<(), String>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(tokio::time::timeout(
            std::time::Duration::from_secs(45),
            future,
        ))
        .map_err(|_| "desktop portal authorization timed out".to_string())?
}
