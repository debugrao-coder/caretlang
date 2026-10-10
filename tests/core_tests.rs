use caretlang::core::{Generation, Identity, InputState, Rect, StateTracker, opacity, popup_rect};

fn identity(foreground: usize, focus: usize) -> Identity {
    Identity { foreground, focus }
}
fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect {
        left,
        top,
        right,
        bottom,
    }
}

#[test]
fn first_focus_and_confirmed_changes_emit_without_repeating() {
    let mut tracker = StateTracker::default();
    assert_eq!(
        tracker.observe(identity(1, 2), InputState::Chinese),
        Some(InputState::Chinese)
    );
    assert_eq!(tracker.observe(identity(1, 2), InputState::Chinese), None);
    assert_eq!(
        tracker.observe(identity(1, 2), InputState::English),
        Some(InputState::English)
    );
    assert_eq!(
        tracker.observe(identity(1, 2), InputState::Chinese),
        Some(InputState::Chinese)
    );
    assert_eq!(
        tracker.observe(identity(3, 4), InputState::English),
        Some(InputState::English)
    );
    assert_eq!(
        tracker.observe(identity(3, 5), InputState::Chinese),
        Some(InputState::Chinese)
    );
}

#[test]
fn transient_unknown_does_not_repeat_focus_but_reset_rearms_it() {
    let mut tracker = StateTracker::default();
    tracker.observe(identity(1, 2), InputState::Chinese);
    assert_eq!(tracker.observe(identity(1, 2), InputState::Unknown), None);
    assert_eq!(tracker.observe(identity(1, 2), InputState::Chinese), None);
    assert_eq!(
        tracker.observe(identity(1, 2), InputState::English),
        Some(InputState::English)
    );
    tracker.reset();
    assert_eq!(
        tracker.observe(identity(1, 2), InputState::Chinese),
        Some(InputState::Chinese)
    );
}

#[test]
fn unknown_first_focus_waits_for_confirmed_state() {
    let mut tracker = StateTracker::default();
    assert_eq!(tracker.observe(identity(1, 2), InputState::Unknown), None);
    assert_eq!(
        tracker.observe(identity(1, 2), InputState::English),
        Some(InputState::English)
    );
    assert_eq!(tracker.observe(identity(3, 4), InputState::Unknown), None);
    assert_eq!(
        tracker.observe(identity(3, 4), InputState::Chinese),
        Some(InputState::Chinese)
    );
}

#[test]
fn obsolete_queries_are_rejected() {
    let mut generation = Generation::default();
    let old = generation.advance();
    assert!(generation.accepts(old));
    let current = generation.advance();
    assert!(!generation.accepts(old));
    assert!(generation.accepts(current));
}

#[test]
fn caret_body_is_above_and_right_with_shadow_margin() {
    assert_eq!(
        popup_rect(
            Some(rect(100, 100, 102, 120)),
            rect(0, 0, 900, 800),
            rect(0, 0, 1920, 1080),
            96
        ),
        rect(102, 54, 148, 100)
    );
    assert_eq!(
        popup_rect(
            Some(rect(100, 100, 102, 120)),
            rect(0, 0, 900, 800),
            rect(0, 0, 1920, 1080),
            144
        ),
        rect(102, 31, 171, 100)
    );
}

#[test]
fn screen_edges_and_negative_monitor_coordinates_are_clamped() {
    let work = rect(-1920, 0, 0, 1080);
    let p = popup_rect(Some(rect(-3, 2, -1, 22)), work, work, 192);
    assert_eq!(p, rect(-92, 22, 0, 114));
    let tiny = popup_rect(None, rect(0, 0, 10, 10), rect(0, 0, 20, 20), 96);
    assert_eq!(tiny, rect(0, 0, 46, 46));
}

#[test]
fn fallback_body_has_sixteen_dip_inset() {
    assert_eq!(
        popup_rect(None, rect(100, 100, 900, 800), rect(0, 0, 1920, 1080), 96),
        rect(846, 108, 892, 154)
    );
}

#[test]
fn opacity_has_fade_hold_and_exact_expiration() {
    assert_eq!(opacity(0, 800, true), 0);
    assert_eq!(opacity(40, 800, true), 128);
    assert_eq!(opacity(80, 800, true), 255);
    assert_eq!(opacity(680, 800, true), 255);
    assert_eq!(opacity(740, 800, true), 128);
    assert_eq!(opacity(800, 800, true), 0);
    assert_eq!(opacity(u64::MAX, 800, true), 0);
    assert_eq!(opacity(0, 800, false), 255);
    assert_eq!(opacity(799, 800, false), 255);
    assert_eq!(opacity(800, 800, false), 0);
    assert_eq!(opacity(0, 0, true), 0);
    assert_eq!(opacity(40, 100, true), 255);
    assert_eq!(opacity(70, 100, true), 128);
}
