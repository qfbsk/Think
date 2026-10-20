struct Background {
    size: vec2<f32>,
    image: vec2<f32>,
    tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> background: Background;
@group(0) @binding(1) var image: texture_2d<f32>;
@group(0) @binding(2) var image_sampler: sampler;

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let position = corners[index];
    let normalized = vec2<f32>(
        (position.x + 1.0) * 0.5,
        (1.0 - position.y) * 0.5,
    );
    let cover = max(background.size.x / background.image.x, background.size.y / background.image.y);
    let visible = background.size / cover;
    let origin = (background.image - visible) * 0.5;

    var out: VertexOut;
    out.clip = vec4<f32>(position, 0.0, 1.0);
    out.uv = (origin + normalized * visible) / background.image;
    return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let color = textureSample(image, image_sampler, input.uv).rgb;
    return vec4<f32>(mix(color, background.tint.rgb, background.tint.a), 1.0);
}
