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
const A: Keycode = Keycode::new(30 + 8);

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
