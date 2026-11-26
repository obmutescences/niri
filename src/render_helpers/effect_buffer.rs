use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::mem;

use anyhow::{ensure, Context as _};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::{Id, RenderElementStates};
use smithay::backend::renderer::gles::{GlesFrame, GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{
    Bind as _, Color32F, ContextId, FrameContext as _, Offscreen as _, Renderer as _, Texture,
};
use smithay::utils::{Buffer, Logical, Physical, Scale, Size, Transform};

use crate::niri::OutputRenderElements;
use crate::render_helpers::blur::{Blur, BlurOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CustomBlurKey {
    passes: u8,
    offset_x100: i32,
}

impl CustomBlurKey {
    fn from_options(options: BlurOptions) -> Self {
        Self {
            passes: options.passes,
            offset_x100: (options.offset * 100.0).round() as i32,
        }
    }
}

#[derive(Debug)]
pub struct EffectBuffer {
    /// Id to be used for this effect buffer's elements.
    id: Id,

    /// Size of the effect buffer.
    size: Size<i32, Buffer>,
    /// Scale of the effect buffer.
    scale: Scale<f64>,
    /// Options for blurring.
    blur_options: BlurOptions,

    /// Elements to be rendered on demand.
    elements: Elements,
    /// Offscreen buffer where elements get rendered.
    offscreen: Option<Offscreen>,
    /// Blurring program, if available.
    blur: Option<Blur>,
    /// Per-override cached blurred textures so XRay windows with custom `blur-passes` /
    /// `blur-offset` render at full XRay speed without re-blurring static wallpapers.
    custom_blurs: HashMap<CustomBlurKey, (Blur, Option<GlesTexture>)>,

    /// Commit counter that takes into account both original and blurred texture changes.
    commit_counter: CommitCounter,
}

#[derive(Debug)]
enum Elements {
    /// Contents remain unchanged.
    Unchanged(
        // Storage to avoid reallocating it every time.
        Vec<OutputRenderElements<GlesRenderer>>,
    ),
    /// New contents, need to check damage and render.
    New(Vec<OutputRenderElements<GlesRenderer>>),
}

#[derive(Debug)]
struct Offscreen {
    /// The texture with the offscreen contents.
    texture: GlesTexture,
    /// Id of the renderer context that the texture comes from.
    renderer_context_id: ContextId<GlesTexture>,
    /// Scale of the texture.
    scale: Scale<f64>,
    /// Damage tracker for drawing to the texture.
    damage: OutputDamageTracker,
    /// Render element states from the last render into the offscreen.
    states: RenderElementStates,
    /// Rendered blurred version of the texture.
    ///
    /// When texture needs to be reblurred, this field must be reset to `None`.
    blurred: Option<GlesTexture>,
}

impl Default for Elements {
    fn default() -> Self {
        Self::Unchanged(Vec::new())
    }
}

impl EffectBuffer {
    pub fn new() -> Self {
        Self {
            id: Id::new(),
            size: Size::default(),
            scale: Scale::from(1.),
            blur_options: BlurOptions::default(),
            elements: Elements::default(),
            offscreen: None,
            blur: None,
            custom_blurs: HashMap::new(),
            commit_counter: CommitCounter::default(),
        }
    }

    pub fn id(&self) -> &Id {
        &self.id
    }

    pub fn commit(&self) -> CommitCounter {
        self.commit_counter
    }

    pub fn logical_size(&self) -> Size<f64, Logical> {
        self.size.to_f64().to_logical(self.scale, Transform::Normal)
    }

    pub fn scale(&self) -> Scale<f64> {
        self.scale
    }

    pub fn render_element_states(&self) -> Option<&RenderElementStates> {
        self.offscreen.as_ref().map(|o| &o.states)
    }

    pub fn update_size(&mut self, size: Size<i32, Physical>, scale: Scale<f64>) {
        self.size = size.to_logical(1).to_buffer(1, Transform::Normal);
        self.scale = scale;
    }

    pub fn update_blur_options(&mut self, options: BlurOptions) {
        if self.blur_options == options {
            return;
        }

        self.blur_options = options;

        if let Some(offscreen) = &mut self.offscreen {
            if offscreen.blurred.is_some() {
                offscreen.blurred = None;
                self.commit_counter.increment();
            }
        }
    }

    pub fn elements(&mut self) -> &mut Vec<OutputRenderElements<GlesRenderer>> {
        // Assume we're going to insert new elements, switch to New.
        match mem::take(&mut self.elements) {
            Elements::Unchanged(elements) | Elements::New(elements) => {
                self.elements = Elements::New(elements);
            }
        }
        let Elements::New(elements) = &mut self.elements else {
            unreachable!();
        };
        elements
    }

    fn invalidate_custom_blurs(&mut self) {
        for (_, cached) in self.custom_blurs.values_mut() {
            *cached = None;
        }
    }

    pub fn prepare(&mut self, renderer: &mut GlesRenderer, blur: bool) -> bool {
        self.prepare_with_blur(renderer, blur.then_some(self.blur_options))
    }

    pub fn prepare_with_blur(
        &mut self,
        renderer: &mut GlesRenderer,
        blur_options: Option<BlurOptions>,
    ) -> bool {
        if let Err(err) = self.prepare_offscreen(renderer) {
            warn!("error preparing offscreen: {err:?}");
            return false;
        };

        if let Some(opts) = blur_options.filter(|o| o.passes > 0 && o.offset > 0.0) {
            if let Err(err) = self.prepare_blur_with(renderer, opts) {
                warn!("error preparing blur: {err:?}");
                return false;
            }
        }

        true
    }

    fn prepare_offscreen(&mut self, renderer: &mut GlesRenderer) -> anyhow::Result<()> {
        let _span = tracy_client::span!("EffectBuffer::prepare_offscreen");

        // Check if we need to create or recreate the texture.
        let size_string;
        let mut reason = "";
        if let Some(Offscreen {
            texture,
            renderer_context_id,
            ..
        }) = &mut self.offscreen
        {
            let old_size = texture.size();
            if old_size != self.size {
                size_string = format!(
                    "size changed from {} × {} to {} × {}",
                    old_size.w, old_size.h, self.size.w, self.size.h
                );
                reason = &size_string;

                self.offscreen = None;
            } else if !texture.is_unique_reference() {
                reason = "not unique";

                self.offscreen = None;
            } else if *renderer_context_id != renderer.context_id() {
                reason = "renderer id changed";

                self.offscreen = None;
            }
        } else {
            reason = "first render";
        }

        if self.offscreen.is_none() {
            self.invalidate_custom_blurs();
        }

        let offscreen = if let Some(offscreen) = &mut self.offscreen {
            offscreen
        } else {
            trace!("creating new offscreen texture: {reason}");
            let span = tracy_client::span!("creating effect offscreen texture");
            span.emit_text(reason);

            let texture: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, self.size)
                .context("error creating texture")?;

            let buffer_size = self.size.to_logical(1, Transform::Normal).to_physical(1);
            let damage = OutputDamageTracker::new(buffer_size, self.scale, Transform::Normal);

            self.offscreen.insert(Offscreen {
                texture,
                renderer_context_id: renderer.context_id(),
                scale: self.scale,
                damage,
                states: RenderElementStates::default(),
                blurred: None,
            })
        };

        // Recreate the damage tracker if the scale changes. We already recreate it for buffer size
        // changes, and transform is always Normal.
        let mut invalidated_blur = false;
        if offscreen.scale != self.scale {
            offscreen.scale = self.scale;

            trace!("recreating damage tracker due to scale change");
            let buffer_size = self.size.to_logical(1, Transform::Normal).to_physical(1);
            offscreen.damage = OutputDamageTracker::new(buffer_size, self.scale, Transform::Normal);

            self.commit_counter.increment();
            offscreen.blurred = None;
            invalidated_blur = true;
        }

        // Render the elements if any.
        let mut elements = match mem::take(&mut self.elements) {
            Elements::New(elements) => elements,
            x @ Elements::Unchanged(_) => {
                // No redrawing necessary.
                self.elements = x;
                if invalidated_blur {
                    self.invalidate_custom_blurs();
                }
                return Ok(());
            }
        };

        let res = {
            let mut target = renderer
                .bind(&mut offscreen.texture)
                .context("error binding texture")?;
            offscreen
                .damage
                .render_output(renderer, &mut target, 1, &elements, Color32F::TRANSPARENT)
                .context("error rendering")?
        };

        offscreen.states = res.states;

        if res.damage.is_some() {
            self.commit_counter.increment();

            // Original texture changed; reset the blurred texture.
            offscreen.blurred = None;
            invalidated_blur = true;
        }

        // Clear and put the storage back.
        elements.clear();
        self.elements = Elements::Unchanged(elements);

        if invalidated_blur {
            self.invalidate_custom_blurs();
        }

        Ok(())
    }

    fn prepare_blur_with(
        &mut self,
        renderer: &mut GlesRenderer,
        options: BlurOptions,
    ) -> anyhow::Result<()> {
        let offscreen = self.offscreen.as_mut().context("missing offscreen")?;
        ensure!(
            offscreen.renderer_context_id == renderer.context_id(),
            "wrong renderer context id"
        );

        let is_default = options == self.blur_options;
        if is_default {
            if offscreen.blurred.is_some() {
                // Already rendered.
                return Ok(());
            }

            if let Some(blur) = &self.blur {
                if blur.context_id() != renderer.context_id() {
                    debug!("recreating blur: renderer changed");
                    self.blur = None;
                }
            }

            let blur = if let Some(blur) = &mut self.blur {
                blur
            } else {
                let Some(blur) = Blur::new(renderer) else {
                    // Missing blur shader.
                    return Ok(());
                };
                self.blur.insert(blur)
            };

            blur.prepare_textures(
                |fourcc, size| renderer.create_buffer(fourcc, size),
                &offscreen.texture,
                self.blur_options,
            )
            .context("error preparing blur textures")?;

            return Ok(());
        }

        let key = CustomBlurKey::from_options(options);
        if let Some((blur, cached)) = self.custom_blurs.get(&key) {
            if blur.context_id() == renderer.context_id() && cached.is_some() {
                return Ok(());
            }
        }

        if self
            .custom_blurs
            .get(&key)
            .is_some_and(|(blur, _)| blur.context_id() != renderer.context_id())
        {
            self.custom_blurs.remove(&key);
        }

        if self.custom_blurs.len() >= 8 && !self.custom_blurs.contains_key(&key) {
            self.custom_blurs.clear();
        }

        let (blur, cached) = match self.custom_blurs.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let Some(blur) = Blur::new(renderer) else {
                    return Ok(());
                };
                entry.insert((blur, None))
            }
        };
        *cached = None;
        blur.prepare_textures(
            |fourcc, size| renderer.create_buffer(fourcc, size),
            &offscreen.texture,
            options,
        )
        .context("error preparing custom blur textures")?;

        Ok(())
    }

    pub fn render(&mut self, frame: &mut GlesFrame, blur: bool) -> anyhow::Result<GlesTexture> {
        self.render_pair_with_blur(frame, blur.then_some(self.blur_options))
            .map(|(tex, _)| tex)
    }

    /// Returns `(primary_texture, optional_sharp_texture)`.
    /// When `blur_options` has positive `passes` and `offset`, `primary_texture` is blurred with
    /// those exact settings and `optional_sharp_texture` holds the original crisp offscreen buffer
    /// for dual-texture Liquid Glass bevel clarity.
    pub fn render_pair_with_blur(
        &mut self,
        frame: &mut GlesFrame,
        blur_options: Option<BlurOptions>,
    ) -> anyhow::Result<(GlesTexture, Option<GlesTexture>)> {
        let offscreen = self.offscreen.as_mut().context("offscreen is missing")?;

        let Some(options) = blur_options.filter(|o| o.passes > 0 && o.offset > 0.0) else {
            return Ok((offscreen.texture.clone(), None));
        };

        let sharp = offscreen.texture.clone();
        if options == self.blur_options {
            let texture = if let Some(texture) = &offscreen.blurred {
                texture.clone()
            } else {
                let blur = self.blur.as_mut().context("blur is missing")?;
                let mut guard = frame.renderer();
                let renderer = guard.as_mut();
                let blurred = blur
                    .render(renderer, &offscreen.texture, self.blur_options)
                    .context("error rendering blur")?;
                offscreen.blurred.insert(blurred).clone()
            };
            return Ok((texture, Some(sharp)));
        }

        let key = CustomBlurKey::from_options(options);
        let (blur, cached) = self
            .custom_blurs
            .get_mut(&key)
            .context("custom blur is missing")?;
        let texture = if let Some(texture) = cached {
            texture.clone()
        } else {
            let mut guard = frame.renderer();
            let renderer = guard.as_mut();
            let blurred = blur
                .render(renderer, &offscreen.texture, options)
                .context("error rendering custom blur")?;
            cached.insert(blurred).clone()
        };

        Ok((texture, Some(sharp)))
    }
}
