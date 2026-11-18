// Layer pass: one textured quad per picture, blended in linear light.
struct Layer {
    // 2x2 matrix (rows) from quad corners to clip space.
    matrix: vec4<f32>,
    // xy: centre in clip space, z: opacity.
    offset: vec4<f32>,
    // 2x2 matrix (rows) from quad corners to texture coordinate offsets.
    uv: vec4<f32>,
    uv_centre: vec4<f32>,
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
    let p = vec2<f32>(dot(layer.matrix.xy, corner), dot(layer.matrix.zw, corner));
    out.pos = vec4<f32>(layer.offset.xy + p, 0.0, 1.0);
    // Texture space has y down; quad space has y up.
    let q = vec2<f32>(corner.x, -corner.y);
    out.uv = layer.uv_centre.xy + vec2<f32>(dot(layer.uv.xy, q), dot(layer.uv.zw, q));
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
    let alpha = texel.a * layer.offset.z;
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

// Delivery passes for export: the frame as limited-range BT.709 Y'CbCr,
// written as the two planes of NV12 so the encoder gets what it wants
// without a conversion on the CPU.
fn display_rgb(uv: vec2<f32>) -> vec3<f32> {
    return to_display(textureSample(picture, picture_sampler, uv).rgb);
}

@fragment
fn luma_fs(v: V) -> @location(0) vec4<f32> {
    let c = display_rgb(v.uv);
    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>((16.0 + 219.0 * y) / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn chroma_fs(v: V) -> @location(0) vec4<f32> {
    // Rendered at half size with a linear sampler: each texel is the mean
    // of the four pixels it covers.
    let c = display_rgb(v.uv);
    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let cb = (c.b - y) / 1.8556;
    let cr = (c.r - y) / 1.5748;
    return vec4<f32>((128.0 + 224.0 * cb) / 255.0, (128.0 + 224.0 * cr) / 255.0, 0.0, 1.0);
}
