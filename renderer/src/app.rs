use std::sync::Arc;

use anyhow::{Context, Result, bail};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use super::text::TextRenderer;

/// the entry point to all of my problems
pub fn run(text: &str) -> Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        text,
        renderer: None,
        error: None,
    };

    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

struct App<'a> {
    text: &'a str,
    renderer: Option<Renderer>,
    error: Option<anyhow::Error>,
}

impl ApplicationHandler for App<'_> {
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
            .and_then(|window| pollster::block_on(Renderer::new(Arc::new(window), self.text)));

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
            WindowEvent::Resized(size) => renderer.resize(size),
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
    text: TextRenderer,
}

impl Renderer {
    async fn new(window: Arc<Window>, text: &str) -> Result<Self> {
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

        let text = TextRenderer::new(&device, config.format, text)?;

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            text,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }

        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.window.request_redraw();
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

        self.text
            .render(&mut encoder, &view, self.config.width, self.config.height);

        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);

        Ok(())
    }
}
