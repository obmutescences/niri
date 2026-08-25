use std::ffi::OsString;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use niri_config::Sounds;

use crate::utils::spawning::spawn;

const SAME_KIND_COOLDOWN: Duration = Duration::from_millis(50);
const FOCUS_SUPPRESS_WINDOW: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    WindowOpen,
    WindowClose,
    FocusChange,
    WorkspaceSwitch,
}

impl Kind {
    fn index(self) -> usize {
        match self {
            Kind::WindowOpen => 0,
            Kind::WindowClose => 1,
            Kind::FocusChange => 2,
            Kind::WorkspaceSwitch => 3,
        }
    }

    fn is_focus_change(self) -> bool {
        self == Kind::FocusChange
    }
}

#[derive(Default)]
struct Cooldowns {
    last_non_focus: Option<Instant>,
    last_per_kind: [Option<Instant>; 4],
}

static COOLDOWNS: Mutex<Cooldowns> = Mutex::new(Cooldowns {
    last_non_focus: None,
    last_per_kind: [None; 4],
});

pub fn play(sounds: &Sounds, kind: Kind) {
    if !sounds.on {
        return;
    }

    let path = match kind {
        Kind::WindowOpen => sounds.window_open.as_deref(),
        Kind::WindowClose => sounds.window_close.as_deref(),
        Kind::FocusChange => sounds.focus_change.as_deref(),
        Kind::WorkspaceSwitch => sounds.workspace_switch.as_deref(),
    };
    let Some(path) = path else { return };

    let now = Instant::now();
    {
        let mut cooldowns = COOLDOWNS.lock().unwrap();

        // Opening a focused window or closing the focused window fires both the open/close and
        // the focus change events; skip the focus sound if another sound just played.
        if kind.is_focus_change() {
            if let Some(last) = cooldowns.last_non_focus {
                if now.duration_since(last) < FOCUS_SUPPRESS_WINDOW {
                    return;
                }
            }
        }

        let last_kind = &mut cooldowns.last_per_kind[kind.index()];
        if let Some(last) = *last_kind {
            if now.duration_since(last) < SAME_KIND_COOLDOWN {
                return;
            }
        }

        *last_kind = Some(now);
        if !kind.is_focus_change() {
            cooldowns.last_non_focus = Some(now);
        }
    }

    spawn(
        vec![OsString::from("pw-play"), path.as_os_str().to_os_string()],
        None,
    );
}
