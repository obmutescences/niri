precision highp float;

uniform float niri_alpha;
uniform float niri_scale;
uniform sampler2D tex;
uniform vec2 src_size;
uniform vec2 tex_size;
uniform vec2 src_pivot;
uniform vec2 dest_size;
uniform vec2 dest_center;
uniform vec2 persp_half_size;
uniform vec2 card_half_size;
uniform float angle;
uniform float card_scale;
// Pseudo-3D perspective tilt in [-1, 1] (x = yaw/horizontal tilt, y = pitch/vertical tilt)
uniform vec2 tilt;
// True 3D Y-axis flip angle in radians (0 = flat facing viewer)
uniform float flip_y;
// Dynamic glass specular glint strength on hover (0..1)
uniform float glint_strength;

varying vec2 niri_v_coords;

vec2 rotate2d(vec2 p, float a) {
    float c = cos(a);
    float s = sin(a);
    return vec2(c * p.x - s * p.y, s * p.x + c * p.y);
}

void main() {
    // Reconstruct exact coordinate relative to the card's true rotation pivot.
    vec2 p = (niri_v_coords - 0.5) * dest_size + dest_center;

    // Pseudo-3D Y-axis card flip (used by Flip motion style) combined with subtle hover tilt.
    float cos_flip = cos(flip_y);
    float sin_flip = sin(flip_y);
    float flip_scale_x = max(abs(cos_flip), 0.04);

    vec2 norm_p = p / max(persp_half_size, vec2(1.0));
    float persp_tilt = dot(norm_p, tilt) * 0.08;
    float persp_flip = norm_p.x * sin_flip * 0.22;
    float persp = max(1.0 + persp_tilt + persp_flip, 0.35);

    vec2 p3d = p / persp;
    p3d.x /= flip_scale_x;

    // Inverse 2D rotation and scale around src_pivot.
    vec2 q = rotate2d(p3d, -angle) / max(card_scale, 0.0001);
    vec2 px = src_pivot + q;
    vec2 uv = px / max(tex_size, vec2(1.0));
    vec2 local_uv = (q + card_half_size) / max(card_half_size * 2.0, vec2(1.0));

    // Smooth 1-physical-pixel edge anti-aliasing instead of hard aliased discard.
    float aa_px = 1.0 / max(niri_scale * card_scale, 0.5);
    vec2 edge_d = min(px, src_size - px);
    float edge_alpha = smoothstep(0.0, aa_px, min(edge_d.x, edge_d.y));
    if (edge_alpha <= 0.001) {
        discard;
    }

    vec4 color = texture2D(tex, clamp(uv, vec2(0.0), vec2(1.0)));

    // Subtle moving diagonal glass specular glint following hover 3D tilt (card surface only).
    if (glint_strength > 0.001 && color.a > 0.001
        && abs(q.x) <= card_half_size.x && abs(q.y) <= card_half_size.y) {
        vec2 centered = (local_uv - 0.5) * 2.0;
        float d_beam = dot(centered - tilt * 0.55, vec2(0.68, 0.73));
        float beam = exp(-(d_beam * d_beam) * 6.5);
        color.rgb += vec3(0.95, 0.98, 1.0) * (beam * 0.14 * glint_strength * color.a);
    }

    // Slight foreshortening dim near 90-degree 3D flip edge for depth realism.
    float flip_shade = mix(0.68, 1.0, abs(cos_flip));
    color.rgb *= flip_shade;

    gl_FragColor = color * (niri_alpha * edge_alpha);
}
