// Layer pass: one textured quad per picture, blended in linear light.
struct Layer {
    // Half-size of the quad in clip space (xy) and its centre (zw).
    rect: vec4<f32>,
    // 2x2 matrix turning quad coordinates into texture coordinates.
    uv: vec4<f32>,
    // x: opacity
    params: vec4<f32>,
};
@group(0) @binding(0) var<uniform> layer: Layer;
@group(0) @binding(1) var picture: texture_2d<f32>;
@group(0) @binding(2) var picture_sampler: sampler;

struct V {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn layer_vs(@builtin(vertex_index) i: u32) -> V {
    // Corners of a strip: (-1,-1) (1,-1) (-1,1) (1,1).
    let corner = vec2<f32>(f32(i & 1u) * 2.0 - 1.0, f32((i >> 1u) & 1u) * 2.0 - 1.0);
    var out: V;
    out.pos = vec4<f32>(layer.rect.zw + corner * layer.rect.xy, 0.0, 1.0);
    // Texture space has y down; quad space has y up.
    let q = vec2<f32>(corner.x, -corner.y);
    out.uv = vec2<f32>(dot(layer.uv.xy, q), dot(layer.uv.zw, q)) * 0.5 + vec2<f32>(0.5);
    return out;
}

// Pictures arrive display-referred (BT.709 video shown as on an sRGB
// screen). Decode with the sRGB curve to get linear light.
fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn to_display(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, x <= vec3<f32>(0.0031308));
}

@fragment
fn layer_fs(v: V) -> @location(0) vec4<f32> {
    let texel = textureSample(picture, picture_sampler, v.uv);
    let alpha = texel.a * layer.params.x;
    // Premultiplied, to match the blend state.
    return vec4<f32>(to_linear(texel.rgb) * alpha, alpha);
}

// Output pass: the linear composite, encoded for the display.
@vertex
fn output_vs(@builtin(vertex_index) i: u32) -> V {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: V;
    out.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(p.x, 1.0 - p.y);
    return out;
}

@fragment
fn output_fs(v: V) -> @location(0) vec4<f32> {
    let c = textureSample(picture, picture_sampler, v.uv);
    return vec4<f32>(to_display(c.rgb), 1.0);
}
