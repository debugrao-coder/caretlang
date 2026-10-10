//! Pure state and placement rules, independent of Windows APIs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum InputState {
    Chinese,
    English,
    #[default]
    Unknown,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Identity {
    pub foreground: usize,
    pub focus: usize,
}
#[derive(Default)]
pub struct StateTracker {
    last: Option<(Identity, InputState)>,
}
impl StateTracker {
    /// Show the first confirmed state for each focus, then only confirmed changes.
    pub fn observe(&mut self, identity: Identity, state: InputState) -> Option<InputState> {
        if state == InputState::Unknown {
            if self.last.is_some_and(|(previous, _)| previous != identity) {
                self.reset();
            }
            return None;
        }
        let changed = self.last != Some((identity, state));
        self.last = Some((identity, state));
        changed.then_some(state)
    }
    /// Rearm the initial indicator after a new focus, pause, lock, or resume.
    pub fn reset(&mut self) {
        self.last = None;
    }
}
#[derive(Default)]
pub struct Generation(u64);
impl Generation {
    /// Issue a token for a new request, invalidating every previous request.
    pub fn advance(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
    /// Check this together with the foreground identity before displaying a reply.
    pub fn accepts(&self, generation: u64) -> bool {
        self.0 == generation
    }
}
/// Position the 30 DIP body inside an 8 DIP transparent shadow margin.
/// Prefer above the caret, then below it when the upper screen edge interferes.
pub fn popup_rect(caret: Option<Rect>, foreground: Rect, workarea: Rect, dpi: u32) -> Rect {
    let scale = |dip: i64| (dip * i64::from(dpi.clamp(48, 960)) + 48) / 96;
    let size = scale(46);
    let (x, y) = match caret {
        Some(caret) => {
            let above = i64::from(caret.top) - size;
            (
                i64::from(caret.right),
                if above < i64::from(workarea.top) {
                    i64::from(caret.bottom)
                } else {
                    above
                },
            )
        }
        None => (
            i64::from(foreground.right) - scale(16) - scale(30) - scale(8),
            i64::from(foreground.top) + scale(16) - scale(8),
        ),
    };
    let x = x.clamp(
        i64::from(workarea.left),
        (i64::from(workarea.right) - size).max(i64::from(workarea.left)),
    );
    let y = y.clamp(
        i64::from(workarea.top),
        (i64::from(workarea.bottom) - size).max(i64::from(workarea.top)),
    );
    let bounded = |n: i64| n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    Rect {
        left: bounded(x),
        top: bounded(y),
        right: bounded(x + size),
        bottom: bounded(y + size),
    }
}

/// Opacity for a display cycle. Start a fresh elapsed clock on every switch.
/// Short durations proportionally shorten both fades; reduced motion is instant.
pub fn opacity(elapsed_ms: u64, duration_ms: u64, animations: bool) -> u8 {
    if elapsed_ms >= duration_ms {
        return 0;
    }
    if !animations {
        return 255;
    }
    let fade_in = if duration_ms < 200 {
        duration_ms * 2 / 5
    } else {
        80
    };
    let fade_out = if duration_ms < 200 {
        duration_ms - fade_in
    } else {
        120
    };
    if elapsed_ms < fade_in {
        return ((elapsed_ms * 255 + fade_in / 2) / fade_in) as u8;
    }
    let remaining = duration_ms - elapsed_ms;
    if remaining < fade_out {
        return ((remaining * 255 + fade_out / 2) / fade_out) as u8;
    }
    255
}
