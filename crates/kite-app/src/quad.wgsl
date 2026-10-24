struct Screen {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> screen: Screen;

struct VertexIn {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) rect: vec4<f32>,
    @location(3) radius: f32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local: vec2<f32>,
    @location(2) half: vec2<f32>,
    @location(3) radius: f32,
};

@vertex
fn vs_main(input: VertexIn) -> VertexOut {
    var out: VertexOut;
    let ndc = vec2<f32>(
        input.position.x / screen.size.x * 2.0 - 1.0,
        1.0 - input.position.y / screen.size.y * 2.0,
    );
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.color = input.color;
    out.local = input.position - input.rect.xy;
    out.half = input.rect.zw * 0.5;
    out.radius = min(input.radius, min(out.half.x, out.half.y));
    return out;
}

/// Rounded where a quad asks for it, with a pixel of feather on the edge;
/// every other quad keeps the plain hard rectangle it always had.
@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    if (input.radius <= 0.0) {
        return input.color;
    }
    let q = abs(input.local - input.half) - (input.half - vec2<f32>(input.radius));
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - input.radius;
    if (d > 0.5) {
        discard;
    }
    var color = input.color;
    color.a = color.a * smoothstep(0.5, -0.5, d);
    return color;
}
