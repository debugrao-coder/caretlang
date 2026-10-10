//! Small, bounded Win32 queries. Never reads document text or sends input.
use caretlang::core::{Identity, InputState, Rect};
use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{HiDpi::*, Input::Ime::*, WindowsAndMessaging::*},
};

#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub identity: Identity,
    pub state: InputState,
    pub caret: Option<Rect>,
    pub window: Rect,
    pub workarea: Rect,
    pub dpi: u32,
}
impl Snapshot {
    /// A UIA caret can be on another display than the foreground window center.
    pub fn set_caret(&mut self, caret: Rect) {
        self.caret = Some(caret);
        // SAFETY: monitor handles belong to Windows and buffers are correctly sized.
        unsafe {
            let mon = MonitorFromPoint(
                POINT {
                    x: caret.left,
                    y: caret.top,
                },
                MONITOR_DEFAULTTONEAREST,
            );
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut info).as_bool() {
                self.workarea = rect(info.rcWork);
            }
            let mut dx = self.dpi;
            let mut dy = self.dpi;
            if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() {
                self.dpi = dx;
            }
        }
    }
}

fn rect(r: RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// WeType compatibility policy (the only IME supported by this release).
/// Its IMC_GETOPENSTATUS switches between 0/1 in both IMM and TSF clients,
/// while IMC_GETCONVERSIONMODE can stay at zero in Codex's TSF text input.
/// Do not combine those two signals or infer state from keyboard events.
pub fn mode(open: Option<usize>) -> InputState {
    match open {
        Some(0) => InputState::English,
        Some(1) => InputState::Chinese,
        _ => InputState::Unknown,
    }
}

pub fn identity() -> Option<Identity> {
    // SAFETY: Windows owns all handles; GUITHREADINFO is correctly sized.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() {
            return None;
        }
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(GetWindowThreadProcessId(fg, None), &mut gui).ok()?;
        // During foreground handoff Windows can briefly report the previous app's
        // focus HWND. Never query that old IME under the new foreground identity.
        if gui.hwndFocus.is_invalid()
            || (gui.hwndFocus != fg && GetAncestor(gui.hwndFocus, GA_ROOT) != fg)
        {
            return None;
        }
        Some(Identity {
            foreground: fg.0 as usize,
            focus: gui.hwndFocus.0 as usize,
        })
    }
}

pub fn is_shell(window: usize) -> bool {
    let mut name = [0u16; 128];
    // SAFETY: the handle is queried only; GetClassNameW bounds writes to the slice.
    let size = unsafe { GetClassNameW(HWND(window as *mut _), &mut name) };
    let class = String::from_utf16_lossy(&name[..size.max(0) as usize]);
    matches!(
        class.as_str(),
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "NotifyIconOverflowWindow"
            | "Progman"
            | "WorkerW"
            | "CaretLang.Overlay"
    )
}

pub fn snapshot() -> Option<Snapshot> {
    // SAFETY: each handle is queried only, each buffer matches its Win32 API.
    // Hung or destroyed target IME windows return Unknown within 20 ms.
    unsafe {
        let id = identity()?;
        let fg = HWND(id.foreground as *mut _);
        if IsIconic(fg).as_bool() {
            return None;
        }
        let focus = HWND(id.focus as *mut _);
        let ime = ImmGetDefaultIMEWnd(focus);
        let query = |command: usize| -> Option<usize> {
            if ime.is_invalid() {
                return None;
            }
            let mut value = 0;
            let ok = SendMessageTimeoutW(
                ime,
                WM_IME_CONTROL,
                WPARAM(command),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                20,
                Some(&mut value),
            );
            (ok.0 != 0).then_some(value)
        };
        let open = query(5);
        let state = mode(open);
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        let caret = if GetGUIThreadInfo(GetWindowThreadProcessId(fg, None), &mut gui).is_ok()
            && !gui.hwndCaret.is_invalid()
            && gui.rcCaret.bottom > gui.rcCaret.top
        {
            let mut points = [
                POINT {
                    x: gui.rcCaret.left,
                    y: gui.rcCaret.top,
                },
                POINT {
                    x: gui.rcCaret.right,
                    y: gui.rcCaret.bottom,
                },
            ];
            MapWindowPoints(Some(gui.hwndCaret), None, &mut points);
            Some(Rect {
                left: points[0].x,
                top: points[0].y,
                right: points[1].x,
                bottom: points[1].y,
            })
        } else {
            None
        };
        let mut window = RECT::default();
        GetWindowRect(fg, &mut window).ok()?;
        let mon = if let Some(c) = caret {
            MonitorFromPoint(
                POINT {
                    x: c.left,
                    y: c.top,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        } else {
            MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST)
        };
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return None;
        }
        let mut dx = 96;
        let mut dy = 96;
        if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_err() {
            dx = GetDpiForWindow(fg).max(96);
        }
        if identity() != Some(id) {
            return None;
        }
        Some(Snapshot {
            identity: id,
            state,
            caret,
            window: rect(window),
            workarea: rect(mi.rcWork),
            dpi: dx,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wetype_closed_is_english() {
        assert_eq!(mode(Some(0)), InputState::English);
    }
    #[test]
    fn unknown_does_not_guess_english() {
        assert_eq!(mode(None), InputState::Unknown);
        assert_eq!(mode(Some(2)), InputState::Unknown);
    }
    #[test]
    fn wetype_codex_open_toggle_does_not_depend_on_conversion_flag() {
        let observed_open = [1, 0, 1, 0];
        assert_eq!(
            observed_open.map(|v| mode(Some(v))),
            [
                InputState::Chinese,
                InputState::English,
                InputState::Chinese,
                InputState::English,
            ]
        );
    }
}
