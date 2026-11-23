use crate::grammar::TrueTypeFontFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    pub advance_width: u16,
}

#[derive(Debug)]
pub struct TrueTypeFontShaper<'a> {
    file: &'a TrueTypeFontFile<'a>,
}

impl<'a> TrueTypeFontShaper<'a> {
    pub const fn from(file: &'a TrueTypeFontFile<'a>) -> Self {
        Self { file }
    }

    pub fn shape(&self, phrase: &str) -> Vec<ShapedGlyph> {
        phrase
            .chars()
            .map(|character| self.shape_char(character))
            .collect()
    }

    pub fn shape_char(&self, character: char) -> ShapedGlyph {
        let cmap_4 = self
            .file
            .cmap_table
            .format_4()
            .expect("How do you handle other formats?");

        let glyph_id = cmap_4.find_glyph_index(character) as u16;

        ShapedGlyph {
            glyph_id,
            advance_width: self.file.hmtx_table.advance_width(glyph_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::TrueTypeFontParser;
    use crate::grammar::Glyph;

    use super::*;

    #[test]
    fn test_parse_lato() {
        let ttf_file = include_bytes!("../Lato-Regular.ttf");
        let ttf = TrueTypeFontParser::new(ttf_file).parse().unwrap();

        let shaper = TrueTypeFontShaper::from(&ttf);
        let glyph = |g: ShapedGlyph| &ttf.glyph_table.glyphs[g.glyph_id as usize];

        let shaped = shaper.shape("a b");
        assert_eq!(shaped.len(), 3);
        assert!(matches!(glyph(shaped[0]), Glyph::Simple(_)));
        assert!(matches!(glyph(shaped[1]), Glyph::Empty));
        assert!(shaped[1].advance_width > 0);

        let shaped = shaper.shape("a a");
        assert_eq!(shaped[0], shaped[2]);

        let shaped = shaper.shape("\u{E000}");
        assert_eq!(shaped[0].glyph_id, 0);
        assert!(!matches!(glyph(shaped[0]), Glyph::Empty));

        // would truncate to 'A' if we cast to u16
        assert_eq!(shaper.shape("\u{10041}")[0].glyph_id, 0);
    }
}
