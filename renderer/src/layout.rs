use std::collections::{HashMap, hash_map::Entry};

use anyhow::Result;
use font::{
    grammar::TrueTypeFontFile,
    mesh::{GlyphVertex, mesh},
    outline,
    shaper::TrueTypeFontShaper,
};
use markup5ever_rcdom::{Handle, NodeData};
use winit::window::Window;

use crate::style::{ComputedStyle, Display, ElementStyles, LineHeight, Width};

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
    blocks: Vec<BlockBox>,
}

struct BlockBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    lines: Vec<TextLine>,
}

struct TextLine {
    text: String,
    baseline_y: f32,
    font_size_px: f32,
}

pub(crate) fn layout_document(
    root: &Handle,
    element_styles: &ElementStyles,
    viewport: &Viewport,
    font: &TrueTypeFontFile<'_>,
) -> LayoutTree {
    let mut blocks = Vec::new();
    let mut y = 0.0;

    let mut pending = vec![root.clone()];
    while let Some(node) = pending.pop() {
        let style = element_styles.get(&node);

        if style.is_some_and(|style| matches!(style.display, Display::None)) {
            continue;
        }

        let is_h1_or_p = match &node.data {
            NodeData::Element { name, .. } => matches!(name.local.as_ref(), "h1" | "p"),
            _ => false,
        };

        if let Some(style) =
            style.filter(|style| is_h1_or_p && matches!(style.display, Display::Block))
        {
            let text = collect_visible_text(&node, element_styles);
            let width = resolve_width(&style.width, viewport.width);
            let lines = wrap_text(font, &text, width, style.font_size_px);

            let (lines, height) = position_lines(font, lines, y, style);

            blocks.push(BlockBox {
                x: 0.0,
                y,
                width,
                height,
                lines,
            });
            y += height;

            continue;
        }

        pending.extend(node.children.borrow().iter().rev().cloned());
    }

    LayoutTree { blocks }
}

fn position_lines(
    font: &TrueTypeFontFile<'_>,
    lines: Vec<String>,
    block_y: f32,
    style: &ComputedStyle,
) -> (Vec<TextLine>, f32) {
    let scale = style.font_size_px / f32::from(font.head_table.units_per_em);
    let ascent = f32::from(font.hhea_table.ascent) * scale;
    let descent = -f32::from(font.hhea_table.descent) * scale;
    let line_gap = f32::from(font.hhea_table.line_gap) * scale;

    let line_height = match style.line_height {
        LineHeight::Normal => ascent + descent + line_gap,
        LineHeight::Px(px) => px,
        LineHeight::Number(n) => n * style.font_size_px,
    };

    let first_baseline = block_y + ascent + (line_height - ascent - descent) / 2.0;
    let height = lines.len() as f32 * line_height;

    let lines = lines
        .into_iter()
        .enumerate()
        .map(|(index, text)| TextLine {
            text,
            baseline_y: first_baseline + index as f32 * line_height,
            font_size_px: style.font_size_px,
        })
        .collect();

    (lines, height)
}

fn resolve_width(width: &Width, available: f32) -> f32 {
    match width {
        Width::Auto => available,
        Width::Px(px) => *px,
        Width::Percent(percent) => available * percent / 100.0,
    }
}

fn wrap_text(
    font: &TrueTypeFontFile<'_>,
    text: &str,
    width: f32,
    font_size_px: f32,
) -> Vec<String> {
    let shaper = TrueTypeFontShaper::from(font);

    let scale = font_size_px / f32::from(font.head_table.units_per_em);
    let measure = |text: &str| {
        text.chars()
            .map(|ch| f32::from(shaper.shape_char(ch).advance_width))
            .sum::<f32>()
            * scale
    };

    let space_width = measure(" ");

    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_width = 0.0;

    for w in text.split_ascii_whitespace() {
        let w_width = measure(w);

        // ran out of space dog, time to push a new line
        if !line.is_empty() && line_width + space_width + w_width > width {
            lines.push(std::mem::take(&mut line));
            line_width = 0.0
        }

        if !line.is_empty() {
            line.push(' ');
            line_width += space_width;
        }

        line.push_str(w);
        line_width += w_width;
    }

    if !line.is_empty() {
        lines.push(line);
    }

    lines
}

fn collect_visible_text(root: &Handle, styles: &ElementStyles) -> String {
    let mut text = String::new();

    let mut stack = vec![root.clone()];

    while let Some(node) = stack.pop() {
        if styles
            .get(&node)
            .is_some_and(|style| matches!(style.display, Display::None))
        {
            continue;
        }

        if let NodeData::Text { contents } = &node.data {
            text.push_str(contents.borrow().as_ref());
        }

        stack.extend(node.children.borrow().iter().rev().cloned());
    }

    text.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) type GlyphMeshCache = HashMap<u16, Vec<GlyphVertex>>;

pub(crate) fn mesh_layout(
    tree: &LayoutTree,
    font: &TrueTypeFontFile<'_>,
    viewport: &Viewport,
    glyph_meshes: &mut GlyphMeshCache,
) -> Result<Vec<GlyphVertex>> {
    let mut vertices = Vec::new();

    if viewport.width <= 0.0 || viewport.height <= 0.0 {
        return Ok(vertices);
    }

    let shaper = TrueTypeFontShaper::from(font);

    let units_per_em = f32::from(font.head_table.units_per_em);
    let font_y_min = f32::from(font.head_table.y_min);
    let font_y_max = f32::from(font.head_table.y_max);

    for block in &tree.blocks {
        for line in &block.lines {
            let scale = line.font_size_px / units_per_em;

            let top = line.baseline_y - font_y_max * scale;
            let bottom = line.baseline_y - font_y_min * scale;
            if bottom <= 0.0 || top >= viewport.height {
                continue;
            }

            let mut pen_x = block.x;

            for ch in line.text.chars() {
                let glyph = shaper.shape_char(ch);
                let glyph_vertices = match glyph_meshes.entry(glyph.glyph_id) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(mesh(&outline(font, glyph.glyph_id)?)?),
                };

                for mut vertex in glyph_vertices.iter().copied() {
                    let [x, y] = vertex.position;
                    let page_x = pen_x + x * scale;
                    let page_y = line.baseline_y - y * scale;

                    vertex.position = [
                        2.0 * page_x / viewport.width - 1.0,
                        1.0 - 2.0 * page_y / viewport.height,
                    ];
                    vertices.push(vertex);
                }

                pen_x += f32::from(glyph.advance_width) * scale;
            }
        }
    }

    Ok(vertices)
}
