use std::thread;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

pub struct ShortcutManager {
    _manager: GlobalHotKeyManager,
}

impl ShortcutManager {
    pub fn start() -> Option<Self> {
        if std::env::var_os("GIGATYPE_NO_NATIVE_SHORTCUTS").is_some() {
            return None;
        }
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return None;
        }
        let manager = GlobalHotKeyManager::new().ok()?;
        let toggle = HotKey::new(None, Code::F9);
        let cancel = HotKey::new(Some(Modifiers::SHIFT), Code::F9);
        manager.register(toggle).ok()?;
        manager.register(cancel).ok()?;
        let toggle_id = toggle.id();
        let cancel_id = cancel.id();
        thread::spawn(move || {
            while let Ok(event) = GlobalHotKeyEvent::receiver().recv() {
                if event.state != HotKeyState::Pressed {
                    continue;
                }
                if event.id == toggle_id {
                    let _ = crate::send_daemon("toggle");
                } else if event.id == cancel_id {
                    let _ = crate::send_daemon("cancel");
                }
            }
        });
        Some(Self { _manager: manager })
    }
}
