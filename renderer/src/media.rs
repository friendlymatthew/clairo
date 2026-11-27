use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use markup5ever_rcdom::{Handle, NodeData};
use resvg::{tiny_skia, usvg};
use url::Url;
use wgpu::util::DeviceExt;

pub(crate) type Images = HashMap<String, Raster>;

pub(crate) struct Raster {
    pub width: u32,
    pub height: u32,
    pixels: Vec<u8>,
}

pub(crate) fn fetch_images(root: &Handle, base: &Url) -> Images {
    let mut images = Images::new();
    let mut pending = vec![root.clone()];
    let mut seen = std::collections::HashSet::new();
    while let Some(node) = pending.pop() {
        if let NodeData::Element { name, attrs, .. } = &node.data
            && name.local.as_ref() == "img"
            && let Some(src) = attrs
                .borrow()
                .iter()
                .find(|attr| attr.name.local.as_ref() == "src")
            && seen.insert(src.value.to_string())
            && let Ok(url) = base.join(src.value.trim())
        {
            let path = url.path().to_ascii_lowercase();
            let thumbnail_size = path
                .rsplit('/')
                .next()
                .and_then(|file| file.split_once("px-"))
                .and_then(|(size, _)| size.parse::<u32>().ok());
            let svg = path.ends_with(".svg")
                || (path.contains(".svg/") && thumbnail_size.is_none_or(|size| size >= 128));
            // Raster images are disabled until Clairo's decoder is ready.
            if svg {
                match fetch_svg(url) {
                    Ok(image) => {
                        images.insert(src.value.to_string(), image);
                    }
                    Err(error) => eprintln!("could not load image {}: {error:#}", src.value),
                }
            }
        }
        pending.extend(node.children.borrow().iter().rev().cloned());
    }
    images
}

fn fetch_svg(mut url: Url) -> Result<Raster> {
    if url.host_str() == Some("thumb.wikimedia.org")
        && let Some((directory, file)) = url.path().split_once("/thumb/")
        && let Some((original, _)) = file.rsplit_once('/')
    {
        url = Url::parse(&format!(
            "https://upload.wikimedia.org{directory}/{original}"
        ))?;
    }

    let (_, response) = net::http1::get_following_redirects(url.as_str())?;
    let bytes = response.decoded_body()?;
    let tree = usvg::Tree::from_data(&bytes, &usvg::Options::default())?;
    let scale = (512.0 / tree.size().width().max(tree.size().height())).min(4.0);
    let width = (tree.size().width() * scale).ceil() as u32;
    let height = (tree.size().height() * scale).ceil() as u32;
    ensure!(
        width <= 512 && height <= 512,
        "SVG raster exceeds image bounds"
    );
    let mut pixmap = tiny_skia::Pixmap::new(width, height).context("invalid SVG size")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia stores premultiplied RGBA. Composite on the page's white background.
    for pixel in pixmap.data_mut().chunks_exact_mut(4) {
        let white = 255 - pixel[3];
        for channel in &mut pixel[..3] {
            *channel = channel.saturating_add(white);
        }
        pixel[3] = 255;
    }
    Ok(Raster {
        width,
        height,
        pixels: pixmap.take(),
    })
}

pub(crate) struct ImageBox {
    pub src: String,
    pub href: Option<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

pub(crate) struct ImageRenderer {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    textures: HashMap<String, wgpu::BindGroup>,
}

impl ImageRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("image.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("article images"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: None,
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: None,
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            sampler,
            textures: HashMap::new(),
        }
    }

    pub fn set_images(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, images: &Images) {
        self.textures.clear();
        let layout = self.pipeline.get_bind_group_layout(0);
        for (src, image) in images {
            let texture = device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some(src),
                    size: wgpu::Extent3d {
                        width: image.width,
                        height: image.height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &image.pixels,
            );
            let view = texture.create_view(&Default::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(src),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.textures.insert(src.clone(), group);
        }
    }

    pub fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        images: &[ImageBox],
        viewport: &crate::layout::Viewport,
        scroll: f32,
    ) {
        let mut vertices = Vec::<[f32; 4]>::new();
        let mut textures = Vec::new();
        for image in images {
            let y = image.y - scroll;
            if y >= viewport.height || y + image.height <= 0.0 {
                continue;
            }
            let Some(texture) = self.textures.get(&image.src) else {
                continue;
            };
            let x0 = image.x * 2.0 / viewport.width - 1.0;
            let x1 = (image.x + image.width) * 2.0 / viewport.width - 1.0;
            let y0 = 1.0 - y * 2.0 / viewport.height;
            let y1 = 1.0 - (y + image.height) * 2.0 / viewport.height;
            let top_left = [x0, y0, 0.0, 0.0];
            let top_right = [x1, y0, 1.0, 0.0];
            let bottom_left = [x0, y1, 0.0, 1.0];
            let bottom_right = [x1, y1, 1.0, 1.0];
            vertices.extend([
                top_left,
                bottom_left,
                top_right,
                top_right,
                bottom_left,
                bottom_right,
            ]);
            textures.push(texture);
        }
        if vertices.is_empty() {
            return;
        }
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("image quads"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        for (index, texture) in (0u32..).zip(textures) {
            pass.set_bind_group(0, texture, &[]);
            pass.draw(index * 6..index * 6 + 6, 0..1);
        }
    }
}
