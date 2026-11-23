use anyhow::{Context, Result};
use font::TrueTypeFontParser;
use wgpu::util::DeviceExt;

pub struct TextRenderer {
    pipeline: wgpu::RenderPipeline,
    vertices: Option<wgpu::Buffer>,
    vertex_count: u32,
    width: f32,
    height: f32,
}

impl TextRenderer {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        text: &str,
    ) -> Result<Self> {
        let shader = device.create_shader_module(wgpu::include_wgsl!("text.wgsl"));

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glyph geometry"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_curve"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<font::GlyphVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_curve"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let font = TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf"))
            .parse()
            .context("could not parse Lato")?;

        let geometry = crate::layout::layout_text(&font, text)?;
        let vertices = (!geometry.vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glyph triangles"),
                contents: bytemuck::cast_slice(&geometry.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });

        Ok(Self {
            pipeline,
            vertices,
            vertex_count: geometry.vertices.len().try_into()?,
            width: geometry.width,
            height: geometry.height,
        })
    }

    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("page text"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        let scale = ((width as f32) / self.width)
            .min((height as f32) / self.height)
            .min(1.0);

        pass.set_viewport(0., 0., self.width * scale, self.height * scale, 0., 1.);
        pass.set_pipeline(&self.pipeline);
        if let Some(vertices) = &self.vertices {
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.draw(0..self.vertex_count, 0..1);
        }
    }
}
