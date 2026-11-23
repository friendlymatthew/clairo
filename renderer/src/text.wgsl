struct CurveOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) side: f32,
}

@vertex
fn vs_curve(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) side: f32) -> CurveOutput {
    return CurveOutput(vec4<f32>(position, 0.0, 1.0), uv, side);
}

@fragment
fn fs_curve(in: CurveOutput) -> @location(0) vec4<f32> {
    if in.side != 0.0 && in.side * (in.uv.x * in.uv.x - in.uv.y) < 0.0 {
        discard;
    }
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
