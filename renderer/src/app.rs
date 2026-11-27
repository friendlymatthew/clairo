use std::{sync::Arc, time::Instant};

use anyhow::{Context, Result, bail};
use font::{TrueTypeFontParser, grammar::TrueTypeFontFile};
use markup5ever_rcdom::Handle;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use crate::{
    layout::{GlyphMeshCache, Viewport, layout_document, mesh_layout},
    style::ElementStyles,
    text::TextRenderer,
};

/// the entry point to all of my problems
pub fn run(dom: &Handle) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        page: Page::new(dom),
        renderer: None,
        error: None,
    };

    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

struct Page {
    dom: Handle,
    styles: ElementStyles,
}

impl Page {
    fn new(dom: &Handle) -> Self {
        let started = Instant::now();
        let styles = ElementStyles::compute(dom);
        eprintln!("style computation took {:?}", started.elapsed());
        Self {
            dom: dom.clone(),
            styles,
        }
    }
}

struct App {
    page: Page,
    renderer: Option<Renderer>,
    error: Option<anyhow::Error>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("clairo")
            .with_maximized(true);

        let result = event_loop
            .create_window(attributes)
            .context("could not create window")
            .and_then(|window| pollster::block_on(Renderer::new(Arc::new(window), &self.page)));

        match result {
            Ok(renderer) => {
                renderer.window.request_redraw();
                self.renderer = Some(renderer);
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(renderer) = self.renderer.as_mut().filter(|r| r.window.id() == id) else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Err(error) = renderer.resize(size, &self.page) {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Err(error) = renderer.resize(renderer.window.inner_size(), &self.page) {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::Occluded(false) => renderer.window.request_redraw(),
            WindowEvent::RedrawRequested => {
                if let Err(error) = renderer.draw() {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

struct Renderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    font: TrueTypeFontFile<'static>,
    text: TextRenderer,
    glyph_meshes: GlyphMeshCache,
}

impl Renderer {
    async fn new(window: Arc<Window>, page: &Page) -> Result<Self> {
        let started = Instant::now();
        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone())?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("could not find a GPU adapter for the window")?;

        let (device, queue) = adapter.request_device(&Default::default()).await?;

        let size = window.inner_size();

        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("window has no supported GPU surface configuration")?;

        surface.configure(&device, &config);
        eprintln!("gpu initialization took {:?}", started.elapsed());

        let viewport = Viewport::new(&window);

        let started = Instant::now();
        let font = TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf"))
            .parse()
            .context("could not parse Lato")?;

        eprintln!("font parsing took {:?}", started.elapsed());

        let mut glyph_meshes = GlyphMeshCache::new();
        let started = Instant::now();
        let tree = layout_document(&page.dom, &page.styles, &viewport, &font);
        eprintln!("layout took {:?}", started.elapsed());

        let started = Instant::now();
        let vertices = mesh_layout(&tree, &font, &viewport, &mut glyph_meshes)?;
        eprintln!("glyph meshing took {:?}", started.elapsed());

        let started = Instant::now();
        let text = TextRenderer::new(&device, config.format, &vertices)?;
        eprintln!("Text renderer setup took {:?}", started.elapsed());

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            font,
            text,
            glyph_meshes,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>, page: &Page) -> Result<()> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        let viewport = Viewport::new(&self.window);
        let tree = layout_document(&page.dom, &page.styles, &viewport, &self.font);
        let vertices = mesh_layout(&tree, &self.font, &viewport, &mut self.glyph_meshes)?;
        self.text.update_vertices(&self.device, &vertices)?;

        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.window.request_redraw();
        Ok(())
    }

    fn draw(&mut self) -> Result<()> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        use wgpu::CurrentSurfaceTexture as Frame;
        let frame = match self.surface.get_current_texture() {
            Frame::Success(frame) | Frame::Suboptimal(frame) => frame,
            Frame::Lost => bail!("GPU surface was lost"),
            Frame::Validation => bail!("GPU surface validation failed"),
            _ => return Ok(()),
        };

        let view = frame.texture.create_view(&Default::default());

        let mut encoder = self.device.create_command_encoder(&Default::default());

        self.text.render(&mut encoder, &view);

        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);

        Ok(())
    }
}
