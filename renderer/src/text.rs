use std::{
    collections::{HashMap, hash_map::Entry},
    rc::Rc,
};

use crate::{
    layout::{LayoutTree, Viewport},
    style::Color,
};
use anyhow::Result;
use font::{
    grammar::TrueTypeFontFile,
    mesh::{GlyphVertex, mesh},
    outline,
    shaper::TrueTypeFontShaper,
};
use wgpu::util::DeviceExt;

pub struct TextRenderer {
    pipeline: wgpu::RenderPipeline,
    vertices: Option<wgpu::Buffer>,
    vertex_count: u32,
    samples: u32,
    target: Option<wgpu::Texture>,
    glyph_meshes: GlyphMeshCache,
}

impl TextRenderer {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        vertices: &[TextVertex],
        samples: u32,
    ) -> Result<Self> {
        let shader = device.create_shader_module(wgpu::include_wgsl!("text.wgsl"));

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glyph geometry"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_curve"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TextVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32, 3 => Float32x3],
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
            multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
            multiview_mask: None,
            cache: None,
        });

        let mut renderer = Self {
            pipeline,
            vertices: None,
            vertex_count: 0,
            samples,
            target: None,
            glyph_meshes: GlyphMeshCache::new(),
        };
        renderer.update_vertices(device, vertices)?;
        Ok(renderer)
    }

    pub(crate) fn mesh_layout(
        &mut self,
        tree: &LayoutTree,
        font: &TrueTypeFontFile<'_>,
        viewport: &Viewport,
        scroll: f32,
    ) -> Result<(Vec<TextVertex>, Vec<LinkHit>)> {
        mesh_layout(tree, font, viewport, &mut self.glyph_meshes, scroll)
    }

    pub fn update_vertices(
        &mut self,
        device: &wgpu::Device,
        vertices: &[TextVertex],
    ) -> Result<()> {
        let vertex_count = vertices.len().try_into()?;
        self.vertices = (!vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("glyph triangles"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        self.vertex_count = vertex_count;
        Ok(())
    }

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
    ) {
        let size = view.texture().size();
        if self.samples > 1
            && self
                .target
                .as_ref()
                .is_none_or(|target| target.size() != size)
        {
            self.target = Some(device.create_texture(&wgpu::TextureDescriptor {
                label: Some("multisampled article text"),
                size,
                mip_level_count: 1,
                sample_count: self.samples,
                dimension: wgpu::TextureDimension::D2,
                format: view.texture().format(),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            }));
        }
        let target = self
            .target
            .as_ref()
            .map(|target| target.create_view(&Default::default()));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("page text"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.as_ref().unwrap_or(view),
                depth_slice: None,
                resolve_target: target.as_ref().map(|_| view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: if target.is_some() {
                        wgpu::StoreOp::Discard
                    } else {
                        wgpu::StoreOp::Store
                    },
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(&self.pipeline);
        if let Some(vertices) = &self.vertices {
            pass.set_vertex_buffer(0, vertices.slice(..));
            pass.draw(0..self.vertex_count, 0..1);
        }
    }
}

type GlyphMeshCache = HashMap<u16, Vec<GlyphVertex>>;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct TextVertex {
    glyph: GlyphVertex,
    color: Color,
}

pub(crate) struct LinkHit {
    pub href: Rc<str>,
    bounds: [f32; 4],
}

impl LinkHit {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let [left, top, right, bottom] = self.bounds;
        x >= left && x < right && y >= top && y < bottom
    }
}

pub(crate) fn mesh_layout(
    tree: &LayoutTree,
    font: &TrueTypeFontFile<'_>,
    viewport: &Viewport,
    glyph_meshes: &mut GlyphMeshCache,
    scroll: f32,
) -> Result<(Vec<TextVertex>, Vec<LinkHit>)> {
    let mut vertices = Vec::new();
    let mut hits = Vec::new();

    if viewport.width <= 0.0 || viewport.height <= 0.0 {
        return Ok((vertices, hits));
    }

    for decoration in &tree.decorations {
        let y = decoration.y - scroll;
        if y >= viewport.height || y + decoration.height <= 0.0 {
            continue;
        }
        let [x0, y0] = clip_position(decoration.x, y, viewport);
        let [x1, y1] = clip_position(
            decoration.x + decoration.width,
            y + decoration.height,
            viewport,
        );
        for position in [[x0, y0], [x0, y1], [x1, y0], [x1, y0], [x0, y1], [x1, y1]] {
            vertices.push(TextVertex {
                glyph: GlyphVertex {
                    position,
                    uv: [0.0; 2],
                    side: 0.0,
                },
                color: decoration.color,
            });
        }
    }

    let shaper = TrueTypeFontShaper::from(font);
    for block in &tree.blocks {
        for line in &block.lines {
            let baseline = line.baseline_y - scroll;
            let overscan = line.font_size_px * 3.0;
            if baseline + overscan <= 0.0 || baseline - overscan >= viewport.height {
                continue;
            }

            let mut pen_x = block.x + line.offset;
            let mut spans = line.content.spans.iter().peekable();
            for (byte, ch) in line.content.text.char_indices() {
                while spans.peek().is_some_and(|span| byte >= span.bytes.end) {
                    spans.next();
                }
                let span = spans.peek().filter(|span| span.bytes.contains(&byte));
                let paint = span.map_or(line.paint, |span| span.paint);
                let glyph = shaper.shape_char(ch);
                let scale = paint.size / f32::from(font.head_table.units_per_em);
                let advance = f32::from(glyph.advance_width) * scale;
                let baseline = if paint.raised {
                    baseline - paint.size * 0.4
                } else {
                    baseline
                };
                if let Some(href) = span.and_then(|span| span.href.as_ref()) {
                    hits.push(LinkHit {
                        href: href.clone(),
                        bounds: [
                            pen_x,
                            baseline - f32::from(font.hhea_table.ascent) * scale,
                            pen_x + advance,
                            baseline - f32::from(font.hhea_table.descent) * scale,
                        ],
                    });
                }

                let glyph_vertices = match glyph_meshes.entry(glyph.glyph_id) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(mesh(&outline(font, glyph.glyph_id)?)?),
                };
                for mut vertex in glyph_vertices.iter().copied() {
                    let [x, y] = vertex.position;
                    vertex.position =
                        clip_position(pen_x + x * scale, baseline - y * scale, viewport);
                    vertices.push(TextVertex {
                        glyph: vertex,
                        color: paint.color,
                    });
                }
                pen_x += advance;
            }
        }
    }

    Ok((vertices, hits))
}

fn clip_position(x: f32, y: f32, viewport: &Viewport) -> [f32; 2] {
    [
        2.0 * x / viewport.width - 1.0,
        1.0 - 2.0 * y / viewport.height,
    ]
}
