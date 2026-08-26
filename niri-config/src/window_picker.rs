use knuffel::errors::DecodeError;

use crate::animations::{Animation, Curve, EasingParams, Kind as AnimKind, SpringParams};
use crate::utils::{expect_only_children, parse_arg_node, MergeWith};
use crate::{Color, FloatOrInt};

/// Optional easing/spring override for the picker open/close animations.
///
/// ```kdl
/// window-picker {
///     animation {
///         duration-ms 220
///         curve ease-out-cubic
///         // or: spring damping-ratio=0.6 stiffness=320 epsilon=0.0001
///     }
/// }
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WindowPickerAnim {
    /// `None`: legacy ease-out-cubic with the per-direction `animation-ms-*` durations.
    pub kind: Option<AnimKind>,
}

impl WindowPickerAnim {
    /// Build a concrete `Animation` config, falling back to ease-out-cubic with the given
    /// per-direction default duration (matching the legacy fixed behavior).
    pub fn to_animation(&self, fallback_duration_ms: u16) -> Animation {
        let kind = self.kind.unwrap_or_else(|| {
            AnimKind::Easing(EasingParams {
                duration_ms: u32::from(fallback_duration_ms.max(1)),
                curve: Curve::EaseOutCubic,
            })
        });
        Animation {
            off: false,
            kind,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct WindowPickerAnimPart {
    pub duration_ms: Option<u16>,
    pub spring: Option<WindowPickerSpringPart>,
    pub curve: Option<String>,
}

impl<S> knuffel::Decode<S> for WindowPickerAnimPart
where
    S: knuffel::traits::ErrorSpan,
{
    fn decode_node(
        node: &knuffel::ast::SpannedNode<S>,
        ctx: &mut knuffel::decode::Context<S>,
    ) -> Result<Self, DecodeError<S>> {
        use knuffel::traits::DecodeScalar;
        expect_only_children(node, ctx);

        let mut part = Self::default();

        for child in node.children() {
            match &**child.node_name {
                "duration-ms" => {
                    part.duration_ms = Some(parse_arg_node("duration-ms", child, ctx)?);
                }
                "curve" => {
                    let mut iter_args = child.arguments.iter();
                    let val = iter_args.next().ok_or_else(|| {
                        DecodeError::missing(child, "additional argument `curve` is required")
                    })?;
                    part.curve = Some(DecodeScalar::decode(val, ctx)?);
                }
                "spring" => {
                    let mut damping_ratio = None;
                    let mut stiffness = None;
                    let mut epsilon = None;
                    for (name, val) in &child.properties {
                        match &***name {
                            "damping-ratio" => {
                                damping_ratio = Some(DecodeScalar::decode(val, ctx)?);
                            }
                            "stiffness" => stiffness = Some(DecodeScalar::decode(val, ctx)?),
                            "epsilon" => epsilon = Some(DecodeScalar::decode(val, ctx)?),
                            _ => (),
                        }
                    }
                    part.spring = Some(WindowPickerSpringPart {
                        damping_ratio: damping_ratio.unwrap_or(0.6),
                        stiffness: stiffness.unwrap_or(320),
                        epsilon: epsilon.unwrap_or(0.0001),
                    });
                }
                _ => {
                    ctx.emit_error(DecodeError::unexpected(
                        child,
                        "node",
                        format!("unexpected node `{}`", child.node_name.escape_default()),
                    ));
                }
            }
        }

        Ok(part)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPickerSpringPart {
    pub damping_ratio: f64,
    pub stiffness: u32,
    pub epsilon: f64,
}

impl WindowPickerAnimPart {
    pub fn into_override(&self) -> WindowPickerAnim {
        let kind = if let Some(spring) = &self.spring {
            Some(AnimKind::Spring(SpringParams {
                damping_ratio: spring.damping_ratio,
                stiffness: spring.stiffness,
                epsilon: spring.epsilon,
            }))
        } else if self.curve.is_some() || self.duration_ms.is_some() {
            let curve_owned = self.curve.clone();
            let curve = match curve_owned.as_deref() {
                Some("linear") => Curve::Linear,
                Some("ease-out-quad") => Curve::EaseOutQuad,
                Some("ease-out-expo") => Curve::EaseOutExpo,
                _ => Curve::EaseOutCubic,
            };
            Some(AnimKind::Easing(EasingParams {
                duration_ms: u32::from(self.duration_ms.unwrap_or(180)),
                curve,
            }))
        } else {
            None
        };
        WindowPickerAnim { kind }
    }
}

/// Highlight ring drawn behind matching previews while a letter filter is active.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPickerSelection {
    pub color: Color,
    pub width: f64,
}

impl Default for WindowPickerSelection {
    fn default() -> Self {
        Self {
            color: Color::from_rgba8_unpremul(115, 218, 202, 110),
            width: 26.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct WindowPickerSelectionPart {
    #[knuffel(child)]
    pub active_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub width: Option<FloatOrInt<2, 200>>,
}


/// Configuration for the keyboard-driven window picker.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowPicker {
    pub area_width: f64,
    pub area_height: f64,
    pub max_scale: f64,
    pub gap: f64,
    pub label: WindowPickerLabel,
    pub backdrop: WindowPickerBackdrop,
    pub animation_ms_open: u16,
    pub animation_ms_close: u16,
    /// Stagger between consecutive preview entry animations (0 disables).
    pub stagger_ms: u16,
    /// Easing/spring override for both open and close animations.
    pub animation: WindowPickerAnim,
    /// Highlight ring for previews matching the active letter filter.
    pub selection: WindowPickerSelection,
}

impl Default for WindowPicker {
    fn default() -> Self {
        Self {
            area_width: 0.8,
            area_height: 0.8,
            max_scale: 0.65,
            gap: 24.,
            label: WindowPickerLabel::default(),
            backdrop: WindowPickerBackdrop::default(),
            animation_ms_open: 180,
            animation_ms_close: 180,
            stagger_ms: 28,
            animation: WindowPickerAnim::default(),
            selection: WindowPickerSelection::default(),
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct WindowPickerPart {
    #[knuffel(child, unwrap(argument))]
    pub area_width: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub area_height: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub max_scale: Option<FloatOrInt<0, 1>>,
    #[knuffel(child, unwrap(argument))]
    pub gap: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child)]
    pub label: Option<WindowPickerLabelPart>,
    #[knuffel(child)]
    pub backdrop: Option<WindowPickerBackdropPart>,
    #[knuffel(child, unwrap(argument))]
    pub animation_ms_open: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub animation_ms_close: Option<u16>,
    #[knuffel(child, unwrap(argument))]
    pub stagger_ms: Option<u16>,
    #[knuffel(child)]
    pub animation: Option<WindowPickerAnimPart>,
    #[knuffel(child)]
    pub selection: Option<WindowPickerSelectionPart>,
}

impl MergeWith<WindowPickerPart> for WindowPicker {
    fn merge_with(&mut self, part: &WindowPickerPart) {
        merge!((self, part), area_width, area_height, max_scale, gap);
        merge!((self, part), label, backdrop);
        merge_clone!((self, part), animation_ms_open);
        merge_clone!((self, part), animation_ms_close);
        merge_clone!((self, part), stagger_ms);
        if let Some(anim) = &part.animation {
            self.animation = anim.into_override();
        }
        if let Some(selection) = &part.selection {
            if let Some(color) = selection.active_color {
                self.selection.color = color;
            }
            if let Some(width) = selection.width {
                self.selection.width = width.0;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowPickerLabel {
    pub font: String,
    pub size: f64,
    pub text_color: Color,
    pub background_color: Color,
    pub border_color: Color,
    pub padding_x: f64,
    pub padding_y: f64,
    pub gap: f64,
    pub corner_radius: f64,
}

impl Default for WindowPickerLabel {
    fn default() -> Self {
        Self {
            font: String::from("Sans Bold"),
            size: 24.,
            text_color: Color::from_rgba8_unpremul(255, 255, 255, 255),
            background_color: Color::from_rgba8_unpremul(22, 26, 34, 230),
            border_color: Color::from_rgba8_unpremul(115, 218, 202, 255),
            padding_x: 14.,
            padding_y: 7.,
            gap: 12.,
            corner_radius: 8.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, PartialEq)]
pub struct WindowPickerLabelPart {
    #[knuffel(child, unwrap(argument))]
    pub font: Option<String>,
    #[knuffel(child, unwrap(argument))]
    pub size: Option<FloatOrInt<1, 65535>>,
    #[knuffel(child)]
    pub text_color: Option<Color>,
    #[knuffel(child)]
    pub background_color: Option<Color>,
    #[knuffel(child)]
    pub border_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub padding_x: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub padding_y: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub gap: Option<FloatOrInt<0, 65535>>,
    #[knuffel(child, unwrap(argument))]
    pub corner_radius: Option<FloatOrInt<0, 65535>>,
}

impl MergeWith<WindowPickerLabelPart> for WindowPickerLabel {
    fn merge_with(&mut self, part: &WindowPickerLabelPart) {
        merge_clone!(
            (self, part),
            font,
            text_color,
            background_color,
            border_color
        );
        merge!((self, part), size, padding_x, padding_y, gap, corner_radius,);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPickerBackdrop {
    pub brightness: f64,
    pub color: Color,
    pub saturation: f64,
    pub blur: WindowPickerBlur,
}

impl Default for WindowPickerBackdrop {
    fn default() -> Self {
        Self {
            brightness: 0.55,
            color: Color::from_rgba8_unpremul(16, 19, 26, 51),
            saturation: 0.85,
            blur: WindowPickerBlur::default(),
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct WindowPickerBackdropPart {
    #[knuffel(child, unwrap(argument))]
    pub brightness: Option<FloatOrInt<0, 1>>,
    #[knuffel(child)]
    pub color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub saturation: Option<FloatOrInt<0, 1000>>,
    #[knuffel(child)]
    pub blur: Option<WindowPickerBlurPart>,
}

impl MergeWith<WindowPickerBackdropPart> for WindowPickerBackdrop {
    fn merge_with(&mut self, part: &WindowPickerBackdropPart) {
        merge!((self, part), brightness, saturation, blur);
        merge_clone!((self, part), color);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPickerBlur {
    pub on: bool,
    pub passes: u8,
    pub offset: f64,
}

impl Default for WindowPickerBlur {
    fn default() -> Self {
        Self {
            on: true,
            passes: 3,
            offset: 3.,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct WindowPickerBlurPart {
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child, unwrap(argument))]
    pub passes: Option<u8>,
    #[knuffel(child, unwrap(argument))]
    pub offset: Option<FloatOrInt<0, 100>>,
}

impl MergeWith<WindowPickerBlurPart> for WindowPickerBlur {
    fn merge_with(&mut self, part: &WindowPickerBlurPart) {
        self.on |= part.on;
        if part.off {
            self.on = false;
        }

        merge_clone!((self, part), passes);
        merge!((self, part), offset);
    }
}

#[cfg(test)]
mod tests {
    use smithay::input::keyboard::Keysym;

    use crate::{Action, Config, Trigger};

    #[test]
    fn parse_window_picker() {
        let config = Config::parse_mem(
            r##"
            window-picker {
                area-width 0.7
                area-height 0.75
                max-scale 0.5
                gap 18

                label {
                    font "Monospace Bold"
                    size 30
                    text-color "#112233"
                    background-color "#44556677"
                    border-color "#8899aabb"
                    padding-x 16
                    padding-y 9
                    gap 14
                    corner-radius 6
                }

                backdrop {
                    brightness 0.4
                    color "#01020344"
                    saturation 0.7
                    blur {
                        off
                        passes 5
                        offset 4
                    }
                }

                animation-ms-open 220
                animation-ms-close 350
            }
            "##,
        )
        .unwrap();

        assert_eq!(config.window_picker.area_width, 0.7);
        assert_eq!(config.window_picker.area_height, 0.75);
        assert_eq!(config.window_picker.max_scale, 0.5);
        assert_eq!(config.window_picker.gap, 18.);
        assert_eq!(config.window_picker.label.font, "Monospace Bold");
        assert_eq!(config.window_picker.label.size, 30.);
        assert_eq!(config.window_picker.label.padding_x, 16.);
        assert_eq!(config.window_picker.label.padding_y, 9.);
        assert_eq!(config.window_picker.label.gap, 14.);
        assert_eq!(config.window_picker.label.corner_radius, 6.);
        assert_eq!(config.window_picker.backdrop.brightness, 0.4);
        assert_eq!(config.window_picker.backdrop.saturation, 0.7);
        assert!(!config.window_picker.backdrop.blur.on);
        assert_eq!(config.window_picker.backdrop.blur.passes, 5);
        assert_eq!(config.window_picker.backdrop.blur.offset, 4.);
        assert_eq!(config.window_picker.animation_ms_open, 220);
        assert_eq!(config.window_picker.animation_ms_close, 350);
    }

    #[test]
    fn parse_toggle_binding_without_repeat() {
        let config = Config::parse_mem(
            r#"
            binds {
                Mod+Tab repeat=false { toggle-window-picker; }
            }
            "#,
        )
        .unwrap();

        let bind = config.binds.0.first().unwrap();
        assert_eq!(bind.key.trigger, Trigger::Keysym(Keysym::Tab));
        assert_eq!(bind.action, Action::ToggleWindowPicker);
        assert!(!bind.repeat);
    }
}
