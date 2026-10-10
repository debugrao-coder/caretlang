//! Local preferences and explicitly requested current-user startup registration.
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::*,
        UI::WindowsAndMessaging::*,
    },
    core::{Error, PCWSTR, Result, w},
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub duration_ms: u64,
    pub theme: Theme,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            duration_ms: 800,
            theme: Theme::System,
        }
    }
}
impl Settings {
    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "duration_ms" => {
                    if let Ok(n) = value.trim().parse::<u64>()
                        && (200..=5000).contains(&n)
                    {
                        s.duration_ms = n;
                    }
                }
                "theme" => match value.trim() {
                    "system" => s.theme = Theme::System,
                    "light" => s.theme = Theme::Light,
                    "dark" => s.theme = Theme::Dark,
                    _ => {}
                },
                _ => {}
            }
        }
        s
    }
    fn serialize(&self) -> String {
        format!(
            "duration_ms={}\ntheme={}\n",
            self.duration_ms,
            match self.theme {
                Theme::System => "system",
                Theme::Light => "light",
                Theme::Dark => "dark",
            }
        )
    }
    /// Invalid or absent preference files use built-in defaults.
    pub fn load() -> Self {
        path()
            .ok()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|s| Self::parse(&s))
            .unwrap_or_default()
    }
    /// Flush a sibling temporary file, then atomically replace the destination.
    pub fn save(&self) -> io::Result<()> {
        let destination = path()?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = destination.with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(self.serialize().as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &destination)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
    pub fn dark(&self) -> bool {
        match self.theme {
            Theme::System => dark(),
            Theme::Light => false,
            Theme::Dark => true,
        }
    }
}
fn path() -> io::Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("CaretLang").join("settings.ini"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable"))
}
/// Read Windows app theme preference without changing it.
pub fn dark() -> bool {
    let mut light = 1u32;
    let mut bytes = 4u32;
    // SAFETY: writable DWORD and byte count remain alive for this synchronous call.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut light as *mut u32).cast()),
            Some(&mut bytes),
        ) == ERROR_SUCCESS
            && light == 0
    }
}
/// Default to no animation when the system preference cannot be read.
pub fn animations() -> bool {
    let mut enabled = windows::core::BOOL(0);
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to this valid pointer.
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut windows::core::BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && enabled.as_bool()
    }
}
const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
fn startup_command() -> Result<Vec<u16>> {
    let exe = std::env::current_exe()
        .map_err(|error| Error::new(windows::Win32::Foundation::E_FAIL, error.to_string()))?;
    use std::os::windows::ffi::OsStrExt;
    let mut text = vec![b'"' as u16];
    text.extend(exe.as_os_str().encode_wide());
    text.push(b'"' as u16);
    text.push(0);
    Ok(text)
}
pub fn autostart_enabled() -> bool {
    let Ok(expected) = startup_command() else {
        return false;
    };
    let mut text = vec![0u16; 32768];
    let mut bytes = (text.len() * 2) as u32;
    // SAFETY: the buffer is valid for its declared byte size during this call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN,
            w!("CaretLang"),
            RRF_RT_REG_SZ,
            None,
            Some(text.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    status == ERROR_SUCCESS && text.get(..bytes as usize / 2) == Some(expected.as_slice())
}
/// Only call from an explicit user action in the tray menu.
pub fn set_autostart(enabled: bool) -> Result<()> {
    if enabled {
        let text = startup_command()?;
        // SAFETY: text is a terminated UTF-16 string; byte length includes terminator.
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                w!("CaretLang"),
                REG_SZ.0,
                Some(text.as_ptr().cast()),
                (text.len() * 2) as u32,
            )
            .ok()
        }
    } else {
        // SAFETY: the constant strings remain valid throughout this synchronous call.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, w!("CaretLang")) };
        if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            status.ok()
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_known_preferences() {
        assert_eq!(
            Settings::parse("duration_ms=1200\ntheme=dark\n"),
            Settings {
                duration_ms: 1200,
                theme: Theme::Dark
            }
        );
    }
    #[test]
    fn invalid_fields_preserve_safe_defaults() {
        assert_eq!(
            Settings::parse("duration_ms=0\ntheme=neon\n"),
            Settings::default()
        );
        assert_eq!(
            Settings::parse("duration_ms=18446744073709551615"),
            Settings::default()
        );
    }
    #[test]
    fn preferences_round_trip() {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            let s = Settings {
                duration_ms: 500,
                theme,
            };
            assert_eq!(Settings::parse(&s.serialize()), s);
        }
    }
}
