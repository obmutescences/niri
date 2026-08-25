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

uniform float lg_refraction_strength;
uniform float lg_power_factor;
uniform float lg_refraction_a;
uniform float lg_refraction_b;
uniform float lg_refraction_c;
uniform float lg_refraction_d;
uniform float lg_refraction_power;
uniform float lg_physical_refraction;
uniform float lg_glow_weight;
uniform float lg_glow_bias;
uniform float lg_glow_edge0;
uniform float lg_glow_edge1;
uniform float lg_edge_lighting;
uniform float lg_fringing;
uniform float lg_bevel_width;

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
    float min_half_size,
    float dist,
    float edge_factor,
    float concave_factor,
    float refraction_strength,
    float refraction_bevel_intensity,
    float refraction_offset_strength,
    float refraction_rgb_fringing,
    float bevel_width
) {
    float band_width = max(min_half_size * 0.15 * bevel_width, 4.0);
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

    float rim_mask = clamp(0.25 * sample.concaveFactor, 0.0, glow_strength);

    // Specular highlight: mix towards white, stronger on light content.
    float spec_strength = rim_mask * edge_lighting * (0.08 + lum * 0.7);
    vec3 glow = mix(sample.color.rgb, vec3(1.0), spec_strength);

    // Edge brightness boost: replaces the old additive doubling with a
    // multiplicative boost that is stronger on dark content (to keep glass
    // edges visible) and weaker on light content (which is already bright).
    if (edge_lighting > 0.5) {
        float boost = sample.concaveFactor * 0.4 * (1.0 - lum * 0.6);
        glow = glow * (1.0 + boost);
    }

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
        float corner_spec = corner * (0.08 + lum * 0.6);

        glow = mix(glow, vec3(1.0), corner_spec * shadow_mask);
        glow = mix(glow, vec3(1.0), corner_spec * highlight_mask);
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

    float band_width = clamp(min_half_size * 0.15 * bevel_width, 0.1, min_half_size * 0.9);
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
                min_half_size,
                dist,
                edge_factor,
                concave_factor,
                refraction_strength,
                refraction_bevel_intensity,
                refraction_offset_strength,
                refraction_rgb_fringing,
                bevel_width
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

    vec3 rgb = sample.concaveFactor <= 1.0
        ? glass_outline(position, blur_size, sample, glow_strength, edge_lighting)
        : sample.color.rgb;

    return vec4(rgb, sample.color.a);
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
        float norm_strength = clamp(lg_refraction_strength * 0.05, 0.0, 1.0);
        vec4 remapped_radius = vec4(
            corner_radius.w,
            corner_radius.z,
            corner_radius.x,
            corner_radius.y
        );

        color = glass_effect(
            v_coords,
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
