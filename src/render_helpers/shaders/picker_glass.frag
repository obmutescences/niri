precision highp float;

#if defined(DEBUG_FLAGS)
uniform float niri_tint;
#endif

uniform float niri_alpha;
uniform float niri_scale;
uniform vec2 geo_size;
uniform vec4 corner_radius;
uniform vec4 fill_color;
uniform vec4 border_color;
uniform vec4 accent_color;
// 0 = normal card, 0..1 = focused/hovered accent rim & gentle glow lift
uniform float highlight;
// Vertical position (in logical px from top) of optional divider line above info strip (<= 0 disables)
uniform float divider_y;
// Extra subtle dark scrim opacity for the bottom info strip to keep text high-contrast
uniform float scrim_alpha;

varying vec2 niri_v_coords;

float niri_rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);

float sd_rounded_box(vec2 p, vec2 b, vec4 cr) {
    float r = (p.x > 0.0)
        ? ((p.y < 0.0) ? cr.y : cr.z)
        : ((p.y < 0.0) ? cr.x : cr.w);
    r = clamp(r, 0.0, min(b.x, b.y));
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

float ign(vec2 px) {
    return fract(52.9829189 * fract(dot(px, vec2(0.06711056, 0.00583715))));
}

void main() {
    vec2 size = max(geo_size, vec2(1.0));
    vec2 px = niri_v_coords * size;
    vec2 half_size = size * 0.5;
    float dist = sd_rounded_box(px - half_size, half_size, corner_radius);

    // Only evaluate fill effects when a fill color is explicitly requested (`fill_color.a > 0`).
    // When `fill_color.a == 0` (pure frosted blur cards / HUD / group headers), `fill_pm` stays
    // exact zero so zero background color or tint leaks onto the live blur underneath.
    vec3 base_rgb = vec3(0.0);
    float base_a = fill_color.a;
    if (base_a > 0.001) {
        float ts = clamp(1.0 - niri_v_coords.y * 1.45, 0.0, 1.0);
        float top_sheen = ts * ts;
        base_rgb = fill_color.rgb + vec3(0.08, 0.10, 0.13) * top_sheen;
    }

    // Optional bottom info-strip scrim + separator (disabled when `divider_y <= 0.5`).
    if (divider_y > 0.5 && scrim_alpha > 0.001) {
        float in_info = smoothstep(divider_y - 1.0, divider_y + 2.0, px.y);
        float extra_a = scrim_alpha * in_info;
        base_rgb = mix(base_rgb, vec3(0.05, 0.07, 0.10), extra_a * 0.72);
        base_a = clamp(base_a + extra_a * (1.0 - base_a), 0.0, 1.0);
    }

    if (base_a > 0.001) {
        float grain = (ign(px * niri_scale) - 0.5) * 0.018;
        base_rgb = clamp(base_rgb + grain, 0.0, 1.0);
    }

    // Scale-aware 1.25px directional glass rim border (transitions to accent on hover/focus).
    float aa = 1.0 / max(niri_scale, 0.5);
    float rim_width = mix(1.25, 1.85, highlight) * aa;
    float rim_mask = smoothstep(0.0, -aa * 0.9, dist)
        * (1.0 - smoothstep(-rim_width, -(rim_width + aa * 1.1), dist));

    vec2 p_norm = normalize((px - half_size) / max(half_size, vec2(1.0)) + vec2(0.0001));
    float dir_light = 0.55 + 0.45 * max(dot(p_norm, normalize(vec2(-0.55, -0.83))), 0.0);

    vec4 rim_col = mix(
        vec4(border_color.rgb, border_color.a * dir_light),
        vec4(accent_color.rgb, clamp(accent_color.a * (0.65 + 0.35 * dir_light), 0.0, 1.0)),
        highlight
    );

    // Subtle top-edge inner lift when highlighted (only when fill is active).
    if (base_a > 0.001 && highlight > 0.001) {
        float inner_glow = (1.0 - smoothstep(0.0, 18.0, -dist)) * highlight * 0.14;
        base_rgb = mix(base_rgb, accent_color.rgb, inner_glow);
    }

    // Premultiplied compositing of rim over glass fill.
    vec4 fill_pm = vec4(base_rgb * base_a, base_a);
    float ra = clamp(rim_col.a * rim_mask, 0.0, 1.0);
    vec4 color = vec4(rim_col.rgb * ra, ra) + fill_pm * (1.0 - ra);

    color *= niri_rounding_alpha(px, size, corner_radius);
    color *= niri_alpha;

#if defined(DEBUG_FLAGS)
    if (niri_tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
