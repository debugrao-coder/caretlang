//! Native notification-area icon and keyboard-accessible popup menu.
use crate::settings::{Settings, Theme, autostart_enabled};
use std::cell::Cell;
use windows::{
    Win32::{
        Foundation::{E_FAIL, HWND, LPARAM, POINT, WPARAM},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{Error, PCWSTR, Result, w},
};
pub const PAUSE: u32 = 100;
pub const DURATION_SHORT: u32 = 200;
pub const DURATION_NORMAL: u32 = 201;
pub const DURATION_LONG: u32 = 202;
pub const THEME_SYSTEM: u32 = 300;
pub const THEME_LIGHT: u32 = 301;
pub const THEME_DARK: u32 = 302;
pub const AUTOSTART: u32 = 400;
pub const PREVIEW: u32 = 500;
pub const QUIT: u32 = 900;

pub struct Tray {
    hwnd: HWND,
    callback: u32,
    icon: HICON,
    paused: Cell<bool>,
}
impl Tray {
    pub fn new(hwnd: HWND, callback: u32) -> Result<Self> {
        let icon = make_icon()?;
        let tray = Self {
            hwnd,
            callback,
            icon,
            paused: Cell::new(false),
        };
        // SAFETY: data references an owned icon and live host window for the call.
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &tray.data()) }.as_bool() {
            return Err(Error::new(E_FAIL, "Unable to add notification icon"));
        }
        Ok(tray)
    }
    fn data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: self.callback,
            hIcon: self.icon,
            ..Default::default()
        };
        let tip = if self.paused.get() {
            "CaretLang · 已暂停"
        } else {
            "CaretLang · 输入法状态提示"
        };
        for (target, source) in data.szTip.iter_mut().zip(tip.encode_utf16()) {
            *target = source;
        }
        data
    }
    pub fn refresh(&self, paused: bool) {
        self.paused.set(paused); /* SAFETY: valid owned data for synchronous shell call. */
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data());
        }
    }
    pub fn restore(&self) {
        /* SAFETY: Explorer restart discarded the previous registration. */
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &self.data());
        }
    }
    pub fn menu(&self, paused: bool, settings: &Settings) -> Option<u32> {
        self.try_menu(paused, settings).ok().flatten()
    }
    fn try_menu(&self, paused: bool, settings: &Settings) -> Result<Option<u32>> {
        // SAFETY: this thread owns every menu handle until the RAII guard drops.
        unsafe {
            let menu = Menu(CreatePopupMenu()?);
            AppendMenuW(menu.0, MF_STRING | MF_DISABLED, 0, w!("CaretLang"))?;
            item(
                menu.0,
                PAUSE,
                if paused {
                    "恢复提示"
                } else {
                    "暂停提示"
                },
                false,
            )?;
            item(menu.0, PREVIEW, "预览提示", false)?;
            AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())?;
            item(
                menu.0,
                DURATION_SHORT,
                "显示时长 · 0.5 秒",
                settings.duration_ms == 500,
            )?;
            item(
                menu.0,
                DURATION_NORMAL,
                "显示时长 · 0.8 秒",
                settings.duration_ms == 800,
            )?;
            item(
                menu.0,
                DURATION_LONG,
                "显示时长 · 1.2 秒",
                settings.duration_ms == 1200,
            )?;
            AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())?;
            item(
                menu.0,
                THEME_SYSTEM,
                "主题 · 跟随系统",
                settings.theme == Theme::System,
            )?;
            item(
                menu.0,
                THEME_LIGHT,
                "主题 · 浅色",
                settings.theme == Theme::Light,
            )?;
            item(
                menu.0,
                THEME_DARK,
                "主题 · 深色",
                settings.theme == Theme::Dark,
            )?;
            AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())?;
            item(menu.0, AUTOSTART, "开机启动", autostart_enabled())?;
            item(menu.0, QUIT, "退出", false)?;
            let mut point = POINT::default();
            GetCursorPos(&mut point)?;
            let _ = SetForegroundWindow(self.hwnd);
            let selected = TrackPopupMenu(
                menu.0,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                self.hwnd,
                None,
            )
            .0 as u32;
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            Ok((selected != 0).then_some(selected))
        }
    }
}
impl Drop for Tray {
    fn drop(&mut self) {
        /* SAFETY: registration is removed before its owned icon is destroyed. */
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = DestroyIcon(self.icon);
        }
    }
}
struct Menu(HMENU);
impl Drop for Menu {
    fn drop(&mut self) {
        /* SAFETY: Menu exclusively owns this handle. */
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}
fn item(menu: HMENU, id: u32, label: &str, checked: bool) -> Result<()> {
    let label: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
    // SAFETY: menu is valid and AppendMenuW copies the terminated label.
    unsafe {
        AppendMenuW(
            menu,
            MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED },
            id as usize,
            PCWSTR(label.as_ptr()),
        )
    }
}
fn make_icon() -> Result<HICON> {
    let mut pixels = [0u8; 32 * 32 * 4];
    let mut mask = [255u8; 32 * 4];
    for y in 0usize..32 {
        for x in 0usize..32 {
            let dx = if x < 7 { 7 - x } else { x.saturating_sub(24) };
            let dy = if y < 7 { 7 - y } else { y.saturating_sub(24) };
            if dx * dx + dy * dy > 49 {
                continue;
            }
            mask[y * 4 + x / 8] &= !(0x80 >> (x % 8));
            let i = (y * 32 + x) * 4;
            let glyph = ((9..12).contains(&x) && (8..24).contains(&y))
                || ((9..23).contains(&x) && (21..24).contains(&y))
                || ((17..23).contains(&x) && (8..11).contains(&y));
            let color = if glyph {
                [243, 250, 246, 255]
            } else {
                [129, 117, 45, 255]
            };
            pixels[i..i + 4].copy_from_slice(&color);
        }
    }
    // SAFETY: both bitmaps have correctly sized 32-by-32 storage; CreateIcon copies them.
    unsafe { CreateIcon(None, 32, 32, 1, 32, mask.as_ptr(), pixels.as_ptr()) }
}
