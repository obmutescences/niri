#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform sampler2D tex_sharp;

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float niri_scale;

uniform vec2 geo_size;
uniform vec4 corner_radius;
uniform mat3 input_to_geo;
// 2x2 inverse linear transform mapping delta in coords_geo -> delta in v_coords:
// [m00, m10, m01, m11] (column-major).
uniform vec4 geo_to_tex_linear;
uniform float has_sharp_tex;

uniform float noise;
uniform float saturation;
uniform vec4 bg_color;

uniform vec4 tint_from;
uniform vec4 tint_to;
uniform float tint_angle;
uniform float tint_mode;
uniform float tint_opacity;
uniform float tint_linear_size;
uniform float tint_linear_depth;
uniform float tint_radial_size;
uniform float tint_radial_depth;
uniform float tint_oklab;

uniform float lg_enabled;
uniform float lg_refraction_strength;
uniform float lg_power_factor;
uniform float lg_refraction_power;
uniform float lg_physical_refraction;
uniform float lg_glow_weight;
uniform float lg_edge_lighting;
uniform float lg_fringing;
uniform float lg_bevel_width;
uniform float lg_bevel_px;
uniform float lg_interior_warp;
uniform vec4 lg_rim_color;
uniform float lg_rim_strength;
uniform float lg_inner_shadow;
uniform vec4 lg_sheen_color;
uniform float lg_sheen_strength;
uniform float lg_ior;
uniform float lg_thickness;
uniform float lg_bevel_clarity;
uniform float lg_dispersion;
uniform float lg_adaptive_contrast;
uniform float lg_cursor_glow;
uniform vec2 lg_cursor_pos;
uniform float lg_focus_shimmer;
uniform float lg_focus_progress;

float niri_rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);

// Map a delta in window-normalized coords_geo space to texture UV space.
// Unifies XRay, Non-XRay, Halo padding, Overview zoom, and rotated outputs.
vec2 geo_delta_to_tex(vec2 delta_geo) {
    return vec2(
        geo_to_tex_linear.x * delta_geo.x + geo_to_tex_linear.z * delta_geo.y,
        geo_to_tex_linear.y * delta_geo.x + geo_to_tex_linear.w * delta_geo.y
    );
}

// Analytic signed distance and unit outward gradient for a rounded rectangle.
// p is centered at the window center with +X right, +Y down (same as coords_geo).
// cr = (top_left, top_right, bottom_right, bottom_left).
vec3 sd_rounded_box_grad(vec2 p, vec2 half_size, vec4 cr) {
    float r = (p.x > 0.0)
        ? ((p.y < 0.0) ? cr.y : cr.z)
        : ((p.y < 0.0) ? cr.x : cr.w);
    r = clamp(r, 0.0, min(half_size.x, half_size.y));

    vec2 s = vec2(p.x < 0.0 ? -1.0 : 1.0, p.y < 0.0 ? -1.0 : 1.0);
    vec2 q = abs(p) - half_size + r;
    float outside = length(max(q, 0.0));
    float inside = min(max(q.x, q.y), 0.0);
    float dist = outside + inside - r;

    vec2 grad;
    if (outside > 0.0001) {
        grad = (max(q, 0.0) / outside) * s;
    } else if (q.x > q.y) {
        grad = vec2(s.x, 0.0);
    } else {
        grad = vec2(0.0, s.y);
    }
    return vec3(dist, grad);
}

// Interleaved Gradient Noise (Jorge Jimenez) — far smoother than white noise.
float ign(vec2 px) {
    return fract(52.9829189 * fract(dot(px, vec2(0.06711056, 0.00583715))));
}

vec3 srgb_to_linear(vec3 c) {
    vec3 clamped = max(c, vec3(0.0));
    return step(vec3(1e-6), clamped) * pow(max(clamped, vec3(1e-6)), vec3(2.2));
}

vec3 linear_to_srgb(vec3 c) {
    vec3 clamped = max(c, vec3(0.0));
    return step(vec3(1e-6), clamped) * pow(max(clamped, vec3(1e-6)), vec3(1.0 / 2.2));
}

vec3 linear_to_oklab(vec3 c) {
    vec3 lms = vec3(
        0.4122214708 * c.r + 0.5363325363 * c.g + 0.0514459929 * c.b,
        0.2119034982 * c.r + 0.6806995451 * c.g + 0.1073969566 * c.b,
        0.0883024619 * c.r + 0.2817188376 * c.g + 0.6299787005 * c.b
    );
    vec3 clamped = max(lms, vec3(0.0));
    lms = step(vec3(1e-6), clamped) * pow(max(clamped, vec3(1e-6)), vec3(1.0 / 3.0));
    return vec3(
        0.2104542553 * lms.x + 0.7936177850 * lms.y - 0.0040720468 * lms.z,
        1.9779984951 * lms.x - 2.4285922050 * lms.y + 0.4505937099 * lms.z,
        0.0259040371 * lms.x + 0.7827717662 * lms.y - 0.8086757660 * lms.z
    );
}

vec3 oklab_to_linear(vec3 c) {
    vec3 lms = vec3(
        c.x + 0.3963377774 * c.y + 0.2158037573 * c.z,
        c.x - 0.1055613458 * c.y - 0.0638541728 * c.z,
        c.x - 0.0894841775 * c.y - 1.2914855480 * c.z
    );
    lms = lms * lms * lms;
    return clamp(vec3(
        4.0767416621 * lms.x - 3.3077115913 * lms.y + 0.2309699292 * lms.z,
        -1.2684380046 * lms.x + 2.6097574011 * lms.y - 0.3413193965 * lms.z,
        -0.0041960863 * lms.x - 0.7034186147 * lms.y + 1.7076147010 * lms.z
    ), 0.0, 1.0);
}

vec4 mix_tint_colors(vec4 c1, vec4 c2, float t) {
    if (tint_oklab < 0.5) {
        return mix(c1, c2, t);
    }
    vec3 lab1 = linear_to_oklab(srgb_to_linear(c1.rgb));
    vec3 lab2 = linear_to_oklab(srgb_to_linear(c2.rgb));
    vec3 rgb = linear_to_srgb(oklab_to_linear(mix(lab1, lab2, t)));
    return vec4(rgb, mix(c1.a, c2.a, t));
}

vec4 compute_tint(vec2 uv) {
    vec2 size = max(geo_size, vec2(1.0));
    vec2 p = uv * size;
    float t;
    vec4 from = tint_from;

    if (tint_mode < 0.5) {
        float angle = radians(tint_angle);
        vec2 dir = vec2(sin(angle), -cos(angle));
        float extent = max(dot(abs(dir), size), 1.0);
        float raw_t = clamp(dot(p - size * 0.5, dir) / extent + 0.5, 0.0, 1.0);
        float solid = clamp(tint_linear_size, 0.0, 1.0);
        t = clamp((raw_t - solid) / max(1.0 - solid, 0.0001), 0.0, 1.0);
        from.a = clamp(from.a * clamp(tint_linear_depth, 0.0, 1.0), 0.0, 1.0);
    } else {
        vec2 d = (p - size * 0.5) / (max(size.x, size.y) * 0.5);
        float r = clamp(length(d), 0.0, 1.0);
        float inner = clamp(tint_radial_size, 0.0, 1.0);
        t = clamp((r - inner) / max(1.0 - inner, 0.0001), 0.0, 1.0);
        from.a = clamp(from.a * clamp(tint_radial_depth, 0.0, 1.0), 0.0, 1.0);
    }

    return mix_tint_colors(from, tint_to, t);
}

vec3 apply_saturation(vec3 rgb, float sat) {
    const vec3 w = vec3(0.2126, 0.7152, 0.0722);
    return clamp(mix(vec3(dot(rgb, w)), rgb, sat), 0.0, 1.0);
}

// Sample blurred background, optionally blending sharp unblurred background in the bevel.
vec4 sample_glass_bg(vec2 uv, float clarity) {
    vec2 c_uv = clamp(uv, vec2(0.0005), vec2(0.9995));
    vec4 blurred = texture2D(tex, c_uv);
#if defined(NO_ALPHA)
    blurred.a = 1.0;
#endif
    if (has_sharp_tex > 0.5 && clarity > 0.001) {
        vec4 sharp = texture2D(tex_sharp, c_uv);
#if defined(NO_ALPHA)
        sharp.a = 1.0;
#endif
        return mix(blurred, sharp, clarity);
    }
    return blurred;
}

void main() {
    vec3 coords_geo = input_to_geo * vec3(v_coords, 1.0);
    vec2 uv_geo = coords_geo.xy;

    float inside_geo = step(0.0, uv_geo.x) * step(uv_geo.x, 1.0)
        * step(0.0, uv_geo.y) * step(uv_geo.y, 1.0);

    if (inside_geo <= 0.0) {
        gl_FragColor = vec4(0.0);
        return;
    }

    vec2 size = max(geo_size, vec2(1.0));
    vec2 half_size = size * 0.5;
    float min_half = min(half_size.x, half_size.y);

    // Base texture UV, with optional window-centered interior lens warp
    // (computed in window geometry space so XRay and Non-XRay behave identically).
    vec2 uv_base = v_coords;
    if (lg_enabled > 0.5 && abs(lg_interior_warp) > 0.0001) {
        vec2 d_geo = uv_geo - 0.5;
        float r2 = dot(d_geo, d_geo);
        vec2 off_geo = clamp(d_geo * (lg_interior_warp * min(r2, 0.35)), vec2(-0.12), vec2(0.12));
        uv_base -= geo_delta_to_tex(off_geo);
    }

    vec4 bg_sample;
    vec3 glass_normal = vec3(0.0, 0.0, 1.0);
    float dist = -min_half;
    vec2 grad = vec2(0.0, -1.0);
    float edge_factor = 0.0;
    float concave_factor = 0.0;
    float band_width = 0.0;
    float ior = max(lg_ior, 1.01);

    if (lg_enabled > 0.5) {
        vec2 pos_px = (uv_geo - 0.5) * size;
        vec3 sdf = sd_rounded_box_grad(pos_px, half_size, corner_radius);
        dist = sdf.x;
        grad = sdf.yz;

        float relative_band = clamp(min_half * 0.15 * lg_bevel_width, 1.0, min_half * 0.9);
        band_width = lg_bevel_px > 0.0
            ? clamp(lg_bevel_px, 1.0, min_half * 0.9)
            : relative_band;

        float inner_dist = max(-dist, 0.0);

        // Early-out for flat interior (>90% of window pixels):
        // skips bevel curvature, Snell refraction, dispersion, and Fresnel math!
        if (inner_dist < band_width && dist < 0.0) {
            edge_factor = 1.0 - clamp(inner_dist / band_width, 0.0, 1.0);
            float e_smooth = smoothstep(0.0, 1.0, edge_factor);
            float e_pow = pow(max(e_smooth, 0.0001), max(lg_power_factor, 1.0));
            concave_factor = 1.0 - sqrt(max(1.0 - e_pow, 0.0));

            // Sharp bevel clarity: edges refract clear background, interior stays frosted.
            float clarity = clamp(lg_bevel_clarity * pow(max(e_smooth, 0.0001), 1.6), 0.0, 1.0);

            // Smooth convex lens profile normal (zero finite-difference overhead).
            float u = clamp(inner_dist / band_width, 0.0, 1.0);
            float bevel_slope = (1.0 - u) / sqrt(max(1.0 - (1.0 - u) * (1.0 - u), 0.08));
            float normal_tilt = bevel_slope * 0.48 * max(lg_refraction_power, 0.0);
            glass_normal = normalize(vec3(grad * normal_tilt, 1.0));

            if (lg_refraction_strength > 0.0001) {
                // Smooth soft-knee mapping so values above 20 still respond instead of clipping.
                float strength_scale = (lg_refraction_strength / (1.0 + lg_refraction_strength * 0.02)) * 0.08;
                vec2 delta_px_g;

                if (lg_physical_refraction > 0.5) {
                    float eff_ior = 1.0 + (ior - 1.0) * clamp(strength_scale * 1.4, 0.05, 2.2);
                    vec3 refr_g = refract(vec3(0.0, 0.0, -1.0), glass_normal, 1.0 / eff_ior);
                    float eff_thickness = max(lg_thickness, band_width * 0.85) * max(lg_refraction_power, 0.1);
                    delta_px_g = (refr_g.xy / max(-refr_g.z, 0.25)) * eff_thickness;
                } else {
                    float mag_px = min(band_width * 1.8 * concave_factor * strength_scale * lg_refraction_power, band_width * 2.5);
                    delta_px_g = -grad * mag_px;
                }

                float disp = lg_dispersion > 0.0001
                    ? lg_dispersion
                    : (lg_fringing * 0.18);

                if (disp > 0.001) {
                    vec2 uv_r = uv_base + geo_delta_to_tex((delta_px_g * (1.0 + disp)) / size);
                    vec2 uv_g = uv_base + geo_delta_to_tex(delta_px_g / size);
                    vec2 uv_b = uv_base + geo_delta_to_tex((delta_px_g * (1.0 - disp)) / size);
                    vec4 s_g = sample_glass_bg(uv_g, clarity);
                    float r = sample_glass_bg(uv_r, clarity).r;
                    float b = sample_glass_bg(uv_b, clarity).b;
                    bg_sample = vec4(r, s_g.g, b, s_g.a);
                } else {
                    vec2 uv_g = uv_base + geo_delta_to_tex(delta_px_g / size);
                    bg_sample = sample_glass_bg(uv_g, clarity);
                }
            } else {
                bg_sample = sample_glass_bg(uv_base, clarity * 0.5);
            }
        } else {
            bg_sample = sample_glass_bg(uv_base, 0.0);
        }
    } else {
        bg_sample = sample_glass_bg(uv_base, 0.0);
    }

    // --- Physically ordered color pipeline ---
    // 1. Unpremultiply background color.
    float out_a = bg_sample.a;
    vec3 rgb = out_a > 0.0001 ? (bg_sample.rgb / out_a) : vec3(0.0);

    // 2. Apply background saturation (only to transmitted background, never distorting tint/rim).
    if (saturation != 1.0) {
        rgb = apply_saturation(rgb, saturation);
    }

    // Estimate linear perceptual luminance of background.
    float bg_lum = dot(rgb * rgb, vec3(0.2126, 0.7152, 0.0722));

    // 3. Adaptive vibrancy / legibility contrast for glass windows.
    if (lg_enabled > 0.5 && lg_adaptive_contrast > 0.5) {
        // Gently tame very bright spots and slightly lift ultra-dark regions so glass stays legible.
        float bright_damp = smoothstep(0.65, 1.0, bg_lum) * 0.08;
        float dark_lift = (1.0 - smoothstep(0.0, 0.14, bg_lum)) * 0.04;
        rgb = clamp(rgb * (1.0 - bright_damp) + vec3(dark_lift), 0.0, 1.0);
    }

    // 4. Oklab/sRGB gradient tint with Beer-Lambert optical thickness absorption at beveled edges.
    if (tint_opacity > 0.0001) {
        vec4 t_col = compute_tint(uv_geo);
        float ndotv = clamp(glass_normal.z, 0.35, 1.0);
        float optical_depth = lg_enabled > 0.5 ? (1.0 / ndotv) : 1.0;
        float a = clamp(1.0 - pow(max(1.0 - clamp(t_col.a * tint_opacity, 0.0, 0.999), 0.0001), optical_depth), 0.0, 1.0);
        vec3 premul_bg = rgb * out_a;
        vec3 premul_out = t_col.rgb * a + premul_bg * (1.0 - a);
        out_a = a + out_a * (1.0 - a);
        rgb = out_a > 0.0001 ? (premul_out / out_a) : vec3(0.0);
    }

    // 5. Panel-space IGN micro-frost grain (moves with the glass panel, no screen-door lock).
    if (noise > 0.0) {
        vec2 panel_px = uv_geo * size * niri_scale;
        rgb = clamp(rgb + (ign(panel_px) - 0.5) * noise, 0.0, 1.0);
    }

    // 6. Glass lighting: inner shadow, Schlick Fresnel, dual-light specular, rim, cursor glow, shimmer.
    if (lg_enabled > 0.5 && dist < 0.0) {
        float inner_dist = -dist;

        if (lg_inner_shadow > 0.0001 && inner_dist < band_width * 2.0) {
            float inner = smoothstep(0.0, band_width * 2.0, inner_dist);
            rgb *= 1.0 - lg_inner_shadow * (1.0 - inner) * 0.45;
        }

        vec3 V = vec3(0.0, 0.0, 1.0);
        float ndotv = clamp(dot(glass_normal, V), 0.0, 1.0);
        float r0 = (ior - 1.0) / (ior + 1.0);
        float f0 = r0 * r0;
        float om = max(1.0 - ndotv, 0.0);
        float fresnel = f0 + (1.0 - f0) * om * om * om * sqrt(om);

        // Dual directional lights: key from top-left, soft fill from bottom-right.
        vec3 L_key = normalize(vec3(-0.55, -0.72, 0.42));
        vec3 L_fill = normalize(vec3(0.55, 0.72, 0.38));
        vec3 H_key = normalize(L_key + V);
        vec3 H_fill = normalize(L_fill + V);

        float nh_key = max(dot(glass_normal, H_key), 0.0);
        float nh_fill = max(dot(glass_normal, H_fill), 0.0);
        float spec_key = nh_key > 0.0001 ? pow(nh_key, 26.0) : 0.0;
        float spec_fill = (nh_fill > 0.0001 ? pow(nh_fill, 18.0) : 0.0) * 0.42;

        // Crisp 1.25px scale-aware directional glass rim highlight.
        float aa_px = 1.0 / max(niri_scale, 0.5);
        float rim_band = smoothstep(0.0, -1.2 * aa_px, dist)
            * (1.0 - smoothstep(-1.1 * aa_px, -2.6 * aa_px, dist));
        float dir_bias = 0.28 + 0.72 * max(dot(grad, normalize(vec2(-0.58, -0.81))), 0.0);
        float opp_bias = 0.35 * max(dot(grad, normalize(vec2(0.58, 0.81))), 0.0);

        float adapt_rim = lg_adaptive_contrast > 0.5 ? (0.35 + (1.0 - bg_lum) * 0.45) : 0.65;
        float rim_factor = (
            fresnel * concave_factor * 0.55 * lg_edge_lighting
            + (spec_key + spec_fill) * concave_factor * 0.75 * lg_edge_lighting
            + rim_band * (dir_bias + opp_bias) * (0.45 + max(lg_glow_weight, 0.0) * 1.4)
        ) * lg_rim_strength * lg_rim_color.a * adapt_rim;

        // Subtle dark-background bevel lift preserving hue.
        float edge_boost = concave_factor * 0.25 * (1.0 - bg_lum * 0.65) * lg_edge_lighting;
        rgb = rgb * (1.0 + clamp(edge_boost, 0.0, 0.65));
        rgb = mix(rgb, lg_rim_color.rgb, clamp(rim_factor, 0.0, 0.92));

        // Interactive pointer proximity glow along the glass bevel.
        if (lg_cursor_glow > 0.0001 && inner_dist < band_width * 1.5) {
            vec2 p_local = uv_geo * size;
            float d_cur = length(p_local - lg_cursor_pos);
            float cur_falloff = exp(-(d_cur * d_cur) / (2.0 * 145.0 * 145.0));
            float cur_edge = clamp(rim_band * 0.85 + concave_factor * 0.45, 0.0, 1.0);
            float cur_mix = clamp(lg_cursor_glow * cur_falloff * cur_edge * lg_rim_color.a, 0.0, 0.85);
            rgb = mix(rgb, lg_rim_color.rgb, cur_mix);
        }

        // One-shot directional diagonal glass shimmer sweep on focus gain.
        // Positive `lg_focus_progress` sweeps left-to-right; negative sweeps right-to-left.
        float fp = abs(lg_focus_progress);
        if (lg_focus_shimmer > 0.0001 && fp > 0.001 && fp < 0.999) {
            float sx = lg_focus_progress >= 0.0 ? uv_geo.x : (1.0 - uv_geo.x);
            float diag = sx * 0.65 + uv_geo.y * 0.35;
            float sweep_pos = fp * 1.6 - 0.3;
            float d_sweep = (diag - sweep_pos) * 7.5;
            float band = exp(-(d_sweep * d_sweep));
            float env = sin(fp * 3.14159265);
            float shimmer = clamp(band * env * lg_focus_shimmer * 0.28, 0.0, 0.55);
            rgb = mix(rgb, lg_sheen_color.rgb, shimmer);
        }
    }

    // 7. Soft directional sheen (works independently of refraction).
    if (lg_sheen_strength > 0.0001) {
        float sheen_diag = clamp(1.0 - (uv_geo.y * 0.82 + uv_geo.x * 0.18) / 0.58, 0.0, 1.0);
        float sheen_mask = sheen_diag * sheen_diag;
        float s_a = clamp(lg_sheen_color.a * lg_sheen_strength * sheen_mask, 0.0, 1.0);
        rgb = mix(rgb, lg_sheen_color.rgb, s_a);
    }

    // 8. Re-premultiply, composite bg_color behind transparent regions, and apply rounded-corner AA.
    vec4 color = vec4(rgb * out_a, out_a);
    color = color + bg_color * (1.0 - color.a);
    color = color * niri_rounding_alpha(uv_geo * size, size, corner_radius);
    color = color * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
