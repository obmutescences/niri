use knuffel::errors::DecodeError;

use crate::animations::{Animation, Curve, EasingParams, Kind as AnimKind, SpringParams};
use crate::utils::{expect_only_children, parse_arg_node, MergeWith};
use crate::window_picker_card::{
    WindowPickerCard, WindowPickerCardPart, WindowPickerGroupBy, WindowPickerLayout,
    WindowPickerMotion, WindowPickerMotionPart, WindowPickerScope,
};
use crate::{Color, FloatOrInt};

/// Optional easing/spring override for the picker open/close animations.
///
/// ```kdl
/// window-picker {
///     animation {
///         duration-ms 220
///         curve "ease-out-cubic"
///         // or: curve "cubic-bezier" 0.22 1.0 0.36 1.0
///         // or: spring damping-ratio=0.65 stiffness=320 epsilon=0.0001
///         // or: off
///     }
/// }
/// ```
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WindowPickerAnim {
    pub off: bool,
    /// `None`: legacy ease-out-cubic with the per-direction `animation-ms-*` durations.
    pub kind: Option<AnimKind>,
    /// Explicit `duration-ms` set inside `animation {}` (applies to both easing and spring).
    pub explicit_duration_ms: Option<u16>,
}

impl WindowPickerAnim {
    /// Build a concrete `Animation` config, falling back to `fallback_duration_ms` when
    /// `duration-ms` was not explicitly overridden inside `animation {}`.
    pub fn to_animation(&self, fallback_duration_ms: u16) -> Animation {
        if self.off {
            return Animation::new_off();
        }
        let kind = match self.kind {
            Some(AnimKind::Spring(s)) => AnimKind::Spring(s),
            Some(AnimKind::Easing(mut e)) => {
                e.duration_ms = u32::from(
                    self.explicit_duration_ms
                        .unwrap_or(fallback_duration_ms)
                        .max(1),
                );
                AnimKind::Easing(e)
            }
            None => AnimKind::Easing(EasingParams {
                duration_ms: u32::from(fallback_duration_ms.max(1)),
                curve: Curve::EaseOutCubic,
            }),
        };
        Animation { off: false, kind }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct WindowPickerAnimPart {
    pub off: bool,
    pub duration_ms: Option<u16>,
    pub spring: Option<WindowPickerSpringPart>,
    pub curve: Option<Curve>,
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
                "off" => {
                    knuffel::decode::check_flag_node(child, ctx);
                    part.off = true;
                }
                "duration-ms" => {
                    part.duration_ms = Some(parse_arg_node("duration-ms", child, ctx)?);
                }
                "curve" => {
                    let mut iter_args = child.arguments.iter();
                    let val = iter_args.next().ok_or_else(|| {
                        DecodeError::missing(child, "additional argument `curve` is required")
                    })?;
                    let curve_name: String = DecodeScalar::decode(val, ctx)?;
                    let curve = match curve_name.as_str() {
                        "linear" => Some(Curve::Linear),
                        "ease-out-quad" => Some(Curve::EaseOutQuad),
                        "ease-out-cubic" => Some(Curve::EaseOutCubic),
                        "ease-out-expo" => Some(Curve::EaseOutExpo),
                        "cubic-bezier" => {
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(child, "missing x1 for cubic-bezier")
                            })?;
                            let x1: FloatOrInt<0, 1> = DecodeScalar::decode(val, ctx)?;
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(child, "missing y1 for cubic-bezier")
                            })?;
                            let y1: FloatOrInt<{ i32::MIN }, { i32::MAX }> =
                                DecodeScalar::decode(val, ctx)?;
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(child, "missing x2 for cubic-bezier")
                            })?;
                            let x2: FloatOrInt<0, 1> = DecodeScalar::decode(val, ctx)?;
                            let val = iter_args.next().ok_or_else(|| {
                                DecodeError::missing(child, "missing y2 for cubic-bezier")
                            })?;
                            let y2: FloatOrInt<{ i32::MIN }, { i32::MAX }> =
                                DecodeScalar::decode(val, ctx)?;
                            Some(Curve::CubicBezier(x1.0, y1.0, x2.0, y2.0))
                        }
                        other => {
                            ctx.emit_error(DecodeError::unexpected(
                                &val.literal,
                                "argument",
                                format!(
                                    "unexpected animation curve `{other}`. Expected one of: \
                                     `linear`, `ease-out-quad`, `ease-out-cubic`, `ease-out-expo`, \
                                     or `cubic-bezier`."
                                ),
                            ));
                            None
                        }
                    };
                    if let Some(curve) = curve {
                        part.curve = Some(curve);
                    }
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
                            other => {
                                ctx.emit_error(DecodeError::unexpected(
                                    name,
                                    "property",
                                    format!("unexpected spring property `{other}`"),
                                ));
                            }
                        }
                    }
                    part.spring = Some(WindowPickerSpringPart {
                        damping_ratio: damping_ratio.unwrap_or(0.65),
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
        if self.off {
            return WindowPickerAnim {
                off: true,
                kind: None,
                explicit_duration_ms: None,
            };
        }

        let kind = if let Some(spring) = &self.spring {
            Some(AnimKind::Spring(SpringParams {
                damping_ratio: spring.damping_ratio,
                stiffness: spring.stiffness,
                epsilon: spring.epsilon,
            }))
        } else if self.curve.is_some() || self.duration_ms.is_some() {
            let curve = self.curve.unwrap_or(Curve::EaseOutCubic);
            Some(AnimKind::Easing(EasingParams {
                duration_ms: u32::from(self.duration_ms.unwrap_or(180).max(1)),
                curve,
            }))
        } else {
            None
        };
        WindowPickerAnim {
            off: false,
            kind,
            explicit_duration_ms: self.duration_ms,
        }
    }
}

/// Highlight styling for the focused window and the hovered preview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPickerSelection {
    /// Label inversion and glow color for the window that had focus when the picker opened.
    pub focused_color: Color,
    /// Accent color for the hovered / keyboard-selected preview: glow and inverted label.
    pub hover_color: Color,
    /// Soft accent glow behind the hovered / focused preview.
    pub glow: bool,
    /// Continuously breathe the glow while idle (false = 0 FPS idle when static).
    pub glow_pulse: bool,
}

impl Default for WindowPickerSelection {
    fn default() -> Self {
        Self {
            focused_color: Color::from_rgba8_unpremul(115, 218, 202, 255),
            hover_color: Color::from_rgba8_unpremul(115, 218, 202, 255),
            glow: true,
            glow_pulse: false,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct WindowPickerSelectionPart {
    #[knuffel(child)]
    pub focused_color: Option<Color>,
    #[knuffel(child)]
    pub hover_color: Option<Color>,
    #[knuffel(child, unwrap(argument))]
    pub glow: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub glow_pulse: Option<bool>,
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
    /// True when the user explicitly configured `animation-ms-open` in config.
    pub has_explicit_open_ms: bool,
    /// True when the user explicitly configured `animation-ms-close` in config.
    pub has_explicit_close_ms: bool,
    /// Stagger between consecutive preview entry animations (0 disables).
    pub stagger_ms: u16,
    /// Easing/spring override for both open and close animations.
    pub animation: WindowPickerAnim,
    /// Highlight ring for previews matching the active letter filter.
    pub selection: WindowPickerSelection,
    /// Card background and the app metadata strip below the preview.
    pub card: WindowPickerCard,
    /// Entry animation style; all styles share the global open/close duration.
    pub motion: WindowPickerMotion,
    /// Grid packing mode: `grid` or `justified` (equal-height rows).
    pub layout: WindowPickerLayout,
    /// Optional grouping of windows (`none`, `workspace`, `output`).
    pub group_by: WindowPickerGroupBy,
    /// Default scope when opening the picker (`all`, `workspace`, `output`).
    pub scope: WindowPickerScope,
    /// Show the top glass HUD bar with scope selector, search input, and full title.
    pub show_hud: bool,
    /// Left-clicking empty backdrop space closes the picker.
    pub click_backdrop_to_close: bool,
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
            has_explicit_open_ms: false,
            has_explicit_close_ms: false,
            stagger_ms: 28,
            animation: WindowPickerAnim::default(),
            selection: WindowPickerSelection::default(),
            card: WindowPickerCard::default(),
            motion: WindowPickerMotion::default(),
            layout: WindowPickerLayout::default(),
            group_by: WindowPickerGroupBy::default(),
            scope: WindowPickerScope::default(),
            show_hud: true,
            click_backdrop_to_close: true,
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
    #[knuffel(child)]
    pub card: Option<WindowPickerCardPart>,
    #[knuffel(child)]
    pub motion: Option<WindowPickerMotionPart>,
    #[knuffel(child, unwrap(argument))]
    pub layout: Option<WindowPickerLayout>,
    #[knuffel(child, unwrap(argument))]
    pub group_by: Option<WindowPickerGroupBy>,
    #[knuffel(child, unwrap(argument))]
    pub scope: Option<WindowPickerScope>,
    #[knuffel(child, unwrap(argument))]
    pub show_hud: Option<bool>,
    #[knuffel(child, unwrap(argument))]
    pub click_backdrop_to_close: Option<bool>,
}

impl WindowPicker {
    /// Returns the explicit target total duration (in ms) for opening/closing if configured via
    /// `animation { duration-ms ... }` or top-level `animation-ms-open` / `animation-ms-close`.
    /// When `None` is returned in Spring mode, the spring runs for its natural physical duration.
    pub fn target_duration_ms(&self, open: bool) -> Option<u16> {
        if let Some(ms) = self.animation.explicit_duration_ms {
            return Some(ms);
        }
        if open && self.has_explicit_open_ms {
            return Some(self.animation_ms_open);
        }
        if !open && self.has_explicit_close_ms {
            return Some(self.animation_ms_close);
        }
        None
    }
}

impl MergeWith<WindowPickerPart> for WindowPicker {
    fn merge_with(&mut self, part: &WindowPickerPart) {
        merge!((self, part), area_width, area_height, max_scale, gap);
        merge!((self, part), label, backdrop);
        if part.animation_ms_open.is_some() {
            self.has_explicit_open_ms = true;
        }
        if part.animation_ms_close.is_some() {
            self.has_explicit_close_ms = true;
        }
        merge_clone!(
            (self, part),
            animation_ms_open,
            animation_ms_close,
            stagger_ms,
            layout,
            group_by,
            scope,
            show_hud,
            click_backdrop_to_close,
        );
        if let Some(anim) = &part.animation {
            self.animation = anim.into_override();
        }
        if let Some(selection) = &part.selection {
            merge_clone!(
                (self.selection, selection),
                focused_color,
                hover_color,
                glow,
                glow_pulse,
            );
        }
        if let Some(card) = &part.card {
            self.card.merge_with(card);
        }
        if let Some(motion) = &part.motion {
            self.motion.merge_with(motion);
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
    /// Letters used for single-letter labels, in order. Empty means A–Z then AA–ZZ.
    pub letters: String,
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
            letters: String::new(),
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
    /// Custom single-letter alphabet. If it cannot cover all windows, labels
    /// switch to random two-letter combinations.
    #[knuffel(child, unwrap(argument))]
    pub letters: Option<String>,
}

impl MergeWith<WindowPickerLabelPart> for WindowPickerLabel {
    fn merge_with(&mut self, part: &WindowPickerLabelPart) {
        merge_clone!(
            (self, part),
            font,
            text_color,
            background_color,
            border_color,
            letters
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
        assert_eq!(bind.press_action(), Some(&Action::ToggleWindowPicker));
        assert!(!bind.repeat);
    }

    #[test]
    fn parse_window_picker_enhanced_options_and_validation() {
        let config = Config::parse_mem(
            r##"
            window-picker {
                layout "justified"
                group-by "workspace"
                scope "output"
                show-hud false
                click-backdrop-to-close false
                animation-ms-open 240
                animation-ms-close 150
                motion {
                    style "hero"
                }
                animation {
                    curve "ease-out-expo"
                }
                selection {
                    glow true
                    glow-pulse true
                }
                card {
                    tilt false
                    show-icons false
                    glass-border "#ffffff44"
                }
            }
            "##,
        )
        .unwrap();

        let wp = &config.window_picker;
        assert_eq!(wp.layout, crate::WindowPickerLayout::Justified);
        assert_eq!(wp.group_by, crate::WindowPickerGroupBy::Workspace);
        assert_eq!(wp.scope, crate::WindowPickerScope::Output);
        assert!(!wp.show_hud);
        assert!(!wp.click_backdrop_to_close);
        assert_eq!(wp.motion.style, crate::WindowPickerMotionStyle::Hero);
        assert!(wp.selection.glow_pulse);
        assert!(!wp.card.tilt);
        assert!(!wp.card.show_icons);

        // Curve without explicit duration-ms respects per-direction animation-ms-* fallbacks!
        let open_anim = wp.animation.to_animation(wp.animation_ms_open);
        let close_anim = wp.animation.to_animation(wp.animation_ms_close);
        assert_eq!(
            open_anim.kind,
            crate::animations::Kind::Easing(crate::animations::EasingParams {
                duration_ms: 240,
                curve: crate::animations::Curve::EaseOutExpo,
            })
        );
        assert_eq!(
            close_anim.kind,
            crate::animations::Kind::Easing(crate::animations::EasingParams {
                duration_ms: 150,
                curve: crate::animations::Curve::EaseOutExpo,
            })
        );

        // Invalid motion style and invalid curve must emit parse errors instead of silent fallback.
        assert!(
            Config::parse_mem(r#"window-picker { motion { style "nonexistent"; }; }"#).is_err()
        );
        assert!(Config::parse_mem(r#"window-picker { animation { curve "unknown"; }; }"#).is_err());
    }
}
