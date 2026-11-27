use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use niri_config::{WorkspaceCubeSide, WorkspaceSwitch3D, WorkspaceSwitch3DStyle};
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::{Element as _, Kind, RenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, Uniform};
use smithay::backend::renderer::Texture as _;
use smithay::utils::{Logical, Point, Scale, Size};

use super::offscreen::{OffscreenBuffer, OffscreenData};
use super::shader_element::ShaderRenderElement;
use super::shaders::{ProgramType, Shaders};
use super::solid_color::SolidColorRenderElement;
use crate::layer::mapped::LayerSurfaceRenderElement;
use crate::layout::workspace::WorkspaceRenderElement;
use crate::niri_render_elements;

niri_render_elements! {
    WorkspaceCompositeElement<R> => {
        LayerSurface = CropRenderElement<LayerSurfaceRenderElement<R>>,
        Workspace = CropRenderElement<WorkspaceRenderElement<R>>,
        SolidColor = CropRenderElement<SolidColorRenderElement>,
    }
}

/// Computed 3D perspective pose for one workspace rendered via `workspace_3d.frag`.
#[derive(Debug, Clone, Copy)]
pub struct Workspace3DPose {
    /// Workspace center Y relative to the output center (logical px, `+y` down).
    pub center_y: f64,
    /// Workspace center Z offset relative to projection plane (logical px, `+z` recedes).
    pub z_offset: f64,
    /// Rotation around horizontal X axis in radians.
    pub rotate_x: f64,
    /// Virtual camera focal length in logical px.
    pub focal: f64,
    /// Additional 2D plane scale (e.g., workspace dip zoom / depth scale).
    pub ws_scale: [f32; 2],
    /// Dynamic rounded corner radius in logical px.
    pub corner_radius: f32,
    /// Depth darkening amount `[0..1]`.
    pub dim: f32,
    /// Diagonal specular sheen `[0..1]`.
    pub sheen: f32,
    /// Output opacity `[0..1]`.
    pub alpha: f32,
    /// Sort key for painter's order (smaller = drawn closer/on top in front-to-back push order).
    pub sort_z: f64,
}

/// Computes the exact 3D perspective pose for workspace `ws_idx` given the current fractional
/// `render_idx`, instantaneous switch `velocity` (workspaces/sec), dip `zoom`, optional
/// `bounce_norm` (`[-1, 1]`), and user config.
#[allow(clippy::too_many_arguments)]
pub fn compute_workspace_3d_pose(
    ws_idx: usize,
    render_idx: f64,
    velocity: f64,
    zoom: f64,
    bounce_offset_y: f64,
    bounce_norm: f64,
    view_size: Size<f64, Logical>,
    cfg: WorkspaceSwitch3D,
    dip_corner_radius: f32,
    effect_weight: f64,
) -> Option<Workspace3DPose> {
    let h = view_size.h;
    if h <= 1. || effect_weight <= 1e-4 {
        return None;
    }

    let diff = ws_idx as f64 - render_idx;
    if diff.abs() > 2.25 {
        return None;
    }

    let fov_rad = cfg.fov.clamp(15., 110.).to_radians();
    let focal = (h * 0.5) / (fov_rad * 0.5).tan();
    let max_angle = cfg.angle.clamp(0., 85.).to_radians() * effect_weight;
    let dist_norm = (diff.abs() / cfg.radius.max(0.05)).clamp(0., 1.);
    let eased = dist_norm.powf(cfg.curve_power.max(0.1)) * effect_weight;

    // Effective transition activity in [0, 1] used to smoothly grow/fade rounded corners so rest
    // frames remain strictly sharp rectangular windows.
    let activity = (diff
        .abs()
        .min(1.0)
        .max((1.0 - zoom).abs() * 8.0)
        .max(bounce_norm.abs()))
    .clamp(0., 1.)
        * effect_weight;
    let corner_radius =
        cfg.corner_radius.max(dip_corner_radius) * smoothstep(0.0, 0.25, activity as f32);

    // Optional 3D bounce tilt when pushing against first/last workspace edge.
    let bounce_tilt = if cfg.bounce_3d && bounce_norm.abs() > 1e-4 {
        bounce_norm * 7.5_f64.to_radians() * effect_weight
    } else {
        0.0
    };

    let base_gap = h * 0.1 * zoom * (1.0 - cfg.gap_compression.clamp(0., 1.) * 0.85);
    let stride = h * zoom + base_gap;

    match cfg.style {
        WorkspaceSwitch3DStyle::Cube => {
            // Exact 90-degree vertical 3D cube with zero seam between adjacent faces.
            if diff.abs() >= 1.05 {
                return None;
            }
            let phi = diff * std::f64::consts::FRAC_PI_2 * effect_weight + bounce_tilt;
            let r = (h * 0.5) * zoom;
            let (center_y, z_offset, rotate_x) = match cfg.cube_side {
                WorkspaceCubeSide::Outside => {
                    let y_c = r * phi.sin() + bounce_offset_y * 0.35;
                    // Push cube slightly back proportionally to dip zoom so corners never clip
                    // against the camera near plane while keeping seams 100% locked.
                    let dip_z = (1.0 - zoom).max(0.0) * focal * 0.65;
                    let z_c = dip_z + r * (1.0 - phi.cos());
                    (y_c, z_c, phi)
                }
                WorkspaceCubeSide::Inside => {
                    let y_c = r * phi.sin() + bounce_offset_y * 0.35;
                    // Keep entire inner face comfortably in front of the camera.
                    let base_z = r * 1.12 + (1.0 - zoom).max(0.0) * focal * 0.5;
                    let z_c = base_z - r * (1.0 - phi.cos());
                    (y_c, z_c, -phi)
                }
            };

            let face_cos = phi.cos().clamp(0.0, 1.0);
            let dim = (cfg.dim * (1.0 - face_cos as f32)).clamp(0., 0.85);
            let alpha = (1.0 - cfg.fade * (1.0 - face_cos as f32)).clamp(0.05, 1.0);
            let sheen = cfg.sheen * (phi.sin().abs() as f32);

            Some(Workspace3DPose {
                center_y,
                z_offset,
                rotate_x,
                focal,
                ws_scale: [zoom as f32, zoom as f32],
                corner_radius,
                dim,
                sheen,
                alpha,
                sort_z: z_offset,
            })
        }
        WorkspaceSwitch3DStyle::Carousel => {
            // Vertical 3D cylinder carousel: upper leans back, center faces viewer, lower rolls in.
            let theta = (diff.clamp(-1.35, 1.35) * max_angle) + bounce_tilt;
            let cyl_r = (stride / max_angle.max(0.25)).min(h * 1.8);
            let arc_y = if max_angle > 0.02 {
                cyl_r * theta.sin()
            } else {
                diff * stride
            };
            // Blend arc spacing with linear stride so workspaces remain well-spaced even at high
            // angles.
            let center_y = (arc_y * 0.65 + diff * stride * 0.35) + bounce_offset_y * 0.6;
            let depth_scale = (1.0 - (1.0 - cfg.depth) * eased) * zoom;
            let z_offset = cyl_r * (1.0 - theta.cos()).max(0.0) * 0.75
                + (1.0 - depth_scale).max(0.0) * h * 0.35;

            let dim = (cfg.dim * eased as f32).clamp(0., 0.85);
            let alpha = (1.0 - cfg.fade * eased as f32).clamp(0.05, 1.0);
            let sheen = cfg.sheen * (theta.sin().abs() as f32);

            Some(Workspace3DPose {
                center_y,
                z_offset,
                rotate_x: theta,
                focal,
                ws_scale: [depth_scale as f32, depth_scale as f32],
                corner_radius,
                dim,
                sheen,
                alpha,
                sort_z: z_offset + diff.abs() * 10.0,
            })
        }
        WorkspaceSwitch3DStyle::Cards => {
            // VisionOS/iOS depth deck: upper/leaving workspace recedes into Z-depth and dims,
            // while the incoming lower workspace glides cleanly up over the foreground.
            if diff <= 0.0 {
                // Current or outgoing (receding into depth behind incoming card).
                let p = (-diff).clamp(0.0, 1.5);
                let pe = p.min(1.0).powf(cfg.curve_power.max(0.1)) * effect_weight;
                let depth_scale = (1.0 - (1.0 - cfg.depth) * pe) * zoom;
                let center_y = -p * h * 0.14 * zoom + bounce_offset_y * 0.6;
                let z_offset = pe * h * 0.48;
                let rotate_x = -(max_angle * 0.45 * pe) + bounce_tilt;
                let dim = (cfg.dim * pe as f32).clamp(0., 0.85);
                let alpha = (1.0 - cfg.fade * pe as f32).clamp(0.05, 1.0);
                let sheen = cfg.sheen * (pe as f32 * 0.5);

                Some(Workspace3DPose {
                    center_y,
                    z_offset,
                    rotate_x,
                    focal,
                    ws_scale: [depth_scale as f32, depth_scale as f32],
                    corner_radius,
                    dim,
                    sheen,
                    alpha,
                    // Always behind the incoming foreground workspace.
                    sort_z: 1000.0 + z_offset,
                })
            } else {
                // Incoming foreground card sliding smoothly up from below.
                let p = diff.clamp(0.0, 1.5);
                let pe = p.min(1.0).powf(cfg.curve_power.max(0.1)) * effect_weight;
                let center_y = diff * (h * zoom + base_gap * 0.5) + bounce_offset_y * 0.6;
                let rotate_x = (max_angle * 0.25 * pe) + bounce_tilt;
                let z_offset = (1.0 - zoom).max(0.0) * h * 0.25;
                let dim = (cfg.dim * 0.25 * pe as f32).clamp(0., 0.6);
                let alpha = (1.0 - cfg.fade * 0.35 * pe as f32).clamp(0.1, 1.0);
                let sheen = cfg.sheen * (pe as f32);

                Some(Workspace3DPose {
                    center_y,
                    z_offset,
                    rotate_x,
                    focal,
                    ws_scale: [zoom as f32, zoom as f32],
                    corner_radius,
                    dim,
                    sheen,
                    alpha,
                    sort_z: diff * 10.0,
                })
            }
        }
        WorkspaceSwitch3DStyle::TiltSlide => {
            // Velocity-coupled gyroscopic slide: tilt angle tracks real-time switch velocity!
            let vel_norm = (velocity * 0.28 * cfg.velocity_tilt).tanh();
            let theta = (vel_norm * max_angle) + bounce_tilt;
            let depth_scale = (1.0 - (1.0 - cfg.depth) * eased * 0.5) * zoom;
            let center_y = diff * stride + bounce_offset_y;
            let z_offset = (theta.abs() / max_angle.max(0.1)) * h * 0.14 * effect_weight
                + (1.0 - depth_scale).max(0.0) * h * 0.2;
            let dim = (cfg.dim * eased as f32 * 0.6).clamp(0., 0.8);
            let alpha = (1.0 - cfg.fade * eased as f32).clamp(0.05, 1.0);
            let sheen = cfg.sheen * (theta.sin().abs() as f32 * 1.4).min(1.0);

            Some(Workspace3DPose {
                center_y,
                z_offset,
                rotate_x: theta,
                focal,
                ws_scale: [depth_scale as f32, depth_scale as f32],
                corner_radius,
                dim,
                sheen,
                alpha,
                sort_z: z_offset + diff.abs() * 10.0,
            })
        }
        WorkspaceSwitch3DStyle::Fold => {
            // Symmetric book/page fold along the horizontal seam between adjacent workspaces.
            if diff.abs() >= 1.25 {
                return None;
            }
            let fold_angle = (diff.clamp(-1.0, 1.0) * max_angle.max(0.35)) + bounce_tilt;
            let half_h = h * 0.5 * zoom;
            // Seam moves across the screen linearly; each workspace hinges from that shared seam.
            let seam_y = if diff <= 0.0 {
                (diff + 0.5) * h * zoom
            } else {
                (diff - 0.5) * h * zoom
            };
            let dir = if diff <= 0.0 { -1.0 } else { 1.0 };
            let center_y = seam_y + dir * half_h * fold_angle.cos() + bounce_offset_y * 0.5;
            let z_offset = half_h * fold_angle.sin().abs() + (1.0 - zoom).max(0.0) * h * 0.3;
            let dim = (cfg.dim * fold_angle.sin().abs() as f32).clamp(0., 0.85);
            let alpha = (1.0 - cfg.fade * eased as f32).clamp(0.05, 1.0);
            let sheen = cfg.sheen * (fold_angle.sin().abs() as f32);

            Some(Workspace3DPose {
                center_y,
                z_offset,
                rotate_x: -fold_angle,
                focal,
                ws_scale: [zoom as f32, zoom as f32],
                corner_radius,
                dim,
                sheen,
                alpha,
                sort_z: z_offset,
            })
        }
        WorkspaceSwitch3DStyle::Depth | WorkspaceSwitch3DStyle::Parallax => None,
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(1e-5)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Lazily-allocated pool of up to 3 `OffscreenBuffer`s keyed by workspace ID.
/// Automatically cleared when idle so resting desktops hold zero FBOs.
#[derive(Debug, Default)]
pub struct Workspace3DBufferPool {
    slots: RefCell<HashMap<u64, OffscreenBuffer>>,
}

impl Workspace3DBufferPool {
    pub fn clear(&self) {
        let mut slots = self.slots.borrow_mut();
        if !slots.is_empty() {
            slots.clear();
        }
    }

    pub fn retain_ids(&self, active_ids: &[u64]) {
        self.slots
            .borrow_mut()
            .retain(|id, _| active_ids.contains(id));
    }

    /// Renders a single workspace's viewport-cropped elements into its cached `OffscreenBuffer`
    /// and wraps the resulting texture in a `ShaderRenderElement` using `workspace_3d.frag`,
    /// returning both the shader element and its `OffscreenData` so Wayland frame callbacks keep
    /// flowing at full refresh rate during 3D workspace switches.
    pub fn render_workspace(
        &self,
        renderer: &mut GlesRenderer,
        ws_id: u64,
        elements: &[impl RenderElement<GlesRenderer>],
        view_size: Size<f64, Logical>,
        scale: Scale<f64>,
        pose: Workspace3DPose,
    ) -> anyhow::Result<Option<(ShaderRenderElement, OffscreenData)>> {
        if Shaders::get(renderer)
            .program(ProgramType::Workspace3D)
            .is_none()
        {
            return Ok(None);
        }

        let mut slots = self.slots.borrow_mut();
        let buffer = slots.entry(ws_id).or_default();

        let (offscreen, _sync, mut data) = buffer.render(renderer, scale, elements)?;
        let texture = offscreen.texture();
        let tex_size: Size<f64, Logical> = Size::from((
            texture.width() as f64 / scale.x,
            texture.height() as f64 / scale.y,
        ));
        // Map output viewport origin `(0, 0)` into offscreen texture coordinates so multi-window
        // scrolling layouts never shift horizontally or snap at the end of a 3D switch.
        let view_origin = [-offscreen.offset().x as f32, -offscreen.offset().y as f32];

        let elem = ShaderRenderElement::new(
            ProgramType::Workspace3D,
            view_size,
            None,
            scale.x as f32,
            pose.alpha,
            Rc::new([
                Uniform::new("view_size", [view_size.w as f32, view_size.h as f32]),
                Uniform::new("view_origin", view_origin),
                Uniform::new("tex_size", [tex_size.w as f32, tex_size.h as f32]),
                Uniform::new("dest_size", [view_size.w as f32, view_size.h as f32]),
                Uniform::new("center_y", pose.center_y as f32),
                Uniform::new("z_offset", pose.z_offset as f32),
                Uniform::new("rotate_x", pose.rotate_x as f32),
                Uniform::new("focal", pose.focal as f32),
                Uniform::new("ws_scale", pose.ws_scale),
                Uniform::new("corner_radius", pose.corner_radius),
                Uniform::new("dim_amount", pose.dim),
                Uniform::new("sheen_strength", pose.sheen),
            ]),
            HashMap::from([(String::from("tex"), texture.clone())]),
            Kind::Unspecified,
        )
        .with_location(Point::from((0., 0.)));

        data.id = elem.id().clone();

        Ok(Some((elem, data)))
    }
}
