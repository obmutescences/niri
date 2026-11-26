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

// Perspective card parameters (mathematically identical to picker_card.frag).
uniform vec2 pb_card_size;
uniform vec2 pb_dest_center;
uniform vec2 pb_persp_half_size;
uniform float pb_angle;
uniform float pb_card_scale;
uniform vec2 pb_tilt;
uniform float pb_flip_y;

vec2 rotate2d(vec2 p, float a) {
    float c = cos(a);
    float s = sin(a);
    return vec2(c * p.x - s * p.y, s * p.x + c * p.y);
}

// Signed distance to a rounded rectangle centered at origin.
// cr = (top_left, top_right, bottom_right, bottom_left).
float sd_rounded_box(vec2 p, vec2 b, vec4 cr) {
    float r = (p.x > 0.0)
        ? ((p.y < 0.0) ? cr.y : cr.z)
        : ((p.y < 0.0) ? cr.x : cr.w);
    r = clamp(r, 0.0, min(b.x, b.y));
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    vec3 coords_geo = input_to_geo * vec3(v_coords, 1.0);
    vec2 uv_geo = coords_geo.xy;

    // Exact screen coordinate relative to the card's pivot (identical to picker_card.frag).
    vec2 p = (uv_geo - 0.5) * geo_size + pb_dest_center;

    // Exact inverse 3D tilt + Y-axis flip + 2D rotation + scale.
    float cos_flip = cos(pb_flip_y);
    float sin_flip = sin(pb_flip_y);
    float flip_scale_x = max(abs(cos_flip), 0.04);

    vec2 norm_p = p / max(pb_persp_half_size, vec2(1.0));
    float persp_tilt = dot(norm_p, pb_tilt) * 0.08;
    float persp_flip = norm_p.x * sin_flip * 0.22;
    float persp = max(1.0 + persp_tilt + persp_flip, 0.35);

    vec2 p3d = p / persp;
    p3d.x /= flip_scale_x;

    vec2 q = rotate2d(p3d, -pb_angle) / max(pb_card_scale, 0.0001);
    vec2 half_card = pb_card_size * 0.5;
    float dist = sd_rounded_box(q, half_card, corner_radius);

    // Unified 1-physical-pixel anti-aliased rounded rectangle mask matching rounding_alpha.frag.
    float t = clamp(dist * max(niri_scale * pb_card_scale, 0.5) + 0.5, 0.0, 1.0);
    float mask = 1.0 - t * t * (3.0 - 2.0 * t);
    if (mask <= 0.001) {
        gl_FragColor = vec4(0.0);
        return;
    }

    vec4 color = texture2D(tex, clamp(v_coords, vec2(0.0005), vec2(0.9995)));
#if defined(NO_ALPHA)
    color.a = 1.0;
#endif
    color *= mask * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
