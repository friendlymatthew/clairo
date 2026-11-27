use std::{sync::Arc, thread, time::Instant};

use anyhow::{Context, Result, bail};
use font::{TrueTypeFontParser, grammar::TrueTypeFontFile};
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, RcDom};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

use crate::{
    layout::{GlyphMeshCache, Viewport, layout_document, mesh_layout},
    style::ElementStyles,
    text::TextRenderer,
};

/// the entry point to all of my problems
pub fn run(url: String) -> Result<()> {
    let event_loop = EventLoop::<PageLoaded>::with_user_event().build()?;
    let mut app = App {
        url,
        proxy: event_loop.create_proxy(),
        page: None,
        renderer: None,
        error: None,
    };

    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

struct PageLoaded(Result<Vec<u8>>);

struct Page {
    dom: Handle,
    styles: ElementStyles,
}

impl Page {
    fn from_html(html: &[u8]) -> Self {
        let started = Instant::now();
        let dom = parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .one(html)
            .document;
        eprintln!("html parsing took {:?}", started.elapsed());

        let started = Instant::now();
        let styles = ElementStyles::compute(&dom);
        eprintln!("style computation took {:?}", started.elapsed());
        Self { dom, styles }
    }
}

struct App {
    url: String,
    proxy: EventLoopProxy<PageLoaded>,
    page: Option<Page>,
    renderer: Option<Renderer>,
    error: Option<anyhow::Error>,
}

impl ApplicationHandler<PageLoaded> for App {
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
            .and_then(|window| {
                let mut renderer = pollster::block_on(Renderer::new(Arc::new(window)))?;
                renderer.draw()?;
                renderer.window.request_redraw();

                // fetch and decode the wikipedia page on a background thread
                let url = self.url.clone();
                let proxy = self.proxy.clone();
                thread::Builder::new()
                    .name("page-fetch".into())
                    .spawn(move || {
                        let _ = proxy.send_event(PageLoaded(fetch_html(&url)));
                    })
                    .context("could not start page fetch")?;

                Ok(renderer)
            });

        match result {
            Ok(renderer) => {
                self.renderer = Some(renderer);
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, PageLoaded(result): PageLoaded) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        let result = result.and_then(|html| {
            let page = Page::from_html(&html);
            renderer.show_page(&page)?;

            Ok(page)
        });

        match result {
            Ok(page) => self.page = Some(page),
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
                if let Err(error) = renderer.resize(size, self.page.as_ref()) {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Err(error) =
                    renderer.resize(renderer.window.inner_size(), self.page.as_ref())
                {
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
    async fn new(window: Arc<Window>) -> Result<Self> {
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

        let started = Instant::now();
        let font = TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf"))
            .parse()
            .context("could not parse Lato")?;

        eprintln!("font parsing took {:?}", started.elapsed());

        let started = Instant::now();

        let format_features = adapter.get_texture_format_features(config.format).flags;
        let samples = if format_features.sample_count_supported(4)
            && format_features.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_RESOLVE)
        {
            4
        } else {
            1
        };

        let text = TextRenderer::new(&device, config.format, &[], samples)?;
        eprintln!("Text renderer setup took {:?}", started.elapsed());

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            font,
            text,
            glyph_meshes: GlyphMeshCache::new(),
        })
    }

    fn show_page(&mut self, page: &Page) -> Result<()> {
        self.update_page_vertices(page, true)?;
        self.window.request_redraw();

        Ok(())
    }

    fn update_page_vertices(&mut self, page: &Page, print_timings: bool) -> Result<()> {
        let viewport = Viewport::new(&self.window);
        let started = print_timings.then(Instant::now);
        let tree = layout_document(&page.dom, &page.styles, &viewport, &self.font);

        if let Some(started) = started {
            eprintln!("layout took {:?}", started.elapsed());
        }

        let started = print_timings.then(Instant::now);
        let vertices = mesh_layout(&tree, &self.font, &viewport, &mut self.glyph_meshes)?;

        if let Some(started) = started {
            eprintln!("glyph meshing took {:?}", started.elapsed());
        }

        let started = print_timings.then(Instant::now);
        self.text.update_vertices(&self.device, &vertices)?;

        if let Some(started) = started {
            eprintln!("text vertex upload took {:?}", started.elapsed());
        }

        Ok(())
    }

    fn resize(&mut self, size: PhysicalSize<u32>, page: Option<&Page>) -> Result<()> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        if let Some(page) = page {
            self.update_page_vertices(page, false)?;
        }

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

        self.text.render(&self.device, &mut encoder, &view);

        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);

        Ok(())
    }
}

fn fetch_html(url: &str) -> Result<Vec<u8>> {
    let started = Instant::now();
    let response = net::http1::get(url)?;
    eprintln!("http fetch took {:?}", started.elapsed());

    let started = Instant::now();
    let html = response.decoded_body()?.into_owned();
    eprintln!("http body decoding took {:?}", started.elapsed());
    Ok(html)
}
