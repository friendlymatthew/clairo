use std::{collections::HashMap, ops::Range, rc::Rc};

use font::{grammar::TrueTypeFontFile, shaper::TrueTypeFontShaper};
use markup5ever_rcdom::{Handle, NodeData};
use winit::window::Window;

use crate::{
    media::{ImageBox, Images},
    style::{Color, ComputedStyle, Display, ElementStyles, LineHeight, TextAlign, Width},
};
use markup5ever_rcdom::Node;

pub(crate) fn attribute(node: &Node, key: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|attr| attr.name.local.as_ref() == key && attr.name.ns.is_empty())
        .map(|attr| attr.value.to_string())
}

pub(crate) fn article_path(root: &Handle) -> Vec<Handle> {
    fn find(node: &Handle, path: &mut Vec<Handle>) -> bool {
        path.push(node.clone());
        if matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == "main")
            || node.children.borrow().iter().any(|child| find(child, path))
        {
            return true;
        }
        path.pop();
        false
    }

    let mut path = Vec::new();
    find(root, &mut path);
    path
}

pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    pub fn new(w: &Window) -> Self {
        let win_size = w.inner_size();
        let logical = win_size.to_logical::<f32>(w.scale_factor());

        Self {
            width: logical.width,
            height: logical.height,
        }
    }
}

pub(crate) struct LayoutTree {
    pub(crate) blocks: Vec<BlockBox>,
    pub images: Vec<ImageBox>,
    pub anchors: HashMap<String, f32>,
    pub height: f32,
    pub(crate) decorations: Vec<Decoration>,
}

pub(crate) struct Decoration {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) color: Color,
}

pub(crate) struct BlockBox {
    pub(crate) x: f32,
    y: f32,
    width: f32,
    height: f32,
    pub(crate) lines: Vec<TextLine>,
}

#[derive(Clone, Copy)]
pub(crate) struct Paint {
    pub(crate) size: f32,
    pub(crate) color: Color,
    pub(crate) raised: bool,
}

impl From<&ComputedStyle> for Paint {
    fn from(style: &ComputedStyle) -> Self {
        Self {
            size: style.font_size_px,
            color: style.color,
            raised: style.superscript,
        }
    }
}

pub(crate) struct TextSpan {
    pub(crate) bytes: Range<usize>,
    pub(crate) href: Option<Rc<str>>,
    pub(crate) paint: Paint,
}

#[derive(Default)]
pub(crate) struct LineText {
    pub(crate) text: String,
    pub(crate) spans: Vec<TextSpan>,
    width: f32,
    available: f32,
}

pub(crate) struct TextLine {
    pub(crate) content: LineText,
    pub(crate) baseline_y: f32,
    pub(crate) font_size_px: f32,
    pub(crate) offset: f32,
    pub(crate) paint: Paint,
}

pub(crate) fn layout_document(
    root: &Handle,
    element_styles: &ElementStyles,
    viewport: &Viewport,
    font: &TrueTypeFontFile<'_>,
    images: &Images,
    article: &[Handle],
) -> LayoutTree {
    let root_font_size = root
        .children
        .borrow()
        .iter()
        .find_map(|node| element_styles.get(node).map(|style| style.font_size_px))
        .or_else(|| element_styles.get(root).map(|style| style.font_size_px))
        .unwrap_or(16.0);
    let mut layout = BlockLayout {
        styles: element_styles,
        font,
        root_font_size,
        blocks: Vec::new(),
        image_boxes: Vec::new(),
        anchors: HashMap::new(),
        images,
        article,
        decorations: Vec::new(),
    };
    let mut flow = BlockFlow {
        x: 0.0,
        y: 0.0,
        width: viewport.width,
        text: LineText::default(),
        paint: None,
        float: None,
    };
    layout.visit(root, None, None, &mut flow);
    LayoutTree {
        blocks: layout.blocks,
        images: layout.image_boxes,
        anchors: layout.anchors,
        height: flow.y.max(flow.float.map_or(0.0, |float| float.bottom)),
        decorations: layout.decorations,
    }
}

#[derive(Clone, Copy)]
struct RightFloat {
    left: f32,
    bottom: f32,
}

struct BlockFlow {
    x: f32,
    y: f32,
    width: f32,
    text: LineText,
    paint: Option<Paint>,
    float: Option<RightFloat>,
}

struct BlockLayout<'a, 'font> {
    styles: &'a ElementStyles,
    font: &'a TrueTypeFontFile<'font>,
    root_font_size: f32,
    blocks: Vec<BlockBox>,
    image_boxes: Vec<ImageBox>,
    anchors: HashMap<String, f32>,
    images: &'a Images,
    article: &'a [Handle],
    decorations: Vec<Decoration>,
}

impl BlockLayout<'_, '_> {
    fn visit(
        &mut self,
        node: &Handle,
        parent_style: Option<&ComputedStyle>,
        link: Option<&Rc<str>>,
        flow: &mut BlockFlow,
    ) {
        let style = self.styles.get(node);
        if style.is_some_and(|style| matches!(style.display, Display::None)) {
            return;
        }

        if let NodeData::Text { contents } = &node.data {
            let start = flow.text.text.len();
            flow.text.text.push_str(contents.borrow().as_ref());
            if let Some(paint) = flow.paint {
                flow.text.spans.push(TextSpan {
                    bytes: start..flow.text.text.len(),
                    href: link.cloned(),
                    paint,
                });
            }
            return;
        }

        let block = style.filter(|style| matches!(style.display, Display::Block | Display::Grid));
        let tag = match &node.data {
            NodeData::Element { name, .. } => name.local.as_ref(),
            _ => "",
        };
        if block.is_some() || matches!(tag, "img" | "br") {
            self.flush_text(parent_style, flow);
        }
        if let Some(id) = attribute(node, "id") {
            self.anchors.entry(id).or_insert(flow.y);
        }
        let anchor = (tag == "a")
            .then(|| attribute(node, "href"))
            .flatten()
            .map(Rc::<str>::from);
        let link = anchor.as_ref().or(link);

        if tag == "img" {
            if let Some(src) = attribute(node, "src")
                && let Some(image) = self.images.get(&src)
            {
                let natural_width = attribute(node, "width")
                    .and_then(|width| width.parse::<u32>().ok())
                    .filter(|width| *width > 0)
                    .unwrap_or(image.width) as f32;
                let width = natural_width.min(flow.width);
                let height = width * image.height as f32 / image.width as f32;
                if width > 0.0 && height > 0.0 {
                    self.image_boxes.push(ImageBox {
                        src,
                        x: flow.x
                            + match parent_style.map(|style| style.text_align) {
                                Some(TextAlign::Center) => (flow.width - width) / 2.0,
                                Some(TextAlign::End) => flow.width - width,
                                _ => 0.0,
                            },
                        y: flow.y,
                        width,
                        height,
                        href: link.map(|href| href.to_string()),
                    });
                    flow.y += height;
                }
            } else if let Some(alt) = attribute(node, "alt") {
                flow.text.text.push_str(&alt);
            }
            return;
        }

        if let Some(style) = block {
            self.layout_block(node, style, link, flow);
        } else {
            let previous = flow.paint;
            if let Some(style) = style {
                flow.paint = Some(Paint::from(style));
            }
            self.visit_children(node, parent_style, link, flow);
            flow.paint = previous;
        }
    }

    fn visit_children(
        &mut self,
        node: &Handle,
        style: Option<&ComputedStyle>,
        link: Option<&Rc<str>>,
        flow: &mut BlockFlow,
    ) {
        if let Some(child) = self
            .article
            .windows(2)
            .find(|pair| Rc::ptr_eq(&pair[0], node))
            .map(|pair| &pair[1])
        {
            self.visit(child, style, link, flow);
        } else {
            let mut previous_inline_item = false;
            for child in node.children.borrow().iter() {
                let inline_item = matches!(&child.data, NodeData::Element { name, .. } if name.local.as_ref() == "li")
                    && self
                        .styles
                        .get(child)
                        .is_some_and(|style| matches!(style.display, Display::Inline));
                if inline_item && previous_inline_item {
                    flow.text.text.push_str(" · ");
                }
                self.visit(child, style, link, flow);
                if inline_item {
                    previous_inline_item = true;
                }
            }
        }
    }

    fn layout_table_row(
        &mut self,
        node: &Handle,
        style: &ComputedStyle,
        link: Option<&Rc<str>>,
        flow: &mut BlockFlow,
    ) {
        let children = node.children.borrow();
        let cells: Vec<_> = children.iter().filter(|child| {
            matches!(&child.data, NodeData::Element { name, .. } if matches!(name.local.as_ref(), "th" | "td"))
        }).collect();
        let mut x = flow.x;
        let mut bottom = flow.y;
        for (index, cell) in cells.iter().enumerate() {
            let fraction = match (cells.len(), index) {
                (2, 0) => 0.38,
                (2, 1) => 0.62,
                (count, _) => 1.0 / count as f32,
            };
            let width = flow.width * fraction;
            let mut cell_flow = BlockFlow {
                x,
                y: flow.y,
                width,
                text: LineText::default(),
                paint: Some(Paint::from(style)),
                float: None,
            };
            self.visit(cell, Some(style), link, &mut cell_flow);
            self.flush_text(Some(style), &mut cell_flow);
            bottom = bottom.max(cell_flow.y);
            x += width;
        }
        flow.y = bottom;
    }

    fn layout_block(
        &mut self,
        node: &Handle,
        style: &ComputedStyle,
        link: Option<&Rc<str>>,
        parent: &mut BlockFlow,
    ) {
        let resolve =
            |width| resolve_length(width, parent.width, style.font_size_px, self.root_font_size);
        let [margin_top, margin_right, margin_bottom, mut margin_left] =
            style.margin.map(|value| resolve(value).unwrap_or(0.0));
        let [padding_top, padding_right, padding_bottom, padding_left] =
            style.padding.map(|value| resolve(value).unwrap_or(0.0));

        let horizontal_padding = padding_left + padding_right;
        let sizing_padding = if style.border_box {
            horizontal_padding
        } else {
            0.0
        };
        let auto_width = (parent.width - margin_left - margin_right - horizontal_padding).max(0.0);
        let mut width =
            resolve(style.width).map_or(auto_width, |width| (width - sizing_padding).max(0.0));
        if let Some(max_width) = resolve(style.max_width) {
            width = width.min((max_width - sizing_padding).max(0.0));
        }

        let remaining =
            (parent.width - width - horizontal_padding - margin_left - margin_right).max(0.0);
        match (style.margin[3], style.margin[1]) {
            (Width::Auto, Width::Auto) => {
                margin_left = remaining / 2.0;
            }
            (Width::Auto, _) => margin_left = remaining,
            _ => {}
        }

        if matches!(style.display, Display::Grid)
            && let Some(track_max) = resolve_length(
                style.first_grid_track_max,
                width,
                style.font_size_px,
                self.root_font_size,
            )
        {
            width = width.min(track_max);
        }

        let floating = style.float_right
            && width + horizontal_padding + margin_left + margin_right < parent.width * 0.65;
        let y = if floating {
            parent.y.max(parent.float.map_or(0.0, |float| float.bottom))
        } else {
            parent.y
        };
        let border_x = parent.x
            + if floating {
                parent.width - width - horizontal_padding - margin_right
            } else {
                margin_left
            };
        let border_y = y + margin_top;
        let decoration_index = self.decorations.len();
        if let Some(color) = style.background {
            self.decorations.push(Decoration {
                x: border_x,
                y: border_y,
                width: width + horizontal_padding,
                height: 0.0,
                color,
            });
        }

        let mut flow = BlockFlow {
            x: border_x + padding_left,
            y: border_y + padding_top,
            width,
            text: LineText::default(),
            paint: Some(Paint::from(style)),
            float: if floating { None } else { parent.float },
        };

        if matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == "tr") {
            self.layout_table_row(node, style, link, &mut flow);
        } else {
            self.visit_children(node, Some(style), link, &mut flow);
            self.flush_text(Some(style), &mut flow);
        }

        let mut bottom = flow.y + padding_bottom;
        if let Some(color) = style.border_bottom {
            self.decorations.push(Decoration {
                x: border_x,
                y: bottom,
                width: flow
                    .float
                    .filter(|float| bottom < float.bottom && float.left > border_x)
                    .map_or(width + horizontal_padding, |float| {
                        (width + horizontal_padding).min(float.left - border_x)
                    }),
                height: 1.0,
                color,
            });
            bottom += 1.0;
        }
        if style.background.is_some() {
            self.decorations[decoration_index].height = bottom - border_y;
        }

        if floating {
            parent.float = Some(RightFloat {
                left: border_x - margin_left,
                bottom: bottom + margin_bottom,
            });
        } else {
            parent.y = bottom + margin_bottom;
            parent.float = flow.float;
        }
    }

    fn flush_text(&mut self, style: Option<&ComputedStyle>, flow: &mut BlockFlow) {
        let text = std::mem::take(&mut flow.text);
        let Some(style) = style else { return };
        let font = self.font;
        let (line_height, baseline) = line_metrics(font, style);
        let available = |line: usize| {
            let y = flow.y + line as f32 * line_height;
            flow.float
                .filter(|float| y < float.bottom && float.left > flow.x)
                .map_or(flow.width, |float| flow.width.min(float.left - flow.x))
        };
        let lines = wrap_text(self.font, &text, Paint::from(style), available);
        if lines.is_empty() {
            return;
        }
        let height = lines.len() as f32 * line_height;
        let lines = lines
            .into_iter()
            .enumerate()
            .map(|(index, content)| {
                let remaining = (content.available - content.width).max(0.0);
                TextLine {
                    content,
                    baseline_y: flow.y + baseline + index as f32 * line_height,
                    font_size_px: style.font_size_px,
                    offset: match style.text_align {
                        TextAlign::Start => 0.0,
                        TextAlign::Center => remaining / 2.0,
                        TextAlign::End => remaining,
                    },
                    paint: Paint::from(style),
                }
            })
            .collect();
        self.blocks.push(BlockBox {
            x: flow.x,
            y: flow.y,
            width: flow.width,
            height,
            lines,
        });
        flow.y += height;
    }
}

fn line_metrics(font: &TrueTypeFontFile<'_>, style: &ComputedStyle) -> (f32, f32) {
    let scale = style.font_size_px / f32::from(font.head_table.units_per_em);
    let ascent = f32::from(font.hhea_table.ascent) * scale;
    let descent = -f32::from(font.hhea_table.descent) * scale;
    let line_gap = f32::from(font.hhea_table.line_gap) * scale;

    let line_height = match style.line_height {
        LineHeight::Normal => ascent + descent + line_gap,
        LineHeight::Px(px) => px,
        LineHeight::Number(n) => n * style.font_size_px,
    };

    (line_height, ascent + (line_height - ascent - descent) / 2.0)
}

fn resolve_length(
    length: Width,
    available: f32,
    font_size: f32,
    root_font_size: f32,
) -> Option<f32> {
    match length {
        Width::Auto => None,
        Width::Px(px) => Some(px),
        Width::Percent(percent) => Some(available * (percent / 100.0)),
        Width::Em(em) => Some(em * font_size),
        Width::Rem(rem) => Some(rem * root_font_size),
    }
}

fn wrap_text(
    font: &TrueTypeFontFile<'_>,
    content: &LineText,
    fallback: Paint,
    available: impl Fn(usize) -> f32,
) -> Vec<LineText> {
    let shaper = TrueTypeFontShaper::from(font);
    let measure = |text: &str, paint: Paint| {
        let scale = paint.size / f32::from(font.head_table.units_per_em);
        text.chars()
            .map(|ch| f32::from(shaper.shape_char(ch).advance_width) * scale)
            .sum::<f32>()
    };
    let space_width = measure(" ", fallback);
    let mut lines = Vec::new();
    let mut line = LineText {
        available: available(0),
        ..LineText::default()
    };
    for word in content.text.split_ascii_whitespace() {
        let start = word.as_ptr() as usize - content.text.as_ptr() as usize;
        let end = start + word.len();
        let spans = &content.spans[content
            .spans
            .partition_point(|span| span.bytes.end <= start)..];
        let spans = spans.iter().take_while(|span| span.bytes.start < end);
        let mut cursor = start;
        let mut word_width = 0.0;
        for span in spans.clone() {
            let from = span.bytes.start.max(start);
            let to = span.bytes.end.min(end);
            word_width += measure(&content.text[cursor..from], fallback);
            word_width += measure(&content.text[from..to], span.paint);
            cursor = to;
        }
        word_width += measure(&content.text[cursor..end], fallback);

        if !line.text.is_empty() && line.width + space_width + word_width > line.available {
            lines.push(line);
            line = LineText {
                available: available(lines.len()),
                ..LineText::default()
            };
        }
        if !line.text.is_empty() {
            line.text.push(' ');
            line.width += space_width;
        }
        let offset = line.text.len();
        for span in spans {
            line.spans.push(TextSpan {
                bytes: (offset + span.bytes.start.max(start) - start)
                    ..(offset + span.bytes.end.min(end) - start),
                href: span.href.clone(),
                paint: span.paint,
            });
        }
        line.text.push_str(word);
        line.width += word_width;
    }
    if !line.text.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use crate::text::mesh_layout;
    use anyhow::Result;
    use font::TrueTypeFontParser;
    use html5ever::{parse_document, tendril::TendrilSink};
    use markup5ever_rcdom::RcDom;

    use super::*;

    #[test]
    fn article_typography_and_float_use_the_same_line_layout() -> Result<()> {
        let dom = parse_document(RcDom::default(), Default::default()).one(
            "<style>
                .story > h2, h3 { border-bottom: 1px solid #a2a9b1; }
                .story p { font-size: 0.875rem; line-height: 1.5; }
                .story aside { float: right; width: 100px; padding: 6px; background-color: #f8f9fa; }
            </style>
            <article class='story'><h2>Heading</h2><aside>Portrait caption</aside>
            <p><strong>Bold</strong> normal <em>italic</em><a href='#'><sup>[1]</sup></a>
            and several words to wrap around the box on the right.</p></article>",
        );
        let font =
            TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf")).parse()?;
        let viewport = Viewport {
            width: 300.0,
            height: 200.0,
        };
        let rules = crate::style::collect_rules(&dom.document, &Default::default());
        let styles = ElementStyles::compute(&dom.document, &rules, viewport.width);
        let tree = layout_document(
            &dom.document,
            &styles,
            &viewport,
            &font,
            &Images::new(),
            &[],
        );
        let heading = &tree.blocks[0];
        let float = &tree.blocks[1];
        let body = &tree.blocks[2];
        assert_eq!(heading.lines[0].font_size_px, 24.0);
        assert!(float.x > body.x);
        assert_eq!(body.lines[0].font_size_px, 14.0);
        assert_eq!(body.lines[0].content.available, 188.0);
        assert!(
            body.lines[0]
                .content
                .spans
                .iter()
                .any(|span| span.paint.raised && span.href.is_some())
        );
        assert_eq!(tree.decorations.len(), 2);
        Ok(())
    }

    #[test]
    fn scrolling_keeps_wrapped_links_clickable_and_preserves_fragment_positions() -> Result<()> {
        let dom = parse_document(RcDom::default(), Default::default()).one(
            "<header>Outside article</header><main>
             <p>First line</p><p><a href='#section'>Several linked words across lines</a></p>
             <h2 id='section'>Destination</h2></main>",
        );
        let font =
            TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf")).parse()?;
        let viewport = Viewport {
            width: 90.0,
            height: 35.0,
        };
        let rules = crate::style::collect_rules(&dom.document, &Default::default());
        let styles = ElementStyles::compute(&dom.document, &rules, viewport.width);
        let tree = layout_document(
            &dom.document,
            &styles,
            &viewport,
            &font,
            &Images::new(),
            &article_path(&dom.document),
        );
        assert_eq!(tree.blocks.len(), 3);
        assert!(tree.blocks[1].lines.len() > 1);
        assert_eq!(tree.anchors["section"], tree.blocks[2].y);
        let mut cache = HashMap::new();
        let (_, hits) = mesh_layout(&tree, &font, &viewport, &mut cache, tree.blocks[1].y)?;
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.href.as_ref() == "#section"));
        assert!(hits.iter().any(|hit| hit.contains(2.0, 9.0)));
        Ok(())
    }

    #[test]
    fn nested_blocks_use_container_width_and_spacing() -> Result<()> {
        let dom = parse_document(RcDom::default(), Default::default()).one(
            "<style>
                main { max-width: 40rem; margin: 10px auto; padding: 12px 20px;
                       box-sizing: border-box; }
                section { padding: 4px 10px; }
                p { width: 50%; line-height: 20px; margin: 2px 0 6px; }
                p.second { margin-top: 8px; }
             </style>
             <main><section><p>First</p><p class='second'>Second</p></section></main>",
        );
        let font =
            TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf")).parse()?;
        let viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        let rules = crate::style::collect_rules(&dom.document, &Default::default());
        let styles = ElementStyles::compute(&dom.document, &rules, viewport.width);
        let tree = layout_document(
            &dom.document,
            &styles,
            &viewport,
            &font,
            &Images::new(),
            &[],
        );
        assert_eq!(tree.blocks[0].width, 290.0);
        assert_eq!(tree.blocks[0].x, 110.0);
        assert_eq!(tree.blocks[0].y, 28.0);
        assert_eq!(tree.blocks[1].y, 62.0);
        Ok(())
    }

    #[test]
    fn desktop_grid_track_limits_article_wrapping() -> Result<()> {
        let dom = parse_document(RcDom::default(), Default::default()).one(
            "<style>
                @media screen and (min-width: 1120px) {
                    main { display: grid;
                           grid-template: min-content 1fr / minmax(0, 59.25rem) min-content; }
                }
             </style>
             <main><div><p>Article text</p></div></main>",
        );
        let font =
            TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf")).parse()?;
        let rules = crate::style::collect_rules(&dom.document, &Default::default());
        assert!(!rules.media_matches_changed(1300.0, 1500.0));
        assert!(rules.media_matches_changed(1000.0, 1120.0));
        for (width, expected) in [(1000.0, 1000.0), (1500.0, 948.0)] {
            let viewport = Viewport {
                width,
                height: 600.0,
            };
            let styles = ElementStyles::compute(&dom.document, &rules, width);
            let tree = layout_document(
                &dom.document,
                &styles,
                &viewport,
                &font,
                &Images::new(),
                &[],
            );
            assert_eq!(tree.blocks[0].width, expected);
        }
        Ok(())
    }

    #[test]
    fn class_and_id_selectors_respect_the_cascade() -> Result<()> {
        let dom = parse_document(RcDom::default(), Default::default()).one(
            "<style>
                #summary { width: 180px; font-size: 28px; line-height: 40px !important; }
                .intro { width: 200px; font-size: 20px !important; }
             </style>
             <link rel='stylesheet' href='layout.css'>
             <p id='summary' class='wide intro'>Summary</p>
             <p class='wide intro'>Introduction</p>
             <p id='Summary' class='Intro'>Different case</p>",
        );
        let linked = crate::style::LinkedStylesheets::from([(
            "layout.css".into(),
            ".wide { width: 240px; line-height: 50px !important; }
             p { width: 400px; font-size: 16px; }
             p .intro { width: 700px !important; }"
                .into(),
        )]);
        let font =
            TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf")).parse()?;
        let viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        let styles = ElementStyles::compute(
            &dom.document,
            &crate::style::collect_rules(&dom.document, &linked),
            viewport.width,
        );
        let tree = layout_document(
            &dom.document,
            &styles,
            &viewport,
            &font,
            &Images::new(),
            &[],
        );

        let summary = &tree.blocks[0];
        assert_eq!(summary.width, 180.0);
        assert_eq!(summary.lines[0].font_size_px, 20.0);
        assert_eq!(summary.height, 40.0);
        assert_eq!(tree.blocks[1].width, 240.0);
        assert_eq!(tree.blocks[2].width, 400.0);
        Ok(())
    }

    #[test]
    fn compound_selectors_match_every_part_and_count_specificity() -> Result<()> {
        let dom = parse_document(RcDom::default(), Default::default()).one(
            "<style>
                p.intro { width: 200px; }
                .intro { width: 300px; }
                p#summary { width: 160px; font-size: 24px; }
                #summary { width: 180px; }
                p.intro.featured { font-size: 20px; }
                .intro { font-size: 16px; }
                .featured.intro { line-height: 30px; }
                .intro.featured { line-height: 32px; }
                h1#summary { width: 500px !important; }
                p .intro { width: 600px !important; }
             </style>
             <p id='summary' class='intro featured'>Summary</p>
             <p class='intro'>Introduction</p>
             <p class='featured intro'>Featured</p>
             <h1 class='intro'>Heading</h1>",
        );
        let font =
            TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf")).parse()?;
        let viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        let styles = ElementStyles::compute(
            &dom.document,
            &crate::style::collect_rules(&dom.document, &Default::default()),
            viewport.width,
        );
        let tree = layout_document(
            &dom.document,
            &styles,
            &viewport,
            &font,
            &Images::new(),
            &[],
        );

        assert_eq!(tree.blocks[0].width, 160.0);
        assert_eq!(tree.blocks[0].lines[0].font_size_px, 24.0);
        assert_eq!(tree.blocks[1].width, 200.0);
        assert_eq!(tree.blocks[1].lines[0].font_size_px, 16.0);
        assert_eq!(tree.blocks[2].lines[0].font_size_px, 20.0);
        assert_eq!(tree.blocks[2].height, 32.0);
        assert_eq!(tree.blocks[3].width, 300.0);
        Ok(())
    }

    #[test]
    fn text_properties_cascade_independently_and_inherit() -> Result<()> {
        let dom = parse_document(RcDom::default(), Default::default()).one(
            "<style>
                body { font-size: 20px; line-height: 1.5; }
                p { width: 100px; font-size: 24px !important; font-size: 12px; }
                p { font-size: 10px; width: 120px; line-height: -1; }
             </style>
             <h1>Heading</h1>
             <p>Several words that will wrap across more than one line.</p>",
        );
        let font =
            TrueTypeFontParser::new(include_bytes!("../../font/Lato-Regular.ttf")).parse()?;
        let viewport = Viewport {
            width: 800.0,
            height: 600.0,
        };
        let styles = ElementStyles::compute(
            &dom.document,
            &crate::style::collect_rules(&dom.document, &Default::default()),
            viewport.width,
        );
        let tree = layout_document(
            &dom.document,
            &styles,
            &viewport,
            &font,
            &Images::new(),
            &[],
        );

        let heading = &tree.blocks[0];
        assert_eq!(heading.lines[0].font_size_px, 40.0);
        assert_eq!(heading.height, 60.0);

        let paragraph = &tree.blocks[1];
        assert_eq!(paragraph.width, 120.0);
        assert_eq!(paragraph.lines[0].font_size_px, 24.0);
        let baseline_gap = paragraph.lines[1].baseline_y - paragraph.lines[0].baseline_y;
        assert!((baseline_gap - 36.0).abs() < 0.001);
        Ok(())
    }
}
