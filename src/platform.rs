use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub name: &'static str,
    pub audio: &'static str,
    pub shortcuts: &'static str,
    pub insertion: &'static str,
    pub autostart: &'static str,
}

impl Platform {
    pub fn detect() -> Self {
        if let Ok(fake) = env::var("GIGATYPE_FAKE_PLATFORM") {
            return match fake.as_str() {
                "linux-wayland" => Self::linux_wayland(),
                "linux-x11" => Self::linux_x11(),
                "macos" => Self::macos(),
                "bsd" => Self::bsd(),
                _ => Self::generic_unix(),
            };
        }
        #[cfg(target_os = "macos")]
        return Self::macos();
        #[cfg(any(target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
        return Self::bsd();
        #[cfg(target_os = "linux")]
        {
            if env::var_os("WAYLAND_DISPLAY").is_some() {
                return Self::linux_wayland();
            }
            if env::var_os("DISPLAY").is_some() {
                return Self::linux_x11();
            }
        }
        Self::generic_unix()
    }

    fn linux_wayland() -> Self {
        let kde = env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktop| desktop.to_ascii_lowercase().contains("kde"));
        Self {
            name: "linux-wayland",
            audio: "cpal",
            shortcuts: if kde {
                "kde-globalaccel"
            } else {
                "xdg-desktop-portal"
            },
            insertion: if kde {
                "linux-uinput"
            } else {
                "xdg-desktop-portal"
            },
            autostart: "systemd-user",
        }
    }

    fn linux_x11() -> Self {
        Self {
            name: "linux-x11",
            audio: "cpal",
            shortcuts: "x11-grab-key",
            insertion: "x11-xtest",
            autostart: "systemd-user",
        }
    }

    fn macos() -> Self {
        Self {
            name: "macos",
            audio: "cpal",
            shortcuts: "macos-event-tap",
            insertion: "macos-cgevent",
            autostart: "launchd",
        }
    }

    fn bsd() -> Self {
        Self {
            name: "bsd",
            audio: "cpal",
            shortcuts: "x11-grab-key",
            insertion: "x11-xtest",
            autostart: "xdg-autostart",
        }
    }

    fn generic_unix() -> Self {
        Self {
            name: "unix",
            audio: "cpal",
            shortcuts: "desktop-adapter",
            insertion: "clipboard",
            autostart: "xdg-autostart",
        }
    }

    pub fn report(&self) -> String {
        format!(
            "platform: {}\naudio: {}\nshortcuts: {}\ninsertion: {}\nautostart: {}\n",
            self.name, self.audio, self.shortcuts, self.insertion, self.autostart
        )
    }
}
