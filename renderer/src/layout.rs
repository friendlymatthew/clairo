use anyhow::Result;
use font::{
    grammar::TrueTypeFontFile,
    mesh::{GlyphVertex, mesh},
    outline,
    shaper::TrueTypeFontShaper,
};

const PADDING: f32 = 16.0;

pub struct TextMesh {
    pub vertices: Vec<GlyphVertex>,
    pub width: f32,
    pub height: f32,
}

pub fn layout_text(font: &TrueTypeFontFile<'_>, text: &str) -> Result<TextMesh> {
    let shaper = TrueTypeFontShaper::from(font);
    let scale = 128.0 / f32::from(font.head_table.units_per_em);
    let mut vertices = Vec::new();
    let mut glyph_x = 0.0;

    for character in text.chars() {
        let glyph = shaper.shape_char(character);

        // todo: do mesh(outline) per glyph once
        // we can probably cache a lot of this work per character
        for mut vertex in mesh(&outline(font, glyph.glyph_id)?)? {
            let [x, y] = vertex.position;
            vertex.position = [glyph_x + x * scale, -y * scale];
            vertices.push(vertex);
        }

        glyph_x += f32::from(glyph.advance_width) * scale;
    }

    let (mut left, mut top, mut right, mut bottom) = (0.0_f32, 0.0_f32, glyph_x, 0.0_f32);

    for vertex in &vertices {
        let [x, y] = vertex.position;
        left = left.min(x);
        top = top.min(y);
        right = right.max(x);
        bottom = bottom.max(y);
    }

    let width = right - left + 2.0 * PADDING;
    let height = bottom - top + 2.0 * PADDING;
    for vertex in &mut vertices {
        let [x, y] = vertex.position;
        vertex.position = [
            2.0 * (x - left + PADDING) / width - 1.0,
            1.0 - 2.0 * (y - top + PADDING) / height,
        ];
    }

    Ok(TextMesh {
        vertices,
        width,
        height,
    })
}
