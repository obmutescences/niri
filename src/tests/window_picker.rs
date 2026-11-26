//! Regression tests for the window picker's keyboard handling.
//!
//! The picker reads the keysym from `raw` in the keyboard intercept closure. After upstream
//! redefined `raw` as the set of keysyms that differ from `modified`, a plain key on a latin layout
//! produced an empty slice and the picker stopped seeing any keys at all.

use niri_config::Config;
use smithay::backend::input::{InputEvent, InputTime, KeyState, Keycode};
use smithay::input::keyboard::Keysym;

use crate::tests::fixture::Fixture;
use crate::tests::test_input_backend::{TestInputBackend, TestKeyboardKeyEvent};

// evdev codes + 8 = xkb keycodes.
const ESC: Keycode = Keycode::new(1 + 8);
const TAB: Keycode = Keycode::new(15 + 8);
const ENTER: Keycode = Keycode::new(28 + 8);
const LCTRL: Keycode = Keycode::new(29 + 8);
const A: Keycode = Keycode::new(30 + 8);
const SLASH: Keycode = Keycode::new(53 + 8);
const SPACE: Keycode = Keycode::new(57 + 8);
const F2: Keycode = Keycode::new(60 + 8);
const RIGHT: Keycode = Keycode::new(106 + 8);

fn send(f: &mut Fixture, code: Keycode, pressed: bool) {
    let state = f.niri_state();
    state.process_input_event(InputEvent::<TestInputBackend>::Keyboard {
        event: TestKeyboardKeyEvent {
            time: InputTime::from_micros(0),
            code,
            state: if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            count: 1,
        },
    });
}

fn tap(f: &mut Fixture, code: Keycode) {
    send(f, code, true);
    send(f, code, false);
}

fn set_up() -> Fixture {
    let config = Config::parse_mem(
        r#"
        binds {
            Tab repeat=false { toggle-window-picker; }
        }
        "#,
    )
    .unwrap();

    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));
    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);
    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.roundtrip(id);

    // Sanity-check the keycodes against the fixture keymap.
    {
        let state = f.niri_state();
        let keyboard = state.niri.seat.get_keyboard().unwrap();
        keyboard.with_xkb_state(state, |xkb| {
            let xkb = xkb.xkb().lock().unwrap();
            let xkb_state = unsafe { xkb.state() };
            assert_eq!(xkb_state.key_get_one_sym(TAB), Keysym::Tab);
            assert_eq!(xkb_state.key_get_one_sym(ESC), Keysym::Escape);
            assert_eq!(xkb_state.key_get_one_sym(A), Keysym::a);
        });
    }

    f
}

#[test]
fn picker_letters_select_window() {
    let mut f = set_up();

    tap(&mut f, TAB);
    assert!(f.niri().window_picker_ui.is_active(), "picker did not open");

    // The single window is labeled "A", so the letter must select it and close the picker.
    tap(&mut f, A);
    f.niri_complete_animations();
    assert!(
        !f.niri().window_picker_ui.is_active(),
        "letter did not select a window"
    );
}

#[test]
fn picker_escape_closes() {
    let mut f = set_up();

    tap(&mut f, TAB);
    assert!(f.niri().window_picker_ui.is_active(), "picker did not open");

    tap(&mut f, ESC);
    f.niri_complete_animations();
    assert!(
        !f.niri().window_picker_ui.is_active(),
        "Esc did not close the picker"
    );
}

#[test]
fn picker_toggle_key_closes() {
    let mut f = set_up();

    tap(&mut f, TAB);
    assert!(f.niri().window_picker_ui.is_active(), "picker did not open");

    tap(&mut f, TAB);
    f.niri_complete_animations();
    assert!(
        !f.niri().window_picker_ui.is_active(),
        "second toggle did not close the picker"
    );
}

#[test]
fn picker_arrow_and_enter_selects_window() {
    let mut f = set_up();

    tap(&mut f, TAB);
    assert!(f.niri().window_picker_ui.is_active(), "picker did not open");

    tap(&mut f, RIGHT);
    assert!(f.niri().window_picker_ui.is_active());

    tap(&mut f, ENTER);
    f.niri_complete_animations();
    assert!(
        !f.niri().window_picker_ui.is_active(),
        "Enter did not confirm selection"
    );
}

#[test]
fn picker_search_and_quick_look_work() {
    let mut f = set_up();

    tap(&mut f, TAB);
    assert!(f.niri().window_picker_ui.is_active());

    // Space toggles Quick Look; Escape exits Quick Look without closing the picker.
    tap(&mut f, SPACE);
    assert!(f.niri().window_picker_ui.is_active());
    tap(&mut f, ESC);
    assert!(
        f.niri().window_picker_ui.is_active(),
        "Escape should exit Quick Look before closing picker"
    );

    // `/` enters search mode; Escape exits search mode without closing the picker.
    tap(&mut f, SLASH);
    tap(&mut f, A);
    tap(&mut f, ESC);
    assert!(
        f.niri().window_picker_ui.is_active(),
        "Escape should exit search mode before closing picker"
    );

    // Second Escape closes the picker.
    tap(&mut f, ESC);
    f.niri_complete_animations();
    assert!(!f.niri().window_picker_ui.is_active());
}

#[test]
fn picker_idle_animations_stop_when_glow_pulse_off() {
    let mut f = set_up();

    tap(&mut f, TAB);
    assert!(f.niri().window_picker_ui.is_active());

    f.niri_complete_animations();
    assert!(f.niri().window_picker_ui.is_active());
    assert!(
        !f.niri().window_picker_ui.are_animations_ongoing(),
        "picker should be 0 FPS idle once open animation settles when glow_pulse is false"
    );
}

#[test]
fn picker_scope_workspace_filters_other_workspaces() {
    let config = Config::parse_mem(
        r#"
        binds {
            Tab repeat=false { toggle-window-picker; }
            F2 { focus-workspace-down; }
        }
        window-picker {
            scope "workspace"
            layout "justified"
            group-by "none"
            stagger-ms 800
            animation {
                spring damping-ratio=0.72 stiffness=360 epsilon=0.0001
            }
            motion {
                style "cycle"
            }
        }
        "#,
    )
    .unwrap();

    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    // Window 1 on Workspace 1.
    let w1 = f.client(id).create_window();
    let s1 = w1.surface.clone();
    w1.commit();
    f.roundtrip(id);
    let w1 = f.client(id).window(&s1);
    w1.attach_new_buffer();
    w1.ack_last_and_commit();
    f.roundtrip(id);

    // Switch to Workspace 2 and create Window 2 there.
    tap(&mut f, F2);
    f.niri_complete_animations();

    let w2 = f.client(id).create_window();
    let s2 = w2.surface.clone();
    w2.commit();
    f.roundtrip(id);
    let w2 = f.client(id).window(&s2);
    w2.attach_new_buffer();
    w2.ack_last_and_commit();
    f.roundtrip(id);

    // Open picker: scope="workspace" must show ONLY the 1 window on Workspace 2.
    tap(&mut f, TAB);
    assert!(f.niri().window_picker_ui.is_active());
    assert_eq!(
        f.niri().window_picker_ui.visible_window_count(),
        1,
        "scope=\"workspace\" should only include active workspace windows"
    );

    // Ctrl+Tab cycles scope to All (or Output), showing both windows.
    send(&mut f, LCTRL, true);
    tap(&mut f, TAB);
    send(&mut f, LCTRL, false);
    assert_eq!(
        f.niri().window_picker_ui.visible_window_count(),
        2,
        "Ctrl+Tab should switch scope to include windows on other workspaces"
    );

    f.niri_complete_animations();
    assert!(!f.niri().window_picker_ui.are_animations_ongoing());
}

#[test]
fn picker_cycle_mode_advances_style_on_every_opening() {
    let config = Config::parse_mem(
        r#"
        binds {
            Tab repeat=false { toggle-window-picker; }
        }
        window-picker {
            motion {
                style "cycle"
            }
        }
        "#,
    )
    .unwrap();

    let mut f = Fixture::with_config(config);
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    // Open 11 windows (equal to MOTION_CYCLE.len()) to verify cycle never locks at index 0.
    for _ in 0..11 {
        let w = f.client(id).create_window();
        let s = w.surface.clone();
        w.commit();
        f.roundtrip(id);
        let w = f.client(id).window(&s);
        w.attach_new_buffer();
        w.ack_last_and_commit();
        f.roundtrip(id);
    }

    let mut seen = Vec::new();
    for _ in 0..11 {
        tap(&mut f, TAB);
        assert!(f.niri().window_picker_ui.is_active());
        let styles = f.niri().window_picker_ui.active_motion_styles();
        assert_eq!(styles.len(), 11);
        let first = styles[0];
        assert!(
            styles.iter().all(|&s| s == first),
            "all cards in one opening should share the active cycle style"
        );
        seen.push(first);

        tap(&mut f, ESC);
        f.niri_complete_animations();
        assert!(!f.niri().window_picker_ui.is_active());
    }

    // All 11 openings must cover all 11 distinct motion styles in order, then wrap back.
    let unique: std::collections::HashSet<_> = seen.iter().copied().collect();
    assert_eq!(unique.len(), 11);

    tap(&mut f, TAB);
    assert_eq!(f.niri().window_picker_ui.active_motion_styles()[0], seen[0]);
}
