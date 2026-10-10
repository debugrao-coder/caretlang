#![windows_subsystem = "windows"]
//! Window callbacks only enqueue work, keeping foreign calls outside callbacks.
mod platform;
mod render;
mod settings;
mod tray;
mod uia;

use caretlang::core::{Generation, Identity, InputState, StateTracker, opacity, popup_rect};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::Write,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::*,
        System::{LibraryLoader::*, RemoteDesktop::*, Threading::*},
        UI::{Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
    core::*,
};
const WAKE: u32 = WM_APP + 1;
const TRAY: u32 = WM_APP + 2;
const FOCUS: u32 = WM_APP + 3;
const ACTIVATED: u32 = WM_APP + 4;
static WINDOW: AtomicUsize = AtomicUsize::new(0);
thread_local! {static EVENTS:RefCell<VecDeque<Event>>=const {RefCell::new(VecDeque::new())};}
#[derive(PartialEq, Eq)]
enum Event {
    Tick,
    Wake,
    Focus(usize),
    Activated(usize),
    Menu,
    Reset,
    Lock(bool),
    RestoreTray,
}
fn enqueue(event: Event) {
    EVENTS.with(|q| {
        let mut queue = q.borrow_mut();
        if matches!(event, Event::Lock(_)) {
            queue.retain(|old| !matches!(old, Event::Lock(_)));
        }
        if !queue.contains(&event) {
            queue.push_back(event);
        }
    });
}

// SAFETY: Win32 calls on the creating thread. No Rust pointers cross callbacks.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => enqueue(Event::Tick),
        WAKE => enqueue(Event::Wake),
        FOCUS => enqueue(Event::Focus(w.0)),
        ACTIVATED => enqueue(Event::Activated(w.0)),
        TRAY if matches!(l.0 as u32, WM_RBUTTONUP | WM_CONTEXTMENU | WM_LBUTTONUP) => {
            enqueue(Event::Menu)
        }
        // Snapshot already contains destination-monitor DPI and screen pixels.
        // Moving our own popup must not clear the very popup being displayed.
        WM_DPICHANGED => return LRESULT(0),
        WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_POWERBROADCAST => enqueue(Event::Reset),
        WM_WTSSESSION_CHANGE => enqueue(Event::Lock(w.0 == WTS_SESSION_LOCK as usize)),
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            unsafe {
                PostQuitMessage(0);
            }
            return LRESULT(0);
        }
        _ => {
            if msg >= 0xC000 && msg == unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) } {
                enqueue(Event::RestoreTray);
            }
            return unsafe { DefWindowProcW(hwnd, msg, w, l) };
        }
    }
    LRESULT(0)
}
unsafe extern "system" fn winevent(
    _: HWINEVENTHOOK,
    event: u32,
    target: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    let hwnd = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
    if !hwnd.is_invalid() {
        unsafe {
            let foreground = GetForegroundWindow();
            if event == EVENT_SYSTEM_FOREGROUND && target == foreground {
                let _ = PostMessageW(Some(hwnd), ACTIVATED, WPARAM(target.0 as usize), LPARAM(0));
            } else if event == EVENT_OBJECT_FOCUS
                && !target.is_invalid()
                && (target == foreground || GetAncestor(target, GA_ROOT) == foreground)
            {
                let _ = PostMessageW(Some(hwnd), FOCUS, WPARAM(target.0 as usize), LPARAM(0));
            } else {
                let _ = PostMessageW(Some(hwnd), WAKE, WPARAM(0), LPARAM(0));
            }
        }
    }
}
unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: HC_ACTION provides a live KBDLLHOOKSTRUCT. Only shortcut identities
    // are inspected; character data is never retained or logged.
    if code >= 0 && matches!(w.0 as u32, WM_KEYUP | WM_SYSKEYUP) {
        let key = unsafe { &*(l.0 as *const KBDLLHOOKSTRUCT) };
        if matches!(
            key.vkCode,
            0x10 | 0xA0
                | 0xA1
                | 0x11
                | 0xA2
                | 0xA3
                | 0x12
                | 0xA4
                | 0xA5
                | 0x20
                | 0x14
                | 0xF3
                | 0xF4
        ) {
            let hwnd = HWND(WINDOW.load(Ordering::Relaxed) as *mut _);
            unsafe {
                let _ = PostMessageW(Some(hwnd), WAKE, WPARAM(0), LPARAM(0));
            }
        }
    }
    unsafe { CallNextHookEx(None, code, w, l) }
}
struct Resources {
    hwnd: HWND,
    mutex: HANDLE,
    hooks: Vec<HWINEVENTHOOK>,
    keyboard: HHOOK,
}
impl Drop for Resources {
    fn drop(&mut self) {
        WINDOW.store(0, Ordering::Relaxed);
        // SAFETY: all handles were created and are owned by this process.
        unsafe {
            for hook in &self.hooks {
                let _ = UnhookWinEvent(*hook);
            }
            let _ = UnhookWindowsHookEx(self.keyboard);
            let _ = WTSUnRegisterSessionNotification(self.hwnd);
            let _ = KillTimer(Some(self.hwnd), 1);
            let _ = DestroyWindow(self.hwnd);
            let _ = CloseHandle(self.mutex);
        }
    }
}
struct Popup {
    snapshot: platform::Snapshot,
    started: Instant,
    generation: u64,
    located: bool,
    forced: bool,
    frame_logged: bool,
    confirmed_at: Instant,
    visible_since: Option<Instant>,
    location_attempts: u8,
}
impl Popup {
    fn state_expired(&self, now: Instant) -> bool {
        !self.forced
            && now.saturating_duration_since(self.confirmed_at) > Duration::from_millis(200)
    }
    fn visible_elapsed(&mut self, now: Instant, switching: bool) -> Option<u64> {
        if switching {
            // Alt+Tab may activate the destination before its switcher disappears.
            // Do not spend the display lifetime behind that switcher.
            self.visible_since = None;
            return None;
        }
        if self.visible_since.is_none()
            && !self.located
            && now.saturating_duration_since(self.started) < Duration::from_millis(180)
        {
            return None;
        }
        let visible = *self.visible_since.get_or_insert(now);
        Some(now.saturating_duration_since(visible).as_millis() as u64)
    }
}
struct App {
    hwnd: HWND,
    renderer: render::Renderer,
    worker: Option<uia::UiaWorker>,
    tray: tray::Tray,
    settings: settings::Settings,
    tracker: StateTracker,
    generation: Generation,
    popup: Option<Popup>,
    paused: bool,
    locked: bool,
    fast_until: Instant,
    last_query: Instant,
    last_identity: Option<Identity>,
    last_snapshot: Option<platform::Snapshot>,
    last_observed: Option<(Identity, InputState)>,
    last_input: u32,
    last_activity: Instant,
    log: Option<std::fs::File>,
    started: Instant,
    demo: bool,
    demo_phase: u8,
}
impl App {
    fn log(&mut self, text: &str) {
        if let Some(f) = &mut self.log {
            let _ = writeln!(f, "{} {text}", self.started.elapsed().as_millis());
            let _ = f.flush();
        }
    }
    fn hide(&mut self) {
        if let Some(p) = &self.popup {
            self.log(&format!(
                "hide elapsed_ms={}",
                p.started.elapsed().as_millis()
            ));
        }
        self.popup = None;
        self.generation.advance();
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
    fn reset(&mut self) {
        self.log("reset");
        self.hide();
        self.tracker.reset();
        self.last_identity = None;
        self.fast_until = Instant::now() + Duration::from_millis(250);
    }
    fn show(&mut self, snapshot: platform::Snapshot) {
        // Keep the pending lookup invisible rather than flashing at the fallback corner.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        let token = self.generation.advance();
        self.log(&format!(
            "switch state={:?} caret={} dpi={}",
            snapshot.state,
            snapshot.caret.is_some(),
            snapshot.dpi
        ));
        self.popup = Some(Popup {
            snapshot,
            started: Instant::now(),
            generation: token,
            located: snapshot.caret.is_some(),
            forced: self.demo,
            frame_logged: false,
            confirmed_at: Instant::now(),
            visible_since: None,
            location_attempts: 0,
        });
    }
    fn tick(&mut self) {
        let now = Instant::now();
        if let Some(worker) = &mut self.worker
            && let Some(reply) = worker.poll()
            && self.generation.accepts(reply.generation)
            && platform::identity()
                == Some(Identity {
                    foreground: reply.foreground,
                    focus: reply.focus,
                })
            && let Some(p) = &mut self.popup
        {
            if let Some(rect) = reply.rect {
                p.located = true;
                p.snapshot.set_caret(rect);
            }
            self.log(&format!("caret_reply={reply:?}"));
        }
        if self.paused || self.locked {
            self.hide();
            return;
        }
        let mut input = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetLastInputInfo(&mut input) }.as_bool() && input.dwTime != self.last_input {
            self.last_input = input.dwTime;
            self.last_activity = now;
        }
        let current = platform::identity();
        if current != self.last_identity {
            self.log(&format!(
                "identity {:?} -> {:?}",
                self.last_identity, current
            ));
            self.hide();
            self.tracker.reset();
            self.last_identity = current;
        }
        let query_interval = if now < self.fast_until {
            25
        } else if self.last_activity.elapsed() < Duration::from_secs(3) {
            100
        } else {
            750
        };
        if !self.demo
            && now.duration_since(self.last_query) >= Duration::from_millis(query_interval)
        {
            self.last_query = now;
            if let Some(s) = platform::snapshot() {
                let observed = Some((s.identity, s.state));
                if self.last_observed != observed {
                    self.log(&format!("observed={observed:?}"));
                    self.last_observed = observed;
                }
                if s.state != InputState::Unknown
                    && s.identity.foreground != self.hwnd.0 as usize
                    && !platform::is_shell(s.identity.foreground)
                {
                    self.last_snapshot = Some(s);
                    if let Some(p) = &mut self.popup
                        && p.snapshot.identity == s.identity
                    {
                        p.confirmed_at = Instant::now();
                    }
                }
                if self.tracker.observe(s.identity, s.state).is_some()
                    && !self.demo
                    && !platform::is_shell(s.identity.foreground)
                    && s.identity.foreground != self.hwnd.0 as usize
                {
                    self.show(s);
                }
                if s.state == InputState::Unknown
                    && self
                        .popup
                        .as_ref()
                        .is_some_and(|p| p.state_expired(Instant::now()))
                {
                    self.hide();
                    self.tracker.reset();
                }
            } else {
                self.hide();
                // snapshot() can fail after identity() succeeded during handoff.
                // Rearm so the same identity can display when the query recovers.
                self.tracker.reset();
            }
        }
        if self.demo {
            let phase = (self.started.elapsed().as_secs() / 30) as u8;
            if phase >= 2 {
                unsafe {
                    PostQuitMessage(0);
                }
                return;
            }
            if self.demo_phase != phase {
                self.demo_phase = phase;
                if let Some(mut s) = platform::snapshot() {
                    s.state = if phase == 0 {
                        InputState::Chinese
                    } else {
                        InputState::English
                    };
                    self.show(s);
                }
            }
        }
        let Some(p) = &mut self.popup else {
            return;
        };
        if current != Some(p.snapshot.identity) {
            self.hide();
            return;
        }
        let switching = unsafe { GetAsyncKeyState(i32::from(VK_MENU.0)) } < 0;
        if switching {
            p.started = Instant::now();
            p.location_attempts = 0;
            p.visible_elapsed(p.started, true);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        if !p.located
            && p.location_attempts < 3
            && p.visible_since.is_none()
            && let Some(worker) = &mut self.worker
            && worker.is_ready()
            && !worker.is_pending()
        {
            worker.request(
                p.snapshot.identity.foreground,
                p.snapshot.identity.focus,
                p.generation,
            );
            p.location_attempts += 1;
        }
        let Some(elapsed) = p.visible_elapsed(Instant::now(), false) else {
            return;
        };
        let duration = if self.demo {
            29900
        } else {
            self.settings.duration_ms
        };
        if elapsed >= duration {
            self.hide();
            return;
        }
        let alpha = opacity(elapsed, duration, settings::animations());
        let position = popup_rect(
            p.snapshot.caret,
            p.snapshot.window,
            p.snapshot.workarea,
            p.snapshot.dpi,
        );
        let result = self.renderer.render(
            self.hwnd,
            p.snapshot.state,
            self.settings.dark(),
            p.snapshot.dpi,
            position,
            alpha,
        );
        let log_frame = alpha > 0 && !p.frame_logged;
        p.frame_logged |= log_frame;
        if let Err(e) = result {
            self.log(&format!("render_error={e}"));
            self.hide();
            if let Ok(r) = render::Renderer::new() {
                self.renderer = r;
            }
            return;
        }
        // SAFETY: our own nonactivating layered window; no keyboard focus change.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        if log_frame {
            self.log(&format!(
                "frame alpha={alpha} position={position:?} visible={}",
                unsafe { IsWindowVisible(self.hwnd).as_bool() }
            ));
        }
    }
    fn menu(&mut self) {
        self.hide();
        let Some(command) = self.tray.menu(self.paused, &self.settings) else {
            return;
        };
        match command {
            tray::PAUSE => {
                self.paused = !self.paused;
                self.reset();
                self.tray.refresh(self.paused);
            }
            tray::DURATION_SHORT => self.settings.duration_ms = 500,
            tray::DURATION_NORMAL => self.settings.duration_ms = 800,
            tray::DURATION_LONG => self.settings.duration_ms = 1200,
            tray::THEME_SYSTEM => self.settings.theme = settings::Theme::System,
            tray::THEME_LIGHT => self.settings.theme = settings::Theme::Light,
            tray::THEME_DARK => self.settings.theme = settings::Theme::Dark,
            tray::AUTOSTART => {
                if let Err(e) = settings::set_autostart(!settings::autostart_enabled()) {
                    self.error(&format!("开机启动设置失败：{e}"));
                }
            }
            tray::PREVIEW => {
                if let Some(mut s) = self.last_snapshot {
                    // Explicit preview returns to the last observed editor.
                    unsafe {
                        let _ = SetForegroundWindow(HWND(s.identity.foreground as *mut _));
                    }
                    self.last_identity = Some(s.identity);
                    s.state = InputState::Chinese;
                    self.show(s);
                    if let Some(p) = &mut self.popup {
                        p.forced = true;
                    }
                }
            }
            tray::QUIT => unsafe {
                PostQuitMessage(0);
            },
            _ => {}
        }
        if let Err(e) = self.settings.save() {
            self.error(&format!("设置保存失败：{e}"));
        }
    }
    fn error(&mut self, text: &str) {
        self.log(text);
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        unsafe {
            MessageBoxW(
                Some(self.hwnd),
                PCWSTR(wide.as_ptr()),
                w!("CaretLang"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
fn run() -> Result<()> {
    // SAFETY: initialize once before windows; UI ownership stays on this thread.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        let mutex = CreateMutexW(None, false, w!("Local\\CaretLang.Instance.v1"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(mutex);
            return Ok(());
        }
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: w!("CaretLang.Overlay"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            let _ = CloseHandle(mutex);
            return Err(Error::from_thread());
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("CaretLang.Overlay"),
            w!("CaretLang"),
            WS_POPUP,
            0,
            0,
            46,
            46,
            None,
            None,
            Some(instance),
            None,
        )?;
        WINDOW.store(hwnd.0 as usize, Ordering::Relaxed);
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), Some(instance), 0)?;
        let hooks = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS),
        ]
        .into_iter()
        .map(|(a, b)| {
            SetWinEventHook(
                a,
                b,
                None,
                Some(winevent),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        })
        .collect();
        let _resources = Resources {
            hwnd,
            mutex,
            keyboard,
            hooks,
        };
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        let args: Vec<_> = std::env::args().collect();
        let log = args
            .iter()
            .position(|a| a == "--diagnostic")
            .and_then(|i| args.get(i + 1))
            .and_then(|path| std::fs::File::create(path).ok());
        let started = Instant::now();
        let mut app = App {
            hwnd,
            renderer: render::Renderer::new()?,
            worker: uia::UiaWorker::new().ok(),
            tray: tray::Tray::new(hwnd, TRAY)?,
            settings: settings::Settings::load(),
            tracker: StateTracker::default(),
            generation: Generation::default(),
            popup: None,
            paused: false,
            locked: false,
            fast_until: started + Duration::from_millis(300),
            last_query: started - Duration::from_secs(1),
            last_identity: None,
            last_snapshot: None,
            last_observed: None,
            last_input: 0,
            last_activity: started,
            log,
            started,
            demo: args.iter().any(|a| a == "--preview"),
            demo_phase: 255,
        };
        if args.iter().any(|a| a == "--light") {
            app.settings.theme = settings::Theme::Light;
        }
        if args.iter().any(|a| a == "--dark") {
            app.settings.theme = settings::Theme::Dark;
        }
        app.log("started v0.1.0");
        SetTimer(Some(hwnd), 1, 16, None);
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            if result.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
            while let Some(event) = EVENTS.with(|q| q.borrow_mut().pop_front()) {
                match event {
                    Event::Tick => app.tick(),
                    Event::Wake => {
                        app.fast_until = Instant::now() + Duration::from_millis(300);
                        app.tick();
                    }
                    Event::Focus(target) => {
                        let foreground = GetForegroundWindow();
                        let target = HWND(target as *mut _);
                        if target == foreground || GetAncestor(target, GA_ROOT) == foreground {
                            // A burst of focus notifications should not restart a just-issued popup.
                            if !app
                                .popup
                                .as_ref()
                                .is_some_and(|p| p.started.elapsed() < Duration::from_millis(200))
                            {
                                app.tracker.reset();
                            }
                            app.fast_until = Instant::now() + Duration::from_millis(300);
                            app.tick();
                        }
                    }
                    Event::Activated(target) => {
                        if GetForegroundWindow().0 as usize == target && !platform::is_shell(target)
                        {
                            app.reset();
                            app.last_query = Instant::now() - Duration::from_secs(1);
                            app.tick();
                        }
                    }
                    Event::Menu => app.menu(),
                    Event::Reset => app.reset(),
                    Event::Lock(locked) => {
                        app.locked = locked;
                        app.reset();
                    }
                    Event::RestoreTray => app.tray.restore(),
                }
            }
            let interval = if app.popup.is_some() || Instant::now() < app.fast_until {
                16
            } else if app.last_activity.elapsed() < Duration::from_secs(3) {
                100
            } else {
                750
            };
            SetTimer(Some(hwnd), 1, interval, None);
        }
        app.log("stopped");
        Ok(())
    }
}
fn main() {
    if std::env::args().any(|a| a == "--uia-worker") {
        let _ = uia::worker_main();
        return;
    }
    let args: Vec<_> = std::env::args().collect();
    if let Some(directory) = args
        .iter()
        .position(|a| a == "--export-preview")
        .and_then(|i| args.get(i + 1))
    {
        let export = || -> Result<()> {
            std::fs::create_dir_all(directory).map_err(|e| Error::new(E_FAIL, e.to_string()))?;
            let mut renderer = render::Renderer::new()?;
            for (name, state, dark) in [
                ("dark-zh", InputState::Chinese, true),
                ("dark-en", InputState::English, true),
                ("light-zh", InputState::Chinese, false),
                ("light-en", InputState::English, false),
            ] {
                renderer.export_preview(
                    &std::path::Path::new(directory).join(format!("{name}.bmp")),
                    state,
                    dark,
                    192,
                )?;
            }
            Ok(())
        };
        if export().is_err() {
            std::process::exit(1);
        }
        return;
    }
    if let Err(e) = run() {
        let text: Vec<u16> = format!("CaretLang 启动失败：{e}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: both strings are NUL terminated and live for the call.
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                w!("CaretLang"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;
    fn pending_popup(now: Instant) -> Popup {
        Popup {
            snapshot: platform::Snapshot {
                identity: Identity {
                    foreground: 1,
                    focus: 2,
                },
                state: InputState::Chinese,
                caret: None,
                window: Default::default(),
                workarea: Default::default(),
                dpi: 96,
            },
            started: now,
            generation: 1,
            located: false,
            forced: false,
            frame_logged: false,
            confirmed_at: now,
            visible_since: None,
            location_attempts: 0,
        }
    }
    #[test]
    fn lookup_wait_does_not_flash_corner_or_consume_visible_duration() {
        let now = Instant::now();
        let mut popup = pending_popup(now);
        assert_eq!(
            popup.visible_elapsed(now + Duration::from_millis(79), false),
            None
        );
        popup.located = true;
        assert_eq!(
            popup.visible_elapsed(now + Duration::from_millis(100), false),
            Some(0)
        );
        assert_eq!(
            popup.visible_elapsed(now + Duration::from_millis(899), false),
            Some(799)
        );
        let mut fallback = pending_popup(now);
        assert_eq!(
            fallback.visible_elapsed(now + Duration::from_millis(179), false),
            None
        );
        assert_eq!(
            fallback.visible_elapsed(now + Duration::from_millis(180), false),
            Some(0)
        );
    }
    #[test]
    fn alt_tab_does_not_consume_hint_while_switcher_is_open() {
        let now = Instant::now();
        let mut popup = pending_popup(now);
        popup.located = true;
        assert_eq!(popup.visible_elapsed(now, true), None);
        assert_eq!(
            popup.visible_elapsed(now + Duration::from_secs(3), true),
            None
        );
        assert_eq!(
            popup.visible_elapsed(now + Duration::from_secs(4), false),
            Some(0)
        );
        assert_eq!(
            popup.visible_elapsed(now + Duration::from_millis(4799), false),
            Some(799)
        );
    }
    #[test]
    fn brief_read_timeout_preserves_focus_hint_but_stale_state_expires() {
        let now = Instant::now();
        let mut popup = pending_popup(now);
        assert!(!popup.state_expired(now + Duration::from_millis(41)));
        assert!(!popup.state_expired(now + Duration::from_millis(200)));
        assert!(popup.state_expired(now + Duration::from_millis(201)));
        popup.confirmed_at = now + Duration::from_millis(190);
        assert!(!popup.state_expired(now + Duration::from_millis(201)));
    }
    #[test]
    fn moving_overlay_to_a_new_dpi_does_not_reset_the_popup() {
        EVENTS.with(|queue| queue.borrow_mut().clear());
        // This notification branch must not call native functions or dereference lParam.
        unsafe {
            wndproc(HWND::default(), WM_DPICHANGED, WPARAM(0), LPARAM(0));
        }
        assert!(EVENTS.with(|queue| queue.borrow().is_empty()));
    }
    #[test]
    fn modal_menu_does_not_accumulate_timer_and_wake_events() {
        EVENTS.with(|queue| queue.borrow_mut().clear());
        for _ in 0..1000 {
            enqueue(Event::Tick);
            enqueue(Event::Wake);
        }
        assert_eq!(EVENTS.with(|queue| queue.borrow().len()), 2);
    }
}
