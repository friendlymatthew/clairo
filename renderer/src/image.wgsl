struct ImageVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;

@vertex
fn vertex(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> ImageVertex {
    return ImageVertex(vec4<f32>(position, 0.0, 1.0), uv);
}

@fragment
fn fragment(in: ImageVertex) -> @location(0) vec4<f32> {
    return textureSample(image, image_sampler, in.uv);
}
