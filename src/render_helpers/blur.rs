use std::cmp::max;
use std::iter::{once, zip};
use std::rc::Rc;

use anyhow::{ensure, Context as _};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{ffi, link_program, GlesError, GlesRenderer, GlesTexture};
use smithay::backend::renderer::{ContextId, Renderer as _, Texture as _};
use smithay::gpu_span_location;
use smithay::utils::{Buffer, Size};

use crate::render_helpers::resources::Resources;
use crate::render_helpers::shaders::Shaders;

#[derive(Debug)]
pub struct Blur {
    program: BlurProgram,
    /// Context ID of the renderer that created the program and the textures.
    renderer_context_id: ContextId<GlesTexture>,
    /// Output texture followed by intermediate textures, large to small.
    ///
    /// Created lazily and stored here to avoid recreating blur textures frequently.
    textures: Vec<GlesTexture>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct BlurOptions {
    pub passes: u8,
    pub offset: f64,
}

impl From<niri_config::Blur> for BlurOptions {
    fn from(config: niri_config::Blur) -> Self {
        Self {
            passes: config.passes,
            offset: config.offset,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlurProgram(Rc<BlurProgramInner>);

#[derive(Debug)]
struct BlurProgramInner {
    down: BlurProgramInternal,
    up: BlurProgramInternal,
}

#[derive(Debug)]
struct BlurProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_half_pixel: ffi::types::GLint,
    uniform_offset: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

unsafe fn compile_program(gl: &ffi::Gles2, src: &str) -> Result<BlurProgramInternal, GlesError> {
    let program = unsafe { link_program(gl, include_str!("shaders/blur.vert"), src)? };

    let vert = c"vert";
    let tex = c"tex";
    let half_pixel = c"half_pixel";
    let offset = c"offset";

    Ok(BlurProgramInternal {
        program,
        uniform_tex: gl.GetUniformLocation(program, tex.as_ptr()),
        uniform_half_pixel: gl.GetUniformLocation(program, half_pixel.as_ptr()),
        uniform_offset: gl.GetUniformLocation(program, offset.as_ptr()),
        attrib_vert: gl.GetAttribLocation(program, vert.as_ptr()),
    })
}

impl BlurProgram {
    pub fn compile(renderer: &mut GlesRenderer) -> anyhow::Result<Self> {
        renderer
            .with_context(move |gl| unsafe {
                let down = compile_program(gl, include_str!("shaders/blur_down.frag"))
                    .context("error compiling blur_down shader")?;
                let up = compile_program(gl, include_str!("shaders/blur_up.frag"))
                    .context("error compiling blur_up shader")?;
                Ok(Self(Rc::new(BlurProgramInner { down, up })))
            })
            .context("error making GL context current")?
    }

    pub fn destroy(self, renderer: &mut GlesRenderer) -> Result<(), GlesError> {
        renderer.with_context(move |gl| unsafe {
            gl.DeleteProgram(self.0.down.program);
            gl.DeleteProgram(self.0.up.program);
        })
    }
}

impl Blur {
    pub fn new(renderer: &mut GlesRenderer) -> Option<Self> {
        let program = Shaders::get(renderer).blur.clone()?;
        Some(Self {
            program,
            renderer_context_id: renderer.context_id(),
            textures: Vec::new(),
        })
    }

    pub fn context_id(&self) -> ContextId<GlesTexture> {
        self.renderer_context_id.clone()
    }

    pub fn prepare_textures(
        &mut self,
        mut create_texture: impl FnMut(Fourcc, Size<i32, Buffer>) -> Result<GlesTexture, GlesError>,
        source: &GlesTexture,
        options: BlurOptions,
    ) -> anyhow::Result<()> {
        let _span = tracy_client::span!("Blur::prepare_textures");

        let passes = options.passes.clamp(1, 31) as usize;
        let half_res_output = passes >= 3;
        let size = source.size();
        let expected_first_size = if half_res_output {
            Size::new(max(1, size.w / 2), max(1, size.h / 2))
        } else {
            size
        };

        if let Some(output) = self.textures.first_mut() {
            let old_size = output.size();
            if old_size != expected_first_size {
                trace!(
                    "recreating textures: output size changed from {} × {} to {} × {}",
                    old_size.w,
                    old_size.h,
                    expected_first_size.w,
                    expected_first_size.h
                );
                self.textures.clear();
            } else if !output.is_unique_reference() {
                debug!("recreating textures: not unique",);
                self.textures.clear();
            }
        }

        // Create any missing textures. For heavy blurs (`passes >= 3`), skip allocating the
        // full-resolution level 0 output texture and stop upsampling at 1/2 resolution (hardware
        // bilinear filtering handles the final 2x upscale at zero visual loss while saving 75%
        // final-pass fill rate and a full-screen RGBA8 texture).
        let start_level = usize::from(half_res_output);
        let mut w = size.w;
        let mut h = size.h;
        for level in 0..=passes {
            let tex_size = Size::new(w, h);
            w = max(1, w / 2);
            h = max(1, h / 2);

            if level < start_level {
                continue;
            }
            let idx = level - start_level;

            if self.textures.len() > idx {
                continue;
            }

            let texture: GlesTexture =
                create_texture(Fourcc::Abgr8888, tex_size).context("error creating texture")?;
            self.textures.push(texture);
        }

        let needed = passes + 1 - start_level;
        self.textures.drain(needed..);

        Ok(())
    }

    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        source: &GlesTexture,
        options: BlurOptions,
    ) -> anyhow::Result<GlesTexture> {
        let _span = tracy_client::span!("Blur::render");
        trace!("rendering blur");

        ensure!(
            renderer.context_id() == self.renderer_context_id,
            "wrong renderer"
        );

        let passes = options.passes.clamp(1, 31) as usize;
        let half_res_output = passes >= 3;
        let start_level = usize::from(half_res_output);
        let expected_len = passes + 1 - start_level;
        let size = source.size();
        let expected_first_size = if half_res_output {
            Size::new(max(1, size.w / 2), max(1, size.h / 2))
        } else {
            size
        };

        ensure!(
            self.textures.len() == expected_len,
            "wrong textures len: expected {expected_len}, got {}",
            self.textures.len()
        );

        let output = &mut self.textures[0];
        ensure!(
            output.size() == expected_first_size,
            "wrong output texture size: expected {expected_first_size:?}, got {:?}",
            output.size()
        );

        ensure!(
            output.is_unique_reference(),
            "output texture has a non-unique reference"
        );

        let pooled = Resources::get_from_renderer(renderer).map(|res| {
            let res = res.borrow();
            (res.blur_fbo, res.blur_vbo)
        });

        renderer.with_profiled_context(gpu_span_location!("Blur::render"), |gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}

            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);

            gl.ActiveTexture(ffi::TEXTURE0);

            let mut prev_fbo = 0i32;
            gl.GetIntegerv(ffi::FRAMEBUFFER_BINDING, &mut prev_fbo as *mut _);

            let mut temp_fbo = 0;
            let fbo = if let Some((pooled_fbo, _)) = pooled {
                pooled_fbo
            } else {
                gl.GenFramebuffers(1, &mut temp_fbo as *mut _);
                temp_fbo
            };
            gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);

            let vertices: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            let bind_quad = |attrib: u32| {
                gl.EnableVertexAttribArray(attrib);
                if let Some((_, vbo)) = pooled {
                    gl.BindBuffer(ffi::ARRAY_BUFFER, vbo);
                    gl.VertexAttribPointer(attrib, 2, ffi::FLOAT, ffi::FALSE, 0, std::ptr::null());
                } else {
                    gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
                    gl.VertexAttribPointer(
                        attrib,
                        2,
                        ffi::FLOAT,
                        ffi::FALSE,
                        0,
                        vertices.as_ptr().cast(),
                    );
                }
            };

            let program = &self.program.0.down;
            gl.UseProgram(program.program);
            gl.Uniform1i(program.uniform_tex, 0);
            gl.Uniform1f(program.uniform_offset, options.offset as f32);
            bind_quad(program.attrib_vert as u32);

            let down_start = usize::from(!half_res_output);
            let src = once(source).chain(&self.textures[down_start..]);
            let dst = &self.textures[down_start..];
            for (src, dst) in zip(src, dst) {
                let dst_size = dst.size();
                let w = dst_size.w;
                let h = dst_size.h;
                gl.Viewport(0, 0, w, h);

                // During downsampling, half_pixel is half of the destination pixel.
                gl.Uniform2f(program.uniform_half_pixel, 0.5 / w as f32, 0.5 / h as f32);

                let src = src.tex_id();
                let dst = dst.tex_id();

                trace!("drawing down {src} to {dst}");
                gl.FramebufferTexture2D(
                    ffi::FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    dst,
                    0,
                );

                gl.BindTexture(ffi::TEXTURE_2D, src);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_S,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_T,
                    ffi::CLAMP_TO_EDGE as i32,
                );

                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
            }

            gl.DisableVertexAttribArray(program.attrib_vert as u32);

            // Up
            let program = &self.program.0.up;
            gl.UseProgram(program.program);
            gl.Uniform1i(program.uniform_tex, 0);
            gl.Uniform1f(program.uniform_offset, options.offset as f32);
            bind_quad(program.attrib_vert as u32);

            let src = self.textures.iter().rev();
            let dst = self.textures.iter().rev().skip(1);
            for (src, dst) in zip(src, dst) {
                let dst_size = dst.size();
                let w = dst_size.w;
                let h = dst_size.h;
                gl.Viewport(0, 0, w, h);

                // During upsampling, half_pixel is half of the source pixel.
                let src_size = src.size();
                let src_w = src_size.w as f32;
                let src_h = src_size.h as f32;
                gl.Uniform2f(program.uniform_half_pixel, 0.5 / src_w, 0.5 / src_h);

                let src = src.tex_id();
                let dst = dst.tex_id();

                trace!("drawing up {src} to {dst}");
                gl.FramebufferTexture2D(
                    ffi::FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    dst,
                    0,
                );

                gl.BindTexture(ffi::TEXTURE_2D, src);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_S,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_T,
                    ffi::CLAMP_TO_EDGE as i32,
                );

                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
            }

            gl.DisableVertexAttribArray(program.attrib_vert as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.FramebufferTexture2D(
                ffi::FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                0,
                0,
            );
            gl.BindFramebuffer(ffi::FRAMEBUFFER, prev_fbo as u32);
            if temp_fbo != 0 {
                gl.DeleteFramebuffers(1, &temp_fbo);
            }
        })?;

        Ok(self.textures[0].clone())
    }
}
