use crate::utils::MergeWith;
use crate::window_picker::{WindowPickerBlur, WindowPickerBlurPart};
use crate::{Color, FloatOrInt};

/// Entry animation style for the window picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowPickerMotionStyle {
    Pop,
    #[default]
    Slide,
    Drop,
    Burst,
    Vortex,
    Flip,
    Cascade,
    Orbit,
    Wave,
    Zoom,
    /// Walk through all styles in order, one style per card.
    Cycle,
}

impl WindowPickerMotionStyle {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "pop" => Some(Self::Pop),
            "slide" => Some(Self::Slide),
            "drop" => Some(Self::Drop),
            "burst" => Some(Self::Burst),
            "vortex" => Some(Self::Vortex),
            "flip" => Some(Self::Flip),
            "cascade" => Some(Self::Cascade),
            "orbit" => Some(Self::Orbit),
            "wave" => Some(Self::Wave),
            "zoom" => Some(Self::Zoom),
            // "random" is kept as an alias so existing configs keep working.
            "cycle" | "random" => Some(Self::Cycle),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowPickerMotion {
    pub style: WindowPickerMotionStyle,
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct WindowPickerMotionPart {
    #[knuffel(child, unwrap(argument))]
    pub style: Option<String>,
}

impl MergeWith<WindowPickerMotionPart> for WindowPickerMotion {
    fn merge_with(&mut self, part: &WindowPickerMotionPart) {
        if let Some(style) = &part.style {
            if let Some(style) = WindowPickerMotionStyle::from_name(style) {
                self.style = style;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowPickerInfo {
    pub font: String,
    pub title_size: f64,
    pub app_id_size: f64,
    pub workspace_size: f64,
    pub title_color: Color,
    pub app_id_color: Color,
    pub workspace_color: Color,
    pub padding_x: f64,
    pub padding_y: f64,
    /// Height of the info strip below the window preview.
    pub height: f64,
}

impl Default for WindowPickerInfo {
    fn default() -> Self {
        Self {
            font: String::from("Sans"),
            title_size: 13.5,
            app_id_size: 10.,
            workspace_size: 9.5,
            title_color: Color::from_rgba8_unpremul(246, 251, 252, 255),
            app_id_color: Color::from_rgba8_unpremul(214, 232, 238, 150),
            workspace_color: Color::from_rgba8_unpremul(232, 247, 249, 200),
            padding_x: 12.,
            padding_y: 9.,
            height: 58.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct WindowPickerInfoPart {
    #[knuffel(child, unwrap(argument))]
    pub font: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub title_size: Option<FloatOrInt<1, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub app_id_size: Option<FloatOrInt<1, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub workspace_size: Option<FloatOrInt<1, 65535>>,
    #[knuffel(child)]
    pub title_color: Option<Color>,
    #[knuffel(child)]
    pub app_id_color: Option<Color>,
    #[knuffel(child)]
    pub workspace_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub padding_x: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub padding_y: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub height: Option<FloatOrInt<0, 65535>>,
}

impl MergeWith<WindowPickerInfoPart> for WindowPickerInfo {
    fn merge_with(&mut self, part: &WindowPickerInfoPart) {
        merge_clone!(
            (self, part),
            font,
            title_color,
            app_id_color,
            workspace_color,
        );
        merge!(
            (self, part),
            title_size,
            app_id_size,
            workspace_size,
            padding_x,
            padding_y,
            height
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowPickerCard {
    pub on: bool,
    pub corner_radius: f64,
    pub shadow: bool,
    pub info: WindowPickerInfo,
    pub blur: WindowPickerBlur,
}

impl Default for WindowPickerCard {
    fn default() -> Self {
        Self {
            on: true,
            corner_radius: 14.,
            shadow: true,
            info: WindowPickerInfo::default(),
            blur: WindowPickerBlur {
                on: true,
                passes: 2,
                offset: 2.,
            },
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct WindowPickerCardPart {
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child, unwrap(argument))]
    pub corner_radius: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub shadow: Option<bool>,
    #[knuffel(child)]
    pub info: Option<WindowPickerInfoPart>,
    #[knuffel(child)]
    pub blur: Option<WindowPickerBlurPart>,
}

impl MergeWith<WindowPickerCardPart> for WindowPickerCard {
    fn merge_with(&mut self, part: &WindowPickerCardPart) {
        self.on |= part.on;
        if part.off {
            self.on = false;
        }

        merge!((self, part), corner_radius);
        if let Some(shadow) = part.shadow {
            self.shadow = shadow;
        }

        if let Some(info) = &part.info {
            self.info.merge_with(info);
        }
        if let Some(blur) = &part.blur {
            self.blur.merge_with(blur);
        }
    }
}
