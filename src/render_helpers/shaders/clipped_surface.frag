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

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float niri_scale;

uniform vec2 geo_size;
uniform vec4 corner_radius;
uniform mat3 input_to_geo;

uniform vec4 tint_from;
uniform vec4 tint_to;
uniform float tint_angle;
uniform float tint_mode;
uniform float tint_opacity;
uniform float tint_linear_size;
uniform float tint_linear_depth;
uniform float tint_radial_size;
uniform float tint_radial_depth;

uniform float lg_refraction_strength;
uniform float lg_power_factor;
uniform float lg_refraction_power;
uniform float lg_physical_refraction;
uniform float lg_glow_weight;
uniform float lg_edge_lighting;
uniform float lg_fringing;
uniform float lg_bevel_width;
uniform float lg_bevel_px;
// Interior lens warp: bends the whole surface like a lens, not just the edge band.
// 0 disables; positive values pull samples toward the center (magnify).
uniform float lg_interior_warp;
uniform vec4 lg_rim_color;
uniform float lg_rim_strength;
uniform float lg_inner_shadow;
uniform vec4 lg_sheen_color;
uniform float lg_sheen_strength;

float niri_rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);
vec4 postprocess(vec4 color);

struct GlassFragment {
    vec4 color;
    float dist;
    float edgeFactor;
    float concaveFactor;
    vec3 normal;
    float ior;
};

float rounded_rectangle_dist(vec2 p, vec2 b, vec4 radius)
{
    float r = p.x > 0.0
        ? (p.y > 0.0 ? radius.y : radius.w)
        : (p.y > 0.0 ? radius.x : radius.z);
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

vec4 tint_color(vec2 uv) {
    vec2 size = max(geo_size, vec2(1.0));
    vec2 p = uv * size;
    float t;
    vec4 from = tint_from;

    if (tint_mode < 0.5) {
        // CSS linear-gradient angle: 0 = bottom-to-top, 90 = left-to-right,
        // 180 = top-to-bottom.
        float angle = radians(tint_angle);
        vec2 dir = vec2(sin(angle), -cos(angle));
        float extent = max(dot(abs(dir), size), 1.0);
        float raw_t = clamp(dot(p - size * 0.5, dir) / extent + 0.5, 0.0, 1.0);

        // linear-size keeps the start color solid for this fraction of the axis
        // before blending to the end color.
        float solid = clamp(tint_linear_size, 0.0, 1.0);
        t = clamp((raw_t - solid) / max(1.0 - solid, 0.0001), 0.0, 1.0);
        from.a = clamp(from.a * clamp(tint_linear_depth, 0.0, 1.0), 0.0, 1.0);
    } else {
        vec2 d = (p - size * 0.5) / (max(size.x, size.y) * 0.5);
        float r = clamp(length(d), 0.0, 1.0);

        // radial-size keeps the center color solid out to this fraction of the
        // radius, then blends to the edge color.
        float inner = clamp(tint_radial_size, 0.0, 1.0);
        t = clamp((r - inner) / max(1.0 - inner, 0.0001), 0.0, 1.0);

        // radial-depth makes the center color stronger or lighter independently
        // of the spatial size.
        from.a = clamp(from.a * clamp(tint_radial_depth, 0.0, 1.0), 0.0, 1.0);
    }

    return mix(from, tint_to, t);
}

vec4 apply_tint(vec4 color, vec2 uv) {
    if (tint_opacity <= 0.0001) {
        return color;
    }

    vec4 tint = tint_color(uv);
    float a = clamp(tint.a * tint_opacity, 0.0, 1.0);

    // Premultiplied "over": colors the glass and, on transparent samples, also
    // contributes panel opacity like the old border background layer did.
    vec3 rgb = tint.rgb * a + color.rgb * (1.0 - a);
    float alpha = a + color.a * (1.0 - a);
    return vec4(rgb, alpha);
}

vec4 apply_sheen(vec4 color, vec2 uv) {
    if (lg_sheen_strength <= 0.0001) {
        return color;
    }

    float mask = clamp(1.0 - uv.y / 0.5, 0.0, 1.0);
    mask *= mask;
    float a = clamp(lg_sheen_color.a * lg_sheen_strength * mask, 0.0, 1.0);

    // Color is premultiplied; mix in unpremultiplied space and re-premultiply.
    vec3 base = color.a > 0.0001 ? color.rgb / color.a : color.rgb;
    vec3 lit = mix(base, lg_sheen_color.rgb, a);
    return vec4(lit * color.a, color.a);
}

GlassFragment glass_refraction(
    vec2 uv_tex,
    vec2 position,
    vec2 half_size,
    vec4 radius,
    float dist,
    float edge_factor,
    float concave_factor,
    float refraction_strength,
    float refraction_power,
    float refraction_rgb_fringing
) {
    const float h = 1.0;
    vec2 gradient = vec2(
        rounded_rectangle_dist(position + vec2(h, 0.0), half_size, radius)
            - rounded_rectangle_dist(position - vec2(h, 0.0), half_size, radius),
        rounded_rectangle_dist(position + vec2(0.0, h), half_size, radius)
            - rounded_rectangle_dist(position - vec2(0.0, h), half_size, radius)
    );

    vec2 normal = length(gradient) > 0.0 ? -normalize(gradient) : vec2(0.0, 1.0);
    float final_strength = min(0.4 * concave_factor * refraction_strength * refraction_power, 1.5);

    vec2 refract_offset_g = -normal.xy * final_strength;
    vec2 refract_offset_r = -normal.xy * final_strength;
    vec2 refract_offset_b = -normal.xy * final_strength;

    float fringing_factor = refraction_rgb_fringing * 0.3;
    if (fringing_factor > 0.0) {
        refract_offset_r = -normal.xy * (final_strength * (1.0 + fringing_factor));
        refract_offset_b = -normal.xy * (final_strength * (1.0 - fringing_factor));
    }

    // Fix Y-axis: position space has Y inverted relative to UV space
    // (see position.y = -position.y in glass_effect).
    // Without this flip, top/bottom edges refract outward instead of inward,
    // causing the offset to clamp at the texture boundary and killing the effect.
    refract_offset_r.y = -refract_offset_r.y;
    refract_offset_g.y = -refract_offset_g.y;
    refract_offset_b.y = -refract_offset_b.y;

    vec2 coord_r = clamp(uv_tex - refract_offset_r, 0.0, 1.0);
    vec2 coord_g = clamp(uv_tex - refract_offset_g, 0.0, 1.0);
    vec2 coord_b = clamp(uv_tex - refract_offset_b, 0.0, 1.0);

    vec4 color = vec4(
        texture2D(tex, coord_r).r,
        texture2D(tex, coord_g).g,
        texture2D(tex, coord_b).b,
        texture2D(tex, coord_g).a
    );
    return GlassFragment(color, dist, edge_factor, concave_factor, vec3(0.0, 0.0, 1.0), 1.0);
}

GlassFragment snells_refraction(
    vec2 uv_tex,
    vec2 position,
    vec2 half_size,
    vec4 radius,
    float dist,
    float edge_factor,
    float concave_factor,
    float refraction_strength,
    float refraction_bevel_intensity,
    float refraction_offset_strength,
    float refraction_rgb_fringing,
    float band_width
) {
    float ior = 1.0 + refraction_strength * 0.5;

    float min_r = min(min(radius.x, radius.y), min(radius.z, radius.w));
    float eps = min(band_width * 0.75, min_r * 0.6);
    float dxp = rounded_rectangle_dist(position + vec2(eps, 0.0), half_size, radius);
    float dxn = rounded_rectangle_dist(position - vec2(eps, 0.0), half_size, radius);
    float dyp = rounded_rectangle_dist(position + vec2(0.0, eps), half_size, radius);
    float dyn = rounded_rectangle_dist(position - vec2(0.0, eps), half_size, radius);
    vec2 smooth_grad = vec2(dxp - dxn, dyp - dyn);
    float grad_len = length(smooth_grad);

    float normal_height = concave_factor * refraction_bevel_intensity;
    vec2 normal_xy = grad_len > 0.001 ? (smooth_grad / grad_len) * normal_height : vec2(0.0);
    vec3 glass_normal = normalize(vec3(normal_xy, 1.0));

    float lens_magnitude = concave_factor * band_width * refraction_bevel_intensity;
    vec2 surface_normal = grad_len > 0.001 ? smooth_grad / grad_len : vec2(0.0, 1.0);

    vec2 normalized_pos = position / (half_size * 2.0);
    float corner_weight = dot(normalized_pos, normalized_pos) * refraction_offset_strength;
    surface_normal += normalized_pos * concave_factor * corner_weight;

    vec2 uv_scale = 1.0 / (half_size * 2.0);
    vec2 lens_shift = -surface_normal * lens_magnitude * uv_scale;

    vec3 view_ray = vec3(0.0, 0.0, -1.0);
    vec3 refracted = refract(view_ray, glass_normal, 1.0 / ior);
    vec2 dir = length(refracted.xy) > 0.001 ? normalize(refracted.xy) : vec2(0.0);

    float refraction_magnitude = lens_magnitude * refraction_strength;
    vec2 shift_g = dir * refraction_magnitude * uv_scale + lens_shift;
    // Fix Y-axis: position space has Y inverted relative to UV space.
    shift_g.y = -shift_g.y;
    vec2 uv_g = clamp(uv_tex + shift_g, 0.0, 1.0);
    vec4 sample_g = texture2D(tex, uv_g);

    vec4 color;
    if (refraction_rgb_fringing > 0.001) {
        float fringe = clamp(refraction_rgb_fringing, 0.0, 1.0) * 0.3;
        vec2 shift_r = dir * (refraction_magnitude * (1.0 + fringe)) * uv_scale + lens_shift;
        vec2 shift_b = dir * (refraction_magnitude * (1.0 - fringe)) * uv_scale + lens_shift;
        shift_r.y = -shift_r.y;
        shift_b.y = -shift_b.y;

        float r = texture2D(tex, clamp(uv_tex + shift_r, 0.0, 1.0)).r;
        float b = texture2D(tex, clamp(uv_tex + shift_b, 0.0, 1.0)).b;
        color = vec4(r, sample_g.g, b, sample_g.a);
    } else {
        color = sample_g;
    }

    return GlassFragment(color, dist, edge_factor, concave_factor, glass_normal, ior);
}

vec3 glass_outline(
    vec2 position,
    vec2 blur_size,
    GlassFragment sample,
    float glow_strength,
    float edge_lighting
) {
    // Content luminance: used to adapt the glass effect to light vs dark content.
    // Light content gets a specular (white-mixing) edge; dark content gets a
    // subtle brightness boost that preserves its hue.
    float lum = dot(sample.color.rgb, vec3(0.299, 0.587, 0.114));

    float rim_mask = clamp(0.25 * sample.concaveFactor, 0.0, max(glow_strength, 0.0));

    // Specular highlight: mixes towards the configured rim color.
    float spec_strength = clamp(
        rim_mask * edge_lighting * (0.08 + lum * 0.7) * lg_rim_strength * lg_rim_color.a,
        0.0,
        1.0
    );
    vec3 glow = mix(sample.color.rgb, lg_rim_color.rgb, spec_strength);

    // Edge brightness boost: stronger on dark content (to keep glass edges
    // visible) and weaker on light content (which is already bright).
    float boost = sample.concaveFactor * 0.4 * (1.0 - lum * 0.6) * edge_lighting;
    glow = glow * (1.0 + clamp(boost, 0.0, 2.0));

    // Corner highlights: subtle extra specular at top-left and bottom-right.
    if (glow_strength > 0.0) {
        float edge_mask = smoothstep(0.0, -2.0, sample.dist);
        float border_inner = smoothstep(-1.0, -3.0, sample.dist);
        float edge_profile = edge_mask - border_inner;
        float thickness_shadow = pow(edge_profile, 0.9);
        float shadow_mask = smoothstep(blur_size.y * 0.7, -blur_size.y * 0.7, position.y)
            * smoothstep(blur_size.x * 0.7, -blur_size.x * 0.7, position.x);
        float highlight_mask = smoothstep(-blur_size.y * 0.7, blur_size.y * 0.7, position.y)
            * smoothstep(-blur_size.x * 0.7, blur_size.x * 0.7, position.x);

        float corner = thickness_shadow * 0.35;
        float corner_spec = clamp(
            corner * (0.08 + lum * 0.6) * lg_rim_strength * lg_rim_color.a,
            0.0,
            1.0
        );

        glow = mix(glow, lg_rim_color.rgb, corner_spec * shadow_mask);
        glow = mix(glow, lg_rim_color.rgb, corner_spec * highlight_mask);
    }

    return glow;
}

vec4 glass_effect(
    vec2 uv_tex,
    vec2 uv_geo,
    vec4 base_color,
    vec2 blur_size,
    vec4 radius,
    float refraction_strength,
    float refraction_normal_pow,
    float refraction_rgb_fringing,
    float refraction_offset_strength,
    float refraction_bevel_intensity,
    float physically_based_refraction,
    float glow_strength,
    float edge_lighting,
    float bevel_width
) {
    vec2 half_size = blur_size * 0.5;
    float min_half_size = min(half_size.x, half_size.y);

    vec2 position = uv_geo * blur_size - half_size.xy;
    position.y = -position.y;
    float dist = rounded_rectangle_dist(position, half_size, radius);

    if (dist >= 0.0) {
        return base_color;
    }

    float relative_band = clamp(min_half_size * 0.15 * bevel_width, 0.1, min_half_size * 0.9);
    float band_width = lg_bevel_px > 0.0
        ? clamp(lg_bevel_px, 0.1, min_half_size * 0.9)
        : relative_band;
    float edge_factor = 1.0 - clamp(abs(dist) / band_width, 0.0, 1.0);
    float concave_factor = 1.0
        - sqrt(1.0 - pow(smoothstep(0.0, 1.0, edge_factor), refraction_normal_pow));

    GlassFragment sample;
    if (refraction_strength > 0.0) {
        vec4 sdf_radius = clamp(radius * 2.0, min(64.0, min_half_size), min(128.0, min_half_size));
        sample = physically_based_refraction < 0.5
            ? glass_refraction(
                uv_tex,
                position,
                half_size,
                sdf_radius,
                dist,
                edge_factor,
                concave_factor,
                refraction_strength,
                refraction_offset_strength,
                refraction_rgb_fringing
            )
            : snells_refraction(
                uv_tex,
                position,
                half_size,
                sdf_radius,
                dist,
                edge_factor,
                concave_factor,
                refraction_strength,
                refraction_bevel_intensity,
                refraction_offset_strength,
                refraction_rgb_fringing,
                band_width
            );
    } else {
        sample = GlassFragment(
            base_color,
            dist,
            edge_factor,
            concave_factor,
            vec3(0.0, 0.0, 1.0),
            1.0
        );
    }

    sample.color = apply_tint(sample.color, uv_geo);

    if (lg_inner_shadow > 0.0) {
        // Darken the inner side of the bevel to give the pane some depth.
        float inner = smoothstep(0.0, band_width * 2.0, abs(dist));
        float shadow = lg_inner_shadow * (1.0 - inner);
        sample.color.rgb *= 1.0 - shadow * 0.5;
    }

    vec3 rgb = sample.concaveFactor <= 1.0
        ? glass_outline(position, blur_size, sample, glow_strength, edge_lighting)
        : sample.color.rgb;

    return apply_sheen(vec4(rgb, sample.color.a), uv_geo);
}

void main() {
    vec3 coords_geo = input_to_geo * vec3(v_coords, 1.0);

    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    float inside_geo = step(0.0, coords_geo.x) * step(coords_geo.x, 1.0)
        * step(0.0, coords_geo.y) * step(coords_geo.y, 1.0);
    float lg_enabled = step(0.0001, lg_refraction_strength);

    if (inside_geo * lg_enabled > 0.0) {
        // Interior lens: bend sampling toward the center, strongest at the corners, so it
        // blends continuously into the edge bevel. Capped to avoid smearing past the texture.
        vec2 uv_tex = v_coords;
        if (abs(lg_interior_warp) > 0.0001) {
            vec2 d = v_coords - 0.5;
            float r2 = dot(d, d);
            vec2 off = d * (lg_interior_warp * min(r2, 0.35));
            off = clamp(off, vec2(-0.12), vec2(0.12));
            uv_tex -= off;
        }

        float norm_strength = clamp(lg_refraction_strength * 0.05, 0.0, 1.0);
        vec4 remapped_radius = vec4(
            corner_radius.w,
            corner_radius.z,
            corner_radius.x,
            corner_radius.y
        );

        color = glass_effect(
            uv_tex,
            coords_geo.xy,
            color,
            geo_size,
            remapped_radius,
            norm_strength,
            lg_power_factor,
            lg_fringing,
            lg_refraction_power,
            lg_refraction_power,
            lg_physical_refraction,
            lg_glow_weight,
            lg_edge_lighting,
            lg_bevel_width
        );
    } else if (inside_geo * step(0.0001, tint_opacity + lg_sheen_strength) > 0.0) {
        // Tint and sheen work independently of refraction, so a plain blurred
        // panel can still get the gradient and top light.
        color = apply_tint(color, coords_geo.xy);
        color = apply_sheen(color, coords_geo.xy);
    }

    color = postprocess(color);
    color = color * niri_rounding_alpha(coords_geo.xy * geo_size, geo_size, corner_radius)
        * inside_geo;
    color = color * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
