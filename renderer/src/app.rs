use std::{sync::Arc, thread, time::Instant};

use anyhow::{Context, Result, bail};
use font::{TrueTypeFontParser, grammar::TrueTypeFontFile};
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use url::Url;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use crate::{
    layout::{LayoutTree, Viewport, article_path, layout_document},
    media::{ImageRenderer, Images, fetch_images},
    style::{
        ElementStyles, LinkedStylesheets, StyleRules, collect_rules_with_defaults, stylesheet_href,
    },
    text::{LinkHit, TextRenderer},
};

/// the entry point to all of my problems
pub fn run(url: String) -> Result<()> {
    let event_loop = EventLoop::<PageLoaded>::with_user_event().build()?;
    let mut app = App {
        url: Url::parse(&url)?,
        proxy: event_loop.create_proxy(),
        page: None,
        renderer: None,
        error: None,
        history: Vec::new(),
        history_index: 0,
        request_id: 0,
        modifiers: ModifiersState::empty(),
    };

    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}

struct PageLoaded {
    request_id: usize,
    navigation: Navigation,
    result: Result<FetchedPage>,
}

#[derive(Clone, Copy)]
enum Navigation {
    New,
    History(usize),
}

struct HistoryEntry {
    url: Url,
    scroll: f32,
}

struct FetchedPage {
    html: Vec<u8>,
    url: Url,
    base: Url,
    images: Images,
    stylesheets: LinkedStylesheets,
}

struct Page {
    url: Url,
    base: Url,
    images: Images,
    article: Vec<Handle>,
    dom: Handle,
    styles: ElementStyles,
    rules: StyleRules,
    styled_width: f32,
}

impl Page {
    fn from_fetch(fetched: FetchedPage, viewport_width: f32) -> Self {
        let started = Instant::now();
        let dom = parse_html(&fetched.html);
        eprintln!("html parsing took {:?}", started.elapsed());

        let started = Instant::now();
        let article = wikipedia_article(&dom, &fetched.url);
        let defaults = if article.is_empty() {
            ""
        } else {
            include_str!("wikipedia.css")
        };
        let rules = collect_rules_with_defaults(&dom, &fetched.stylesheets, defaults);
        let styles = ElementStyles::compute(&dom, &rules, viewport_width);
        eprintln!("style computation took {:?}", started.elapsed());
        Self {
            url: fetched.url,
            base: fetched.base,
            images: fetched.images,
            article,
            dom,
            styles,
            rules,
            styled_width: viewport_width,
        }
    }

    fn update_styles(&mut self, viewport_width: f32) {
        if self
            .rules
            .media_matches_changed(self.styled_width, viewport_width)
        {
            self.styles = ElementStyles::compute(&self.dom, &self.rules, viewport_width);
        }
        self.styled_width = viewport_width;
    }
}

struct App {
    url: Url,
    proxy: EventLoopProxy<PageLoaded>,
    page: Option<Page>,
    renderer: Option<Renderer>,
    error: Option<anyhow::Error>,
    history: Vec<HistoryEntry>,
    history_index: usize,
    request_id: usize,
    modifiers: ModifiersState,
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

                Ok(renderer)
            });

        match result {
            Ok(renderer) => {
                self.renderer = Some(renderer);
                if let Err(error) = self.navigate(self.url.clone(), Navigation::New) {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: PageLoaded) {
        if event.request_id != self.request_id {
            return;
        }

        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let fetched = match event.result {
            Ok(fetched) => fetched,
            Err(error) if self.page.is_some() => {
                eprintln!("navigation failed: {error:#}");
                renderer.window.set_title("clairo — navigation failed");
                return;
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
        };

        let page = Page::from_fetch(fetched, Viewport::new(&renderer.window).width);
        let saved_scroll = match event.navigation {
            Navigation::History(index) => self.history.get(index).map(|entry| entry.scroll),
            Navigation::New => None,
        };
        let result = renderer.show_page(&page, saved_scroll);
        match result {
            Ok(()) => {
                self.record_navigation(page.url.clone(), event.navigation);
                self.page = Some(page);
            }
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if !self
            .renderer
            .as_ref()
            .is_some_and(|renderer| renderer.window.id() == id)
        {
            return;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            event_loop.exit();
            return;
        }
        if let Err(error) = self.handle_window_event(event) {
            self.error = Some(error);
            event_loop.exit();
        }
    }
}

impl App {
    fn handle_window_event(&mut self, event: WindowEvent) -> Result<()> {
        let Some(renderer) = self.renderer.as_mut() else {
            return Ok(());
        };
        match event {
            WindowEvent::Resized(size) => renderer.resize(size, self.page.as_mut())?,
            WindowEvent::ScaleFactorChanged { .. } => {
                renderer.resize(renderer.window.inner_size(), self.page.as_mut())?;
            }
            WindowEvent::Occluded(false) => renderer.window.request_redraw(),
            WindowEvent::RedrawRequested => renderer.draw()?,
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let point = position.to_logical::<f32>(renderer.window.scale_factor());
                renderer.cursor = Some([point.x, point.y]);
                renderer.update_cursor();
            }
            WindowEvent::CursorLeft { .. } => {
                renderer.cursor = None;
                renderer.update_cursor();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(target) = renderer.link_at_cursor()
                    && let Some(page) = &self.page
                    && let Ok(url) = page.base.join(target.trim())
                    && url.scheme() == "https"
                {
                    self.navigate(url, Navigation::New)?;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Back,
                ..
            } => self.traverse_history(-1)?,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Forward,
                ..
            } => self.traverse_history(1)?,
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, dy) => dy * 42.0,
                    MouseScrollDelta::PixelDelta(delta) => {
                        delta.to_logical::<f32>(renderer.window.scale_factor()).y
                    }
                };
                renderer.scroll_to(renderer.scroll - dy)?;
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let page_height = Viewport::new(&renderer.window).height * 0.9;
                let offset = match event.logical_key.as_ref() {
                    Key::Named(NamedKey::ArrowLeft) | Key::Character("[")
                        if self.modifiers.alt_key() || self.modifiers.super_key() =>
                    {
                        return self.traverse_history(-1);
                    }
                    Key::Named(NamedKey::ArrowRight) | Key::Character("]")
                        if self.modifiers.alt_key() || self.modifiers.super_key() =>
                    {
                        return self.traverse_history(1);
                    }
                    Key::Named(NamedKey::ArrowUp) => -40.0,
                    Key::Named(NamedKey::ArrowDown) => 40.0,
                    Key::Named(NamedKey::PageUp) => -page_height,
                    Key::Named(NamedKey::PageDown) => page_height,
                    Key::Named(NamedKey::Space) if self.modifiers.shift_key() => -page_height,
                    Key::Named(NamedKey::Space) => page_height,
                    Key::Named(NamedKey::Home) => -renderer.scroll,
                    Key::Named(NamedKey::End) => f32::MAX,
                    _ => return Ok(()),
                };
                renderer.scroll_to(renderer.scroll + offset)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn navigate(&mut self, url: Url, navigation: Navigation) -> Result<()> {
        let saved_scroll = self.history_scroll(navigation);
        self.request_id += 1;
        let request_id = self.request_id;

        if let Some(renderer) = &mut self.renderer {
            if let Some(entry) = self.history.get_mut(self.history_index) {
                entry.scroll = renderer.scroll;
            }

            if let Some(page) = &mut self.page
                && page.url[..url::Position::AfterQuery] == url[..url::Position::AfterQuery]
            {
                let scroll = saved_scroll.unwrap_or_else(|| renderer.anchor_scroll(url.fragment()));
                renderer.scroll_to(scroll)?;
                renderer.window.set_title(&format!("clairo — {url}"));
                page.url = url.clone();
                self.record_navigation(url, navigation);
                return Ok(());
            }
            renderer
                .window
                .set_title(&format!("clairo — loading {url}"));
        }

        let proxy = self.proxy.clone();
        thread::Builder::new()
            .name("page-fetch".into())
            .spawn(move || {
                let result = fetch_page(&url);
                let _ = proxy.send_event(PageLoaded {
                    request_id,
                    navigation,
                    result,
                });
            })
            .context("could not start page fetch")?;
        Ok(())
    }

    fn traverse_history(&mut self, offset: isize) -> Result<()> {
        if let Some(index) = self.history_index.checked_add_signed(offset)
            && let Some(entry) = self.history.get(index)
        {
            self.navigate(entry.url.clone(), Navigation::History(index))?;
        }
        Ok(())
    }

    fn history_scroll(&self, navigation: Navigation) -> Option<f32> {
        match navigation {
            Navigation::New => None,
            Navigation::History(index) => self.history.get(index).map(|entry| entry.scroll),
        }
    }

    fn record_navigation(&mut self, url: Url, navigation: Navigation) {
        match navigation {
            Navigation::New => {
                self.history.truncate(self.history_index + 1);
                self.history.push(HistoryEntry { url, scroll: 0.0 });
                self.history_index = self.history.len() - 1;
            }
            Navigation::History(index) => {
                self.history[index].url = url;
                self.history_index = index;
            }
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
    image_renderer: ImageRenderer,
    layout: Option<LayoutTree>,
    hits: Vec<LinkHit>,
    scroll: f32,
    cursor: Option<[f32; 2]>,
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

        let image_renderer = ImageRenderer::new(&device, config.format);

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            font,
            text,
            image_renderer,
            layout: None,
            hits: Vec::new(),
            scroll: 0.0,
            cursor: None,
        })
    }

    fn show_page(&mut self, page: &Page, saved_scroll: Option<f32>) -> Result<()> {
        self.image_renderer
            .set_images(&self.device, &self.queue, &page.images);
        let started = Instant::now();
        self.reflow(page);
        eprintln!("layout took {:?}", started.elapsed());

        self.scroll = saved_scroll.unwrap_or_else(|| self.anchor_scroll(page.url.fragment()));
        self.update_visible_text(true)?;
        self.window.set_title(&format!("clairo — {}", page.url));
        Ok(())
    }

    fn reflow(&mut self, page: &Page) {
        self.layout = Some(layout_document(
            &page.dom,
            &page.styles,
            &Viewport::new(&self.window),
            &self.font,
            &page.images,
            &page.article,
        ));
    }

    fn anchor_scroll(&self, fragment: Option<&str>) -> f32 {
        fragment
            .and_then(|fragment| self.layout.as_ref()?.anchors.get(fragment).copied())
            .unwrap_or(0.0)
    }

    fn update_visible_text(&mut self, print_timings: bool) -> Result<()> {
        let Some(layout) = &self.layout else {
            return Ok(());
        };
        let viewport = Viewport::new(&self.window);
        self.scroll = self
            .scroll
            .clamp(0.0, (layout.height - viewport.height).max(0.0));
        let started = print_timings.then(Instant::now);
        let (vertices, hits) = self
            .text
            .mesh_layout(layout, &self.font, &viewport, self.scroll)?;
        if let Some(started) = started {
            eprintln!("glyph meshing took {:?}", started.elapsed());
        }

        let started = print_timings.then(Instant::now);
        self.text.update_vertices(&self.device, &vertices)?;
        if let Some(started) = started {
            eprintln!("text vertex upload took {:?}", started.elapsed());
        }
        self.hits = hits;
        self.update_cursor();
        self.window.request_redraw();
        Ok(())
    }

    fn scroll_to(&mut self, scroll: f32) -> Result<()> {
        let Some(layout) = &self.layout else {
            return Ok(());
        };
        let scroll = scroll.clamp(
            0.0,
            (layout.height - Viewport::new(&self.window).height).max(0.0),
        );
        if self.scroll != scroll {
            self.scroll = scroll;
            self.update_visible_text(false)?;
        }
        Ok(())
    }

    fn link_at_cursor(&self) -> Option<&str> {
        let [x, y] = self.cursor?;
        if let Some(hit) = self.hits.iter().rev().find(|hit| hit.contains(x, y)) {
            return Some(&hit.href);
        }
        let y = y + self.scroll;
        self.layout.as_ref()?.images.iter().find_map(|image| {
            (x >= image.x
                && x < image.x + image.width
                && y >= image.y
                && y < image.y + image.height)
                .then_some(image.href.as_deref())
                .flatten()
        })
    }

    fn update_cursor(&self) {
        self.window.set_cursor(if self.link_at_cursor().is_some() {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        });
    }

    fn resize(&mut self, size: PhysicalSize<u32>, page: Option<&mut Page>) -> Result<()> {
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        if let Some(page) = page {
            page.update_styles(Viewport::new(&self.window).width);
            self.reflow(page);
            self.update_visible_text(false)?;
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
        if let Some(layout) = &self.layout {
            self.image_renderer.draw(
                &self.device,
                &mut encoder,
                &view,
                &layout.images,
                &Viewport::new(&self.window),
                self.scroll,
            );
        }

        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);

        Ok(())
    }
}

fn fetch_page(url: &Url) -> Result<FetchedPage> {
    let started = Instant::now();
    let (url, response) = net::http1::get_following_redirects(url.as_str())?;
    eprintln!("http fetch took {:?}", started.elapsed());

    let started = Instant::now();
    let html = response.decoded_body()?.into_owned();
    eprintln!("http body decoding took {:?}", started.elapsed());

    let dom = parse_html(&html);
    let started = Instant::now();
    let (base, stylesheets) = fetch_stylesheets(&dom, &url);
    eprintln!("linked stylesheet loading took {:?}", started.elapsed());
    let article = wikipedia_article(&dom, &url);
    let started = Instant::now();
    let images = fetch_images(article.last().unwrap_or(&dom), &base);
    eprintln!("loaded {} images in {:?}", images.len(), started.elapsed());
    Ok(FetchedPage {
        html,
        url,
        base,
        images,
        stylesheets,
    })
}

fn wikipedia_article(root: &Handle, url: &Url) -> Vec<Handle> {
    if url
        .domain()
        .is_some_and(|host| host == "wikipedia.org" || host.ends_with(".wikipedia.org"))
    {
        article_path(root)
    } else {
        Vec::new()
    }
}

fn parse_html(html: &[u8]) -> Handle {
    parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .one(html)
        .document
}

fn fetch_stylesheets(root: &Handle, document_url: &Url) -> (Url, LinkedStylesheets) {
    let mut stylesheets = LinkedStylesheets::new();
    let mut pending = vec![root.clone()];
    let mut hrefs = Vec::new();
    let mut base_href = None;

    while let Some(node) = pending.pop() {
        if base_href.is_none()
            && let NodeData::Element { name, attrs, .. } = &node.data
            && name.ns.as_ref() == "http://www.w3.org/1999/xhtml"
            && name.local.as_ref() == "base"
        {
            base_href = attrs
                .borrow()
                .iter()
                .find(|attr| attr.name.local.as_ref() == "href")
                .map(|attr| attr.value.to_string());
        }

        if let Some(href) = stylesheet_href(&node) {
            hrefs.push(href);
        }
        pending.extend(node.children.borrow().iter().rev().cloned());
    }

    let base_url = base_href.and_then(|href| document_url.join(href.trim()).ok());
    let base_url = base_url.as_ref().unwrap_or(document_url);
    for href in hrefs {
        if stylesheets.contains_key(&href) {
            continue;
        }
        let result = (|| -> Result<String> {
            let url = base_url.join(&href)?;
            let response = net::http1::get(url.as_str())?;
            let body = response.decoded_body()?;
            Ok(String::from_utf8_lossy(&body).into_owned())
        })();
        match result {
            Ok(css) => {
                stylesheets.insert(href, css);
            }
            Err(error) => eprintln!("could not load stylesheet {href}: {error}"),
        }
    }
    (base_url.clone(), stylesheets)
}
