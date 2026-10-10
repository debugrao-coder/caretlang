use std::{
    io::Write,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::*,
    Security::*,
    System::Threading::*,
    UI::{Accessibility::*, Input::Ime::*, WindowsAndMessaging::*},
};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--events") {
        unsafe {
            let hook = SetWinEventHook(
                EVENT_OBJECT_FOCUS,
                EVENT_OBJECT_VALUECHANGE,
                None,
                Some(event_probe),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            SetTimer(None, 1, 90000, None);
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                if message.message == WM_TIMER {
                    break;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = UnhookWinEvent(hook);
        }
        return;
    }
    if args.get(1).is_some_and(|s| s == "--integrity") {
        for value in args.iter().skip(2) {
            if let Ok(pid) = value.parse::<u32>() {
                println!("pid={pid} integrity={:?}", integrity(pid));
            }
        }
        println!("probe_integrity={:?}", integrity(std::process::id()));
        return;
    }
    let seconds = args.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(5);
    let start = Instant::now();
    let mut previous = String::new();
    while start.elapsed().as_secs() < seconds {
        // SAFETY: only query current system handles; every call checks success.
        let line = unsafe {
            let fg = GetForegroundWindow();
            let mut pid = 0;
            let tid = GetWindowThreadProcessId(fg, Some(&mut pid));
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            let ok = GetGUIThreadInfo(tid, &mut info).is_ok();
            let target = if ok && !info.hwndFocus.is_invalid() {
                info.hwndFocus
            } else {
                fg
            };
            let ime = ImmGetDefaultIMEWnd(target);
            let focus_tid = GetWindowThreadProcessId(target, None);
            let mut focused = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            let focus_ok = GetGUIThreadInfo(focus_tid, &mut focused).is_ok();
            let mut class = [0u16; 128];
            let length = GetClassNameW(target, &mut class).max(0) as usize;
            let class = String::from_utf16_lossy(&class[..length]);
            let query = |code: usize| -> std::result::Result<usize, u32> {
                if ime.is_invalid() {
                    return Err(1400);
                }
                let mut value = 0;
                SetLastError(WIN32_ERROR(0));
                let r = SendMessageTimeoutW(
                    ime,
                    WM_IME_CONTROL,
                    WPARAM(code),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    30,
                    Some(&mut value),
                );
                if r.0 != 0 {
                    Ok(value)
                } else {
                    Err(GetLastError().0)
                }
            };
            format!(
                "pid={pid} tid={tid} focus_tid={focus_tid} class={class:?} focus_ok={focus_ok} focus_caret={:?} focus_rect={:?} fg={:?} focus={:?} ime={:?} open={:?} mode={:?} caret={:?} rect={:?}",
                focused.hwndCaret,
                focused.rcCaret,
                fg,
                target,
                ime,
                query(5),
                query(1),
                info.hwndCaret,
                info.rcCaret
            )
        };
        if line != previous {
            println!("{} {}", start.elapsed().as_millis(), line);
            let _ = std::io::stdout().flush();
            previous = line;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

unsafe extern "system" fn event_probe(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _: u32,
    time: u32,
) {
    unsafe {
        let fg = GetForegroundWindow();
        if hwnd.is_invalid() || (hwnd != fg && GetAncestor(hwnd, GA_ROOT) != fg) {
            return;
        }
        println!(
            "time={time} event={event:#x} fg={} hwnd={} object={object} child={child}",
            fg.0 as usize, hwnd.0 as usize
        );
        let _ = std::io::stdout().flush();
    }
}

fn integrity(pid: u32) -> windows::core::Result<u32> {
    // SAFETY: OpenProcess and token handles are closed on every result path;
    // the aligned buffer owns the SID while its last authority is copied.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        let _ = CloseHandle(process);
        opened?;
        let result = (|| {
            let mut buffer = vec![0usize; 1024];
            let mut needed = 0;
            GetTokenInformation(
                token,
                TokenIntegrityLevel,
                Some(buffer.as_mut_ptr().cast()),
                (buffer.len() * std::mem::size_of::<usize>()) as u32,
                &mut needed,
            )?;
            let label = &*(buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            let count = *GetSidSubAuthorityCount(label.Label.Sid);
            Ok(*GetSidSubAuthority(label.Label.Sid, u32::from(count) - 1))
        })();
        let _ = CloseHandle(token);
        result
    }
}
