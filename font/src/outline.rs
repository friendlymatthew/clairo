use anyhow::{Result, anyhow, bail, ensure};

use crate::grammar::{
    ComponentGlyphArgument, ComponentGlyphTransformation, Glyph, TrueTypeFontFile,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    #[must_use]
    pub const fn mid(self, other: Point) -> Point {
        Point {
            x: f32::midpoint(self.x, other.x),
            y: f32::midpoint(self.y, other.y),
        }
    }
}

impl From<Point> for [f32; 2] {
    fn from(val: Point) -> Self {
        [val.x, val.y]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Line { p0: Point, p1: Point },
    QuadraticBezier { p0: Point, p1: Point, p2: Point },
}

impl Segment {
    pub fn start(self) -> Point {
        match self {
            Segment::Line { p0, .. } | Segment::QuadraticBezier { p0, .. } => p0,
        }
    }

    pub fn end(self) -> Point {
        match self {
            Segment::Line { p1, .. } => p1,
            Segment::QuadraticBezier { p2, .. } => p2,
        }
    }
}

pub type Contour = Vec<Segment>;

type ContourPoints = Vec<(Point, bool)>;

pub fn outline(font: &TrueTypeFontFile, glyph_id: u16) -> Result<Vec<Contour>> {
    Ok(outline_points(font, glyph_id, 0)?
        .iter()
        .map(|points| walk_contour(points))
        .collect())
}

fn outline_points(font: &TrueTypeFontFile, glyph_id: u16, depth: u8) -> Result<Vec<ContourPoints>> {
    ensure!(depth <= 32, "compound glyph nesting is too deep");
    let glyph = font
        .glyph_table
        .glyphs
        .get(usize::from(glyph_id))
        .ok_or_else(|| anyhow!("glyph {glyph_id} out of range"))?;

    match glyph {
        Glyph::Simple(simple) => {
            let points = simple
                .coordinates
                .iter()
                .zip(&simple.flags)
                .map(|(&(x, y), flags)| {
                    (
                        Point {
                            x: f32::from(x),
                            y: f32::from(y),
                        },
                        flags.on_curve(),
                    )
                })
                .collect::<Vec<_>>();
            let mut start = 0;
            simple
                .end_points_of_contours
                .iter()
                .map(|&end| {
                    let end = usize::from(end);
                    let contour = points
                        .get(start..=end)
                        .ok_or_else(|| anyhow!("contour end point {end} out of range"))?;
                    start = end + 1;
                    Ok(contour.to_vec())
                })
                .collect()
        }
        Glyph::Compound(compound) => {
            let mut contours = Vec::<ContourPoints>::new();
            for component in &compound.components {
                use ComponentGlyphArgument::{Coord, Point as PointIndex};
                use ComponentGlyphTransformation::{Affine, NonUniform, Uniform};

                let one = 1_i16 << 14;
                let matrix = match component.transformation {
                    Uniform(scale) => [scale, 0, 0, scale],
                    NonUniform { x_scale, y_scale } => [x_scale, 0, 0, y_scale],
                    Affine {
                        x_scale,
                        scale_01,
                        scale_10,
                        y_scale,
                    } => [x_scale, scale_10, scale_01, y_scale],
                };
                let [a, b, c, d] = matrix.map(|value| f32::from(value) / f32::from(one));
                let transform = |Point { x, y }| Point {
                    x: a * x + b * y,
                    y: c * x + d * y,
                };
                let mut parts = outline_points(font, component.glyph_index, depth + 1)?;
                for (point, _) in parts.iter_mut().flatten() {
                    *point = transform(*point);
                }
                let mut offset = match (&component.arg_1, &component.arg_2) {
                    (Coord(x), Coord(y)) => {
                        let offset = Point {
                            x: f32::from(*x),
                            y: f32::from(*y),
                        };
                        if component.flag.0 & 0x0800 != 0 {
                            transform(offset)
                        } else {
                            offset
                        }
                    }
                    (PointIndex(parent), PointIndex(child)) => {
                        let parent = contours
                            .iter()
                            .flatten()
                            .nth(usize::from(*parent))
                            .ok_or_else(|| anyhow!("compound parent point out of range"))?
                            .0;
                        let child = parts
                            .iter()
                            .flatten()
                            .nth(usize::from(*child))
                            .ok_or_else(|| anyhow!("compound child point out of range"))?
                            .0;
                        Point {
                            x: parent.x - child.x,
                            y: parent.y - child.y,
                        }
                    }
                    _ => bail!("inconsistent compound glyph arguments"),
                };
                if component.flag.args_are_xy_values() && component.flag.round_xy_to_grid() {
                    offset.x = offset.x.round();
                    offset.y = offset.y.round();
                }
                for (point, _) in parts.iter_mut().flatten() {
                    point.x += offset.x;
                    point.y += offset.y;
                }
                contours.extend(parts);
            }
            Ok(contours)
        }
        Glyph::Empty => Ok(Vec::new()),
    }
}

fn walk_contour(points: &[(Point, bool)]) -> Contour {
    if points.is_empty() {
        return vec![];
    }

    let n = points.len();

    let (start, rest) = {
        let first = points.first().expect("points is nonempty");
        let last = points.last().expect("points is nonempty");

        if first.1 {
            (first.0, &points[1..])
        } else if last.1 {
            (last.0, &points[..n - 1])
        } else {
            (first.0.mid(last.0), points)
        }
    };

    let mut pen = start;
    let mut ctrl = None;
    let mut out = Vec::with_capacity(points.len() + 1);

    for &(p, on_curve) in rest.iter().chain(std::iter::once(&(start, true))) {
        match (on_curve, ctrl) {
            (true, None) => {
                if p != pen {
                    out.push(Segment::Line { p0: pen, p1: p });
                }

                pen = p;
            }
            (true, Some(c)) => {
                out.push(Segment::QuadraticBezier {
                    p0: pen,
                    p1: c,
                    p2: p,
                });

                pen = p;
                ctrl = None;
            }
            (false, None) => {
                ctrl = Some(p);
            }
            (false, Some(c)) => {
                let mid = c.mid(p);
                out.push(Segment::QuadraticBezier {
                    p0: pen,
                    p1: c,
                    p2: mid,
                });

                pen = mid;
                ctrl = Some(p);
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrueTypeFontParser;

    const ON: bool = true;
    const OFF: bool = false;

    const fn p(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    fn pts(v: &[(f32, f32, bool)]) -> Vec<(Point, bool)> {
        v.iter().map(|&(x, y, on)| (p(x, y), on)).collect()
    }

    const fn line(p0: Point, p1: Point) -> Segment {
        Segment::Line { p0, p1 }
    }

    const fn quad(p0: Point, p1: Point, p2: Point) -> Segment {
        Segment::QuadraticBezier { p0, p1, p2 }
    }

    #[test]
    fn walk_square() {
        let contour = walk_contour(&pts(&[
            (0.0, 0.0, ON),
            (10.0, 0.0, ON),
            (10.0, 10.0, ON),
            (0.0, 10.0, ON),
        ]));

        assert_eq!(
            contour,
            [
                line(p(0.0, 0.0), p(10.0, 0.0)),
                line(p(10.0, 0.0), p(10.0, 10.0)),
                line(p(10.0, 10.0), p(0.0, 10.0)),
                line(p(0.0, 10.0), p(0.0, 0.0)),
            ]
        );
    }

    const DOME: [Segment; 3] = [
        quad(p(0.0, 0.0), p(0.0, 100.0), p(50.0, 100.0)),
        quad(p(50.0, 100.0), p(100.0, 100.0), p(100.0, 0.0)),
        line(p(100.0, 0.0), p(0.0, 0.0)),
    ];

    #[test]
    fn walk_dome() {
        // two consecutive off-curve points imply an on-curve point at (50, 100)
        let contour = walk_contour(&pts(&[
            (0.0, 0.0, ON),
            (0.0, 100.0, OFF),
            (100.0, 100.0, OFF),
            (100.0, 0.0, ON),
        ]));

        assert_eq!(contour, DOME);
    }

    #[test]
    fn walk_dome_starting_off_curve() {
        // same contour rotated so it starts off-curve; the walk starts from the last point
        let contour = walk_contour(&pts(&[
            (0.0, 100.0, OFF),
            (100.0, 100.0, OFF),
            (100.0, 0.0, ON),
            (0.0, 0.0, ON),
        ]));

        assert_eq!(contour, DOME);
    }

    #[test]
    fn walk_all_off_curve() {
        // no real on-curve points, so the walk starts at mid(first, last)
        let contour = walk_contour(&pts(&[
            (0.0, 1.0, OFF),
            (1.0, 0.0, OFF),
            (0.0, -1.0, OFF),
            (-1.0, 0.0, OFF),
        ]));

        assert_eq!(
            contour,
            [
                quad(p(-0.5, 0.5), p(0.0, 1.0), p(0.5, 0.5)),
                quad(p(0.5, 0.5), p(1.0, 0.0), p(0.5, -0.5)),
                quad(p(0.5, -0.5), p(0.0, -1.0), p(-0.5, -0.5)),
                quad(p(-0.5, -0.5), p(-1.0, 0.0), p(-0.5, 0.5)),
            ]
        );
    }

    #[test]
    fn accented_compound_glyph_has_an_outline() -> Result<()> {
        let font = TrueTypeFontParser::new(include_bytes!("../Lato-Regular.ttf")).parse()?;
        let glyph = crate::shaper::TrueTypeFontShaper::from(&font).shape_char('é');
        assert!(matches!(
            font.glyph_table.glyphs[usize::from(glyph.glyph_id)],
            Glyph::Compound(_)
        ));
        assert!(outline(&font, glyph.glyph_id)?.len() > 1);
        Ok(())
    }

    #[test]
    fn every_simple_glyph_outline_is_closed() {
        let ttf_file = include_bytes!("../Lato-Regular.ttf");
        let ttf = TrueTypeFontParser::new(ttf_file).parse().unwrap();

        for (id, glyph) in ttf.glyph_table.glyphs.iter().enumerate() {
            let Glyph::Simple(simple) = glyph else {
                continue;
            };

            let contours = outline(&ttf, id as u16).unwrap();

            assert_eq!(contours.len(), simple.end_points_of_contours.len(),);

            for contour in &contours {
                assert!(!contour.is_empty(), "contour should be nonempty");

                for (i, seg) in contour.iter().enumerate() {
                    let next = contour[(i + 1) % contour.len()];
                    assert_eq!(seg.end(), next.start(),);
                }
            }
        }
    }
}
