//! Isolated UI Automation caret lookup. No document text is requested.
use caretlang::core::Rect;
use std::{
    io::{self, BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, SAFEARRAY,
            },
            Ole::{
                SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetElemsize,
                SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype,
            },
            Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
            Variant::{VARIANT, VT_R8},
        },
        UI::{
            Accessibility::*,
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            WindowsAndMessaging::{
                CHILDID_SELF, GA_ROOT, GUITHREADINFO, GetAncestor, GetClassNameW,
                GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, OBJID_CARET,
            },
        },
    },
    core::{BOOL, Interface},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationReply {
    pub foreground: usize,
    pub focus: usize,
    pub generation: u64,
    pub rect: Option<Rect>,
}

fn parse_reply(line: &str) -> Option<LocationReply> {
    let mut fields = line.split_whitespace();
    let foreground = fields.next()?.parse().ok()?;
    let focus = fields.next()?.parse().ok()?;
    let generation = fields.next()?.parse().ok()?;
    let first = fields.next()?;
    let rect = if first == "none" {
        None
    } else {
        let rect = Rect {
            left: first.parse().ok()?,
            top: fields.next()?.parse().ok()?,
            right: fields.next()?.parse().ok()?,
            bottom: fields.next()?.parse().ok()?,
        };
        if rect.right < rect.left || rect.bottom <= rect.top {
            return None;
        }
        Some(rect)
    };
    if fields.next().is_some() {
        return None;
    }
    Some(LocationReply {
        foreground,
        focus,
        generation,
        rect,
    })
}

fn matches_request(
    reply: &LocationReply,
    request: (usize, usize, u64, Instant),
    received: Instant,
) -> bool {
    (reply.foreground, reply.focus, reply.generation) == (request.0, request.1, request.2)
        && received.saturating_duration_since(request.3) <= Duration::from_millis(180)
}

struct Process {
    child: Child,
    input: ChildStdin,
    output: Receiver<(Instant, String)>,
    started: Instant,
    ready: bool,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct UiaWorker {
    process: Option<Process>,
    pending: Option<(usize, usize, u64, Instant)>,
    retry_after: Instant,
}
impl UiaWorker {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            process: Some(Self::spawn()?),
            pending: None,
            retry_after: Instant::now(),
        })
    }
    fn spawn() -> io::Result<Process> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--uia-worker")
            .arg("--uia-parent-pid")
            .arg(std::process::id().to_string())
            .creation_flags(0x08000000) // CREATE_NO_WINDOW; worker communicates only over pipes.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let input = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::sync_channel(2);
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send((Instant::now(), line)).is_err() {
                    break;
                }
            }
        });
        Ok(Process {
            child,
            input,
            output: rx,
            started: Instant::now(),
            ready: false,
        })
    }
    fn failed(&mut self) {
        self.process = None;
        self.pending = None;
        self.retry_after = Instant::now() + Duration::from_secs(1);
    }
    /// True only while one dispatched query is awaiting its reply.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    /// Readiness is independent of query occupancy; poll drives startup/recovery.
    pub fn is_ready(&self) -> bool {
        self.process.as_ref().is_some_and(|process| process.ready)
    }
    pub fn request(&mut self, foreground: usize, focus: usize, generation: u64) {
        if self.pending.is_some() {
            return;
        }
        if self.process.is_none() && Instant::now() >= self.retry_after {
            match Self::spawn() {
                Ok(p) => self.process = Some(p),
                Err(_) => {
                    self.failed();
                    return;
                }
            }
        }
        let Some(process) = self.process.as_mut().filter(|p| p.ready) else {
            return;
        };
        if writeln!(process.input, "{foreground} {focus} {generation}")
            .and_then(|_| process.input.flush())
            .is_err()
        {
            self.failed();
            return;
        }
        self.pending = Some((foreground, focus, generation, Instant::now()));
    }
    pub fn poll(&mut self) -> Option<LocationReply> {
        // Recovery must not depend on request(): callers can legitimately wait for READY.
        if self.process.is_none() && Instant::now() >= self.retry_after {
            match Self::spawn() {
                Ok(process) => self.process = Some(process),
                Err(_) => self.failed(),
            }
        }
        let process = self.process.as_mut()?;
        if !process.ready && process.started.elapsed() > Duration::from_secs(3) {
            self.failed();
            return None;
        }
        match process.output.try_recv() {
            Ok((_, line)) if line == "READY" && !process.ready => {
                process.ready = true;
                None
            }
            Ok((received, line)) => {
                let reply = parse_reply(&line);
                let pending = self.pending.take();
                let matched =
                    reply.filter(|r| pending.is_some_and(|p| matches_request(r, p, received)));
                if matched.is_none() {
                    self.failed();
                }
                matched
            }
            Err(mpsc::TryRecvError::Empty) => {
                if self
                    .pending
                    .is_some_and(|p| p.3.elapsed() > Duration::from_millis(180))
                {
                    self.failed();
                }
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.failed();
                None
            }
        }
    }
}

struct ComApartment;
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct OwnedArray(*mut SAFEARRAY);
impl Drop for OwnedArray {
    fn drop(&mut self) {
        unsafe {
            let _ = SafeArrayDestroy(self.0);
        }
    }
}

/// Child entry point. No user text is read or returned by the protocol.
pub fn worker_main() -> windows::core::Result<()> {
    // UIA screen rectangles must use physical pixels, matching the overlay process.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
    }
    watch_parent()?;
    // This process has one COM thread; all interfaces are dropped before its apartment.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    let _apartment = ComApartment;
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if writeln!(output, "READY")
        .and_then(|_| output.flush())
        .is_err()
    {
        return Ok(());
    }
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let values: Vec<_> = line.split_whitespace().collect();
        if values.len() != 3 {
            continue;
        }
        let (Ok(foreground), Ok(focus), Ok(generation)) = (
            values[0].parse::<usize>(),
            values[1].parse::<usize>(),
            values[2].parse::<u64>(),
        ) else {
            continue;
        };
        let rect = locate(&automation, foreground, focus);
        let result = if let Some(r) = rect {
            writeln!(
                output,
                "{foreground} {focus} {generation} {} {} {} {}",
                r.left, r.top, r.right, r.bottom
            )
        } else {
            writeln!(output, "{foreground} {focus} {generation} none")
        };
        if result.and_then(|_| output.flush()).is_err() {
            break;
        }
    }
    Ok(())
}

fn watch_parent() -> windows::core::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let Some(pid) = args
        .iter()
        .position(|arg| arg == "--uia-parent-pid")
        .and_then(|index| args.get(index + 1))
        .and_then(|pid| pid.parse::<u32>().ok())
    else {
        // Direct diagnostic invocations still terminate on stdin EOF.
        return Ok(());
    };
    // Hold a real process handle (not repeated PID lookups) so PID reuse is harmless.
    // A separate waiting thread also exits when a provider has blocked the COM thread.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid)? };
    let raw = handle.0 as usize;
    thread::spawn(move || unsafe {
        let handle = HANDLE(raw as *mut _);
        let status = WaitForSingleObject(handle, INFINITE);
        let _ = CloseHandle(handle);
        if status == WAIT_OBJECT_0 {
            std::process::exit(0);
        }
    });
    Ok(())
}

fn identity_current(foreground: usize, focus: usize) -> Option<u32> {
    // HWND values are never dereferenced; Win32 checks validity on each query.
    unsafe {
        let hwnd = HWND(foreground as *mut _);
        if foreground == 0 || GetForegroundWindow() != hwnd {
            return None;
        }
        let mut pid = 0;
        let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if tid == 0 || pid == 0 {
            return None;
        }
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if focus != 0 {
            if GetGUIThreadInfo(tid, &mut gui).is_err() {
                // A foreground-only identity is a supported degraded mode.
                if focus != foreground {
                    return None;
                }
            } else if gui.hwndFocus.0 as usize != focus
                && !(gui.hwndFocus.is_invalid() && focus == foreground)
            {
                return None;
            }
        }
        Some(pid)
    }
}

fn locate(automation: &IUIAutomation, foreground: usize, focus: usize) -> Option<Rect> {
    diagnostic("request", None);
    let pid = identity_current(foreground, focus)?;
    // MSAA caret geometry is independent of UIA support. Qt/self-drawn hosts may
    // expose OBJID_CARET while GetFocusedElement/TextPattern is unavailable.
    for hwnd in [focus, foreground] {
        if hwnd == 0 {
            continue;
        }
        if let Some(rect) = msaa_caret(HWND(hwnd as *mut _), foreground) {
            if identity_current(foreground, focus)? != pid {
                return None;
            }
            diagnostic("msaa-accepted", Some(rect));
            return Some(rect);
        }
    }
    // Calls can block in third-party providers, hence isolation and the parent deadline.
    unsafe {
        let element = automation.GetFocusedElement().ok()?;
        diagnostic("focused-element", None);
        if element.CurrentIsPassword().ok()?.as_bool() {
            return None;
        }
        // Browser/WebView providers can run in a renderer process distinct from the
        // foreground window. Establish ownership via the UIA ancestor HWND instead.
        let host = owning_window(automation, &element, foreground)?;
        diagnostic("ownership-ok", None);
        let rect = if host.0 as usize != focus && host.0 as usize != foreground {
            msaa_caret(host, foreground)
        } else {
            None
        };
        let rect = rect.or_else(|| text_caret(&element, is_terminal_window(foreground)))?;
        if identity_current(foreground, focus)? != pid {
            return None;
        }
        let current = automation.GetFocusedElement().ok()?;
        if !automation
            .CompareElements(&element, &current)
            .ok()?
            .as_bool()
        {
            return None;
        }
        diagnostic("accepted", Some(rect));
        Some(rect)
    }
}

fn owning_window(
    automation: &IUIAutomation,
    element: &IUIAutomationElement,
    foreground: usize,
) -> Option<HWND> {
    // A depth bound plus the parent's process deadline bounds malformed UIA trees.
    unsafe {
        let mut current = element.clone();
        let walker = automation.RawViewWalker().ok()?;
        for _ in 0..64 {
            if let Ok(hwnd) = current.CurrentNativeWindowHandle()
                && !hwnd.is_invalid()
                && window_belongs_to(hwnd, foreground)
            {
                return Some(hwnd);
            }
            current = walker.GetParentElement(&current).ok()?;
        }
        None
    }
}

fn window_belongs_to(hwnd: HWND, foreground: usize) -> bool {
    // OS parentage, rather than process ID equality, permits embedded renderer HWNDs.
    unsafe {
        foreground != 0
            && !hwnd.is_invalid()
            && (hwnd.0 as usize == foreground
                || GetAncestor(hwnd, GA_ROOT).0 as usize == foreground)
    }
}

fn msaa_caret(hwnd: HWND, foreground: usize) -> Option<Rect> {
    if !window_belongs_to(hwnd, foreground) {
        return None;
    }
    // OBJID_CARET requests geometry only. Chromium exposes its synthetic caret here
    // even when Win32 has no hwndCaret and a collapsed UIA range has no rectangles.
    unsafe {
        let mut object = std::ptr::null_mut();
        AccessibleObjectFromWindow(hwnd, OBJID_CARET.0 as u32, &IAccessible::IID, &mut object)
            .ok()?;
        if object.is_null() {
            return None;
        }
        let accessible = IAccessible::from_raw(object);
        let child = VARIANT::from(CHILDID_SELF as i32);
        // Reject providers returning a client rectangle instead of ROLE_SYSTEM_CARET.
        if i32::try_from(&accessible.get_accRole(&child).ok()?).ok()? != 7 {
            return None;
        }
        let state = u32::try_from(&accessible.get_accState(&child).ok()?).ok()?;
        if !msaa_caret_visible(state) {
            return None;
        }
        let (mut x, mut y, mut width, mut height) = (0, 0, 0, 0);
        accessible
            .accLocation(&mut x, &mut y, &mut width, &mut height, &child)
            .ok()?;
        rect_from_values([
            f64::from(x),
            f64::from(y),
            f64::from(width),
            f64::from(height),
        ])
        .filter(plausible_caret)
    }
}

fn msaa_caret_visible(state: u32) -> bool {
    const STATE_SYSTEM_INVISIBLE: u32 = 0x8000;
    const STATE_SYSTEM_OFFSCREEN: u32 = 0x10000;
    state & (STATE_SYSTEM_INVISIBLE | STATE_SYSTEM_OFFSCREEN) == 0
}

fn diagnostic(stage: &str, rect: Option<Rect>) {
    if std::env::var_os("CARETLANG_UIA_DIAGNOSTIC").is_some() {
        eprintln!("uia {stage} rect={rect:?}");
    }
}

fn plausible_caret(rect: &Rect) -> bool {
    let width = i64::from(rect.right) - i64::from(rect.left);
    let height = i64::from(rect.bottom) - i64::from(rect.top);
    // Permit insertion beams, block cursors, and short underline cursors. Reject
    // providers returning an entire editor/input bounding box as OBJID_CARET.
    (1..=1024).contains(&height) && (0..=512).contains(&width) && width <= (height * 2).max(32)
}

fn is_terminal_window(foreground: usize) -> bool {
    let mut class = [0_u16; 128];
    let length = unsafe { GetClassNameW(HWND(foreground as *mut _), &mut class) };
    length > 0
        && String::from_utf16_lossy(&class[..length as usize]) == "CASCADIA_HOSTING_WINDOW_CLASS"
}

fn terminal_cell_caret(cell: Rect, start_comparison: i32) -> Option<Rect> {
    // Expansion at document end may move backward; do not guess that cell's edge.
    if start_comparison != 0 || !plausible_caret(&cell) {
        return None;
    }
    Some(Rect {
        right: cell.left,
        ..cell
    })
}

fn text_caret(element: &IUIAutomationElement, terminal: bool) -> Option<Rect> {
    unsafe {
        let caret = element
            .GetCurrentPatternAs::<IUIAutomationTextPattern2>(UIA_TextPattern2Id)
            .ok()
            .and_then(|p| {
                let mut active = BOOL(0);
                let range = p.GetCaretRange(&mut active).ok()?;
                active.as_bool().then_some(range)
            });
        let range = caret.or_else(|| {
            let pattern = element
                .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                .ok()?;
            let selections = pattern.GetSelection().ok()?;
            if selections.Length().ok()? != 1 {
                return None;
            }
            let range = selections.GetElement(0).ok()?;
            (range
                .CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    &range,
                    TextPatternRangeEndpoint_End,
                )
                .ok()?
                == 0)
                .then_some(range)
        })?;
        diagnostic("text-range", None);
        if let Some(rect) = bounding_rect(&range).filter(plausible_caret) {
            diagnostic("text-caret", Some(rect));
            return Some(rect);
        }
        if !terminal
            || range
                .CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    &range,
                    TextPatternRangeEndpoint_End,
                )
                .ok()?
                != 0
        {
            return None;
        }
        // Microsoft Terminal's UiaTextRangeBase returns an empty array for a
        // degenerate range; its Character expansion returns the cursor's grid cell.
        // https://github.com/microsoft/terminal/blob/main/src/types/UiaTextRangeBase.cpp
        // Clone changes only our range object: no Select(), input, or text reads.
        let expanded = range.Clone().ok()?;
        expanded.ExpandToEnclosingUnit(TextUnit_Character).ok()?;
        let start = expanded
            .CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &range,
                TextPatternRangeEndpoint_Start,
            )
            .ok()?;
        let caret = terminal_cell_caret(bounding_rect(&expanded)?, start)?;
        diagnostic("terminal-character-cell", Some(caret));
        Some(caret)
    }
}

unsafe fn bounding_rect(range: &IUIAutomationTextRange) -> Option<Rect> {
    // UIA returns an owned SAFEARRAY of doubles. Validate layout before copying four scalars.
    unsafe {
        let raw = range.GetBoundingRectangles().ok()?;
        if raw.is_null() {
            return None;
        }
        let array = OwnedArray(raw);
        if SafeArrayGetDim(array.0) != 1
            || SafeArrayGetElemsize(array.0) != 8
            || SafeArrayGetVartype(array.0).ok()? != VT_R8
        {
            return None;
        }
        let lower = SafeArrayGetLBound(array.0, 1).ok()?;
        let upper = SafeArrayGetUBound(array.0, 1).ok()?;
        if i64::from(upper) - i64::from(lower) + 1 != 4 {
            return None;
        }
        let mut values = [0_f64; 4];
        for (offset, value) in values.iter_mut().enumerate() {
            let index = lower.checked_add(offset as i32)?;
            SafeArrayGetElement(array.0, &index, (value as *mut f64).cast()).ok()?;
        }
        rect_from_values(values)
    }
}

fn rect_from_values([x, y, width, height]: [f64; 4]) -> Option<Rect> {
    if ![x, y, width, height, x + width, y + height]
        .iter()
        .all(|v| v.is_finite() && *v >= i32::MIN as f64 && *v <= i32::MAX as f64)
        || width < 0.0
        || height <= 0.0
    {
        return None;
    }
    Some(Rect {
        left: x.floor() as i32,
        top: y.floor() as i32,
        right: (x + width).ceil() as i32,
        bottom: (y + height).ceil() as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_preserves_identity_and_negative_coordinates() {
        let reply = parse_reply("42 43 9 -120 20 -119 38").unwrap();
        assert_eq!(
            (reply.foreground, reply.focus, reply.generation),
            (42, 43, 9)
        );
        assert_eq!(
            reply.rect,
            Some(Rect {
                left: -120,
                top: 20,
                right: -119,
                bottom: 38
            })
        );
    }
    #[test]
    fn protocol_distinguishes_unknown_and_rejects_extra_fields() {
        assert_eq!(parse_reply("42 43 9 none").unwrap().rect, None);
        assert!(parse_reply("42 43 9 none extra").is_none());
        assert!(parse_reply("42 43 9 0 0 1").is_none());
        assert!(parse_reply("42 43 9 0 0 -1 20").is_none());
    }
    #[test]
    fn provider_geometry_is_checked() {
        assert!(rect_from_values([f64::NAN, 0.0, 1.0, 12.0]).is_none());
        assert!(rect_from_values([0.0, 0.0, -1.0, 12.0]).is_none());
        assert!(rect_from_values([0.0, 0.0, 1.0, 0.0]).is_none());
        assert_eq!(
            rect_from_values([-12.5, 4.2, 0.0, 15.0]),
            Some(Rect {
                left: -13,
                top: 4,
                right: -12,
                bottom: 20
            })
        );
        assert_eq!(
            rect_from_values([10.0, 20.0, 0.0, 16.0]),
            Some(Rect {
                left: 10,
                top: 20,
                right: 10,
                bottom: 36
            })
        );
    }

    #[test]
    fn reply_must_match_request_identity_generation_and_deadline() {
        let start = Instant::now();
        let reply = parse_reply("42 43 9 none").unwrap();
        assert!(matches_request(
            &reply,
            (42, 43, 9, start),
            start + Duration::from_millis(180)
        ));
        assert!(!matches_request(
            &reply,
            (42, 43, 9, start),
            start + Duration::from_millis(181)
        ));
        assert!(!matches_request(&reply, (42, 43, 10, start), start));
        assert!(!matches_request(&reply, (42, 44, 9, start), start));
        assert!(!matches_request(&reply, (41, 43, 9, start), start));
    }

    #[test]
    fn stale_or_offscreen_msaa_caret_is_rejected() {
        assert!(msaa_caret_visible(0));
        assert!(msaa_caret_visible(4));
        assert!(!msaa_caret_visible(0x8000));
        assert!(!msaa_caret_visible(0x10000));
    }

    #[test]
    fn oversized_provider_rect_is_not_a_caret() {
        assert!(!plausible_caret(&Rect {
            left: 1167,
            top: 1918,
            right: 2747,
            bottom: 2130
        }));
        assert!(plausible_caret(&Rect {
            left: 1195,
            top: 1951,
            right: 1196,
            bottom: 1991
        }));
        assert!(plausible_caret(&Rect {
            left: 0,
            top: 0,
            right: 24,
            bottom: 2
        }));
    }

    #[test]
    fn terminal_character_expansion_requires_unchanged_start() {
        let cell = Rect {
            left: -100,
            top: 200,
            right: -88,
            bottom: 224,
        };
        assert_eq!(
            terminal_cell_caret(cell, 0),
            Some(Rect {
                right: -100,
                ..cell
            })
        );
        assert_eq!(terminal_cell_caret(cell, -1), None);
        assert_eq!(terminal_cell_caret(cell, 1), None);
        assert_eq!(
            terminal_cell_caret(
                Rect {
                    right: 1500,
                    ..cell
                },
                0
            ),
            None
        );
    }

    #[test]
    fn hwnd_ownership_accepts_child_and_rejects_foreign_root() {
        use windows::{
            Win32::UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WS_CHILD, WS_OVERLAPPED,
            },
            core::w,
        };
        struct HiddenWindow(HWND);
        impl Drop for HiddenWindow {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                }
            }
        }
        // Isolated test windows are never shown, activated, or sent input.
        unsafe {
            let parent = HiddenWindow(
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WS_OVERLAPPED,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let child = HiddenWindow(
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WS_CHILD,
                    0,
                    0,
                    1,
                    1,
                    Some(parent.0),
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            let foreign = HiddenWindow(
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!(""),
                    WS_OVERLAPPED,
                    0,
                    0,
                    1,
                    1,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
            );
            assert!(window_belongs_to(parent.0, parent.0.0 as usize));
            assert!(window_belongs_to(child.0, parent.0.0 as usize));
            assert!(!window_belongs_to(foreign.0, parent.0.0 as usize));
            assert!(!window_belongs_to(HWND::default(), parent.0.0 as usize));
            assert!(!window_belongs_to(HWND::default(), 0));
        }
    }
}
