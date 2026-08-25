use std::path::PathBuf;

use crate::utils::MergeWith;

#[derive(Debug, Clone, PartialEq)]
pub struct Sounds {
    pub on: bool,
    pub window_open: Option<PathBuf>,
    pub window_close: Option<PathBuf>,
    pub focus_change: Option<PathBuf>,
    pub workspace_switch: Option<PathBuf>,
}

impl Default for Sounds {
    fn default() -> Self {
        Sounds {
            on: true,
            window_open: None,
            window_close: None,
            focus_change: None,
            workspace_switch: None,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, PartialEq)]
pub struct SoundsPart {
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child, unwrap(argument))]
    pub window_open: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub window_close: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub focus_change: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub workspace_switch: Option<String>,
}

impl MergeWith<SoundsPart> for Sounds {
    fn merge_with(&mut self, part: &SoundsPart) {
        self.on |= part.on;
        if part.off {
            self.on = false;
        }

        if let Some(x) = &part.window_open {
            self.window_open = Some(PathBuf::from(x));
        }
        if let Some(x) = &part.window_close {
            self.window_close = Some(PathBuf::from(x));
        }
        if let Some(x) = &part.focus_change {
            self.focus_change = Some(PathBuf::from(x));
        }
        if let Some(x) = &part.workspace_switch {
            self.workspace_switch = Some(PathBuf::from(x));
        }
    }
}
