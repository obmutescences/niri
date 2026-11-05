precision highp float;

uniform float niri_alpha;
uniform sampler2D tex;
uniform vec2 src_center;
uniform vec2 src_size;
uniform vec2 dest_size;
uniform float angle;
uniform float card_scale;

varying vec2 niri_v_coords;

vec2 rotate(vec2 p, float a) {
    float c = cos(a);
    float s = sin(a);
    return vec2(c * p.x - s * p.y, s * p.x + c * p.y);
}

void main() {
    vec2 p = (niri_v_coords - 0.5) * dest_size;
    vec2 q = rotate(p, -angle) / max(card_scale, 0.0001);
    vec2 uv = (src_center + q) / src_size;

    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        discard;
    }

    vec4 color = texture2D(tex, uv);
    color *= niri_alpha;
    gl_FragColor = color;
}
