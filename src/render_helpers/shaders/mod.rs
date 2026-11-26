use std::cell::RefCell;

use glam::Mat3;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
    UniformValue,
};

use super::renderer::NiriRenderer;
use super::shader_element::ShaderProgram;
use crate::render_helpers::blur::BlurProgram;

pub struct Shaders {
    pub border: Option<ShaderProgram>,
    pub shadow: Option<ShaderProgram>,
    pub picker_card: Option<ShaderProgram>,
    pub picker_glass: Option<ShaderProgram>,
    pub picker_blur: Option<GlesTexProgram>,
    pub clipped_surface: Option<GlesTexProgram>,
    pub postprocess_and_clip: Option<GlesTexProgram>,
    pub resize: Option<ShaderProgram>,
    pub gradient_fade: Option<GlesTexProgram>,
    pub blur: Option<BlurProgram>,
    pub custom_resize: RefCell<Option<ShaderProgram>>,
    pub custom_close: RefCell<Option<ShaderProgram>>,
    pub custom_open: RefCell<Option<ShaderProgram>>,
}

#[derive(Debug, Clone, Copy)]
pub enum ProgramType {
    Border,
    Shadow,
    PickerCard,
    PickerGlass,
    Resize,
    Close,
    Open,
}

impl Shaders {
    fn compile(renderer: &mut GlesRenderer) -> Self {
        let _span = tracy_client::span!("Shaders::compile");

        let border = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("border.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("colorspace", UniformType::_1f),
                UniformName::new("hue_interpolation", UniformType::_1f),
                UniformName::new("color_from", UniformType::_4f),
                UniformName::new("color_to", UniformType::_4f),
                UniformName::new("grad_offset", UniformType::_2f),
                UniformName::new("grad_width", UniformType::_1f),
                UniformName::new("grad_vec", UniformType::_2f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("outer_radius", UniformType::_4f),
                UniformName::new("border_width", UniformType::_1f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling border shader: {err:?}");
        })
        .ok();

        let shadow = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("shadow.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("shadow_color", UniformType::_4f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("window_input_to_geo", UniformType::Matrix3x3),
                UniformName::new("window_geo_size", UniformType::_2f),
                UniformName::new("window_corner_radius", UniformType::_4f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling shadow shader: {err:?}");
        })
        .ok();

        let picker_card = ShaderProgram::compile(
            renderer,
            include_str!("picker_card.frag"),
            &[
                UniformName::new("src_size", UniformType::_2f),
                UniformName::new("tex_size", UniformType::_2f),
                UniformName::new("src_pivot", UniformType::_2f),
                UniformName::new("dest_size", UniformType::_2f),
                UniformName::new("dest_center", UniformType::_2f),
                UniformName::new("persp_half_size", UniformType::_2f),
                UniformName::new("card_half_size", UniformType::_2f),
                UniformName::new("angle", UniformType::_1f),
                UniformName::new("card_scale", UniformType::_1f),
                UniformName::new("tilt", UniformType::_2f),
                UniformName::new("flip_y", UniformType::_1f),
                UniformName::new("glint_strength", UniformType::_1f),
            ],
            &["tex"],
        )
        .map_err(|err| {
            warn!("error compiling picker card shader: {err:?}");
        })
        .ok();

        let picker_blur = renderer
            .compile_custom_texture_shader(
                include_str!("picker_blur.frag"),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                    UniformName::new("pb_card_size", UniformType::_2f),
                    UniformName::new("pb_dest_center", UniformType::_2f),
                    UniformName::new("pb_persp_half_size", UniformType::_2f),
                    UniformName::new("pb_angle", UniformType::_1f),
                    UniformName::new("pb_card_scale", UniformType::_1f),
                    UniformName::new("pb_tilt", UniformType::_2f),
                    UniformName::new("pb_flip_y", UniformType::_1f),
                ],
            )
            .map_err(|err| {
                warn!("error compiling picker blur shader: {err:?}");
            })
            .ok();

        let picker_glass = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("picker_glass.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("fill_color", UniformType::_4f),
                UniformName::new("border_color", UniformType::_4f),
                UniformName::new("accent_color", UniformType::_4f),
                UniformName::new("highlight", UniformType::_1f),
                UniformName::new("divider_y", UniformType::_1f),
                UniformName::new("scrim_alpha", UniformType::_1f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling picker glass shader: {err:?}");
        })
        .ok();

        let clipped_surface = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("clipped_surface.frag"),
                    include_str!("rounding_alpha.frag"),
                ),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                ],
            )
            .map_err(|err| {
                warn!("error compiling clipped surface shader: {err:?}");
            })
            .ok();

        let postprocess_and_clip = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("glass_background.frag"),
                    include_str!("rounding_alpha.frag"),
                ),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                    UniformName::new("geo_to_tex_linear", UniformType::_4f),
                    UniformName::new("tex_sharp", UniformType::_1i),
                    UniformName::new("has_sharp_tex", UniformType::_1f),
                    UniformName::new("noise", UniformType::_1f),
                    UniformName::new("saturation", UniformType::_1f),
                    UniformName::new("bg_color", UniformType::_4f),
                    UniformName::new("tint_from", UniformType::_4f),
                    UniformName::new("tint_to", UniformType::_4f),
                    UniformName::new("tint_angle", UniformType::_1f),
                    UniformName::new("tint_mode", UniformType::_1f),
                    UniformName::new("tint_opacity", UniformType::_1f),
                    UniformName::new("tint_linear_size", UniformType::_1f),
                    UniformName::new("tint_linear_depth", UniformType::_1f),
                    UniformName::new("tint_radial_size", UniformType::_1f),
                    UniformName::new("tint_radial_depth", UniformType::_1f),
                    UniformName::new("tint_oklab", UniformType::_1f),
                    UniformName::new("lg_enabled", UniformType::_1f),
                    UniformName::new("lg_refraction_strength", UniformType::_1f),
                    UniformName::new("lg_power_factor", UniformType::_1f),
                    UniformName::new("lg_refraction_power", UniformType::_1f),
                    UniformName::new("lg_physical_refraction", UniformType::_1f),
                    UniformName::new("lg_glow_weight", UniformType::_1f),
                    UniformName::new("lg_edge_lighting", UniformType::_1f),
                    UniformName::new("lg_fringing", UniformType::_1f),
                    UniformName::new("lg_bevel_width", UniformType::_1f),
                    UniformName::new("lg_bevel_px", UniformType::_1f),
                    UniformName::new("lg_interior_warp", UniformType::_1f),
                    UniformName::new("lg_rim_color", UniformType::_4f),
                    UniformName::new("lg_rim_strength", UniformType::_1f),
                    UniformName::new("lg_inner_shadow", UniformType::_1f),
                    UniformName::new("lg_sheen_color", UniformType::_4f),
                    UniformName::new("lg_sheen_strength", UniformType::_1f),
                    UniformName::new("lg_ior", UniformType::_1f),
                    UniformName::new("lg_thickness", UniformType::_1f),
                    UniformName::new("lg_bevel_clarity", UniformType::_1f),
                    UniformName::new("lg_dispersion", UniformType::_1f),
                    UniformName::new("lg_adaptive_contrast", UniformType::_1f),
                    UniformName::new("lg_cursor_glow", UniformType::_1f),
                    UniformName::new("lg_cursor_pos", UniformType::_2f),
                    UniformName::new("lg_focus_shimmer", UniformType::_1f),
                    UniformName::new("lg_focus_progress", UniformType::_1f),
                ],
            )
            .map_err(|err| {
                warn!("error compiling postprocess_and_clip shader: {err:?}");
            })
            .ok();

        let resize = compile_resize_program(renderer, include_str!("resize.frag"))
            .map_err(|err| {
                warn!("error compiling resize shader: {err:?}");
            })
            .ok();

        let gradient_fade = renderer
            .compile_custom_texture_shader(
                include_str!("gradient_fade.frag"),
                &[UniformName::new("cutoff", UniformType::_2f)],
            )
            .map_err(|err| {
                warn!("error compiling gradient fade shader: {err:?}");
            })
            .ok();

        let blur = BlurProgram::compile(renderer)
            .map_err(|err| {
                warn!("error compiling blur shaders: {err:?}");
            })
            .ok();

        Self {
            border,
            shadow,
            picker_card,
            picker_glass,
            picker_blur,
            clipped_surface,
            postprocess_and_clip,
            resize,
            gradient_fade,
            blur,
            custom_resize: RefCell::new(None),
            custom_close: RefCell::new(None),
            custom_open: RefCell::new(None),
        }
    }

    pub fn get_from_frame<'a>(frame: &'a mut GlesFrame<'_, '_>) -> &'a Self {
        let data = frame.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn get(renderer: &mut impl NiriRenderer) -> &Self {
        let renderer = renderer.as_gles_renderer();
        let data = renderer.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn replace_custom_resize_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_resize.replace(program)
    }

    pub fn replace_custom_close_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_close.replace(program)
    }

    pub fn replace_custom_open_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_open.replace(program)
    }

    pub fn program(&self, program: ProgramType) -> Option<ShaderProgram> {
        match program {
            ProgramType::Border => self.border.clone(),
            ProgramType::Shadow => self.shadow.clone(),
            ProgramType::PickerCard => self.picker_card.clone(),
            ProgramType::PickerGlass => self.picker_glass.clone(),
            ProgramType::Resize => self
                .custom_resize
                .borrow()
                .clone()
                .or_else(|| self.resize.clone()),
            ProgramType::Close => self.custom_close.borrow().clone(),
            ProgramType::Open => self.custom_open.borrow().clone(),
        }
    }
}

pub fn init(renderer: &mut GlesRenderer) {
    let shaders = Shaders::compile(renderer);
    let data = renderer.egl_context().user_data();
    if !data.insert_if_missing(|| shaders) {
        error!("shaders were already compiled");
    }
}

fn compile_resize_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("resize_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("resize_epilogue.frag"));
    program.push_str(include_str!("rounding_alpha.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_curr_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_prev_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_next_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex_prev", UniformType::Matrix3x3),
            UniformName::new("niri_geo_to_tex_next", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_corner_radius", UniformType::_4f),
            UniformName::new("niri_clip_to_geometry", UniformType::_1f),
        ],
        &["niri_tex_prev", "niri_tex_next"],
    )
}

pub fn set_custom_resize_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_resize_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom resize shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_resize_program(program) {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom resize shader: {err:?}");
        }
    }
}

fn compile_close_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("close_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("close_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ],
        &["niri_tex"],
    )
}

pub fn set_custom_close_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_close_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom close shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_close_program(program) {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom close shader: {err:?}");
        }
    }
}

fn compile_open_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("open_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("open_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ],
        &["niri_tex"],
    )
}

pub fn set_custom_open_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_open_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom open shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_open_program(program) {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom open shader: {err:?}");
        }
    }
}

pub fn mat3_uniform(name: &str, mat: Mat3) -> Uniform<'_> {
    Uniform::new(
        name,
        UniformValue::Matrix3x3 {
            matrices: vec![mat.to_cols_array()],
            transpose: false,
        },
    )
}
