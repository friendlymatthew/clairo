use super::{Contour, Point, Segment};
use anyhow::{Result, anyhow, ensure};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers, math::point,
    path::Path,
};

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GlyphVertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
    pub side: f32,
}

fn cross(a: Point, b: Point, c: Point) -> f32 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn split(segment: Segment) -> [Segment; 2] {
    let Segment::QuadraticBezier { p0, p1, p2 } = segment else {
        unreachable!()
    };

    let a = p0.mid(p1);
    let b = p1.mid(p2);
    let m = a.mid(b);
    [
        Segment::QuadraticBezier { p0, p1: a, p2: m },
        Segment::QuadraticBezier { p0: m, p1: b, p2 },
    ]
}

fn overlaps(a: [Point; 3], b: [Point; 3]) -> bool {
    for t in [a, b] {
        for i in 0..3 {
            let p = t[i];
            let q = t[(i + 1) % 3];

            let project = |r: Point| (r.x - p.x) * (q.y - p.y) - (r.y - p.y) * (q.x - p.x);

            let interval = |tri: [Point; 3]| {
                tri.into_iter()
                    .map(project)
                    .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                        (lo.min(v), hi.max(v))
                    })
            };

            let (al, ah) = interval(a);
            let (bl, bh) = interval(b);

            if ah.min(bh) - al.max(bl) <= 0.001 {
                return false;
            }
        }
    }
    true
}

fn triangle(s: Segment) -> Option<[Point; 3]> {
    if let Segment::QuadraticBezier { p0, p1, p2 } = s
        && cross(p0, p1, p2).abs() > 0.001
    {
        return Some([p0, p1, p2]);
    }

    None
}

pub fn mesh(contours: &[Contour]) -> Result<Vec<GlyphVertex>> {
    let mut contours = contours.to_vec();

    for iteration in 0..16 {
        let curves = contours
            .iter()
            .enumerate()
            .flat_map(|(ci, c)| {
                c.iter()
                    .enumerate()
                    .filter_map(move |(si, &s)| triangle(s).map(|t| (ci, si, t)))
            })
            .collect::<Vec<_>>();

        let mut divide = std::collections::BTreeSet::new();
        for (i, &(ci, si, a)) in curves.iter().enumerate() {
            for &(cj, sj, b) in &curves[i + 1..] {
                if overlaps(a, b) {
                    divide.insert((ci, si));
                    divide.insert((cj, sj));
                }
            }
        }

        if divide.is_empty() {
            break;
        }

        ensure!(iteration < 15, "quadratic hull overlap did not converge");

        for (ci, si) in divide.into_iter().rev() {
            let halves = split(contours[ci][si]);
            contours[ci].splice(si..=si, halves);
        }
    }

    let mut vertices = Vec::new();
    let mut interior = Path::builder();

    for contour in &contours {
        let mut polygon = Vec::new();

        for &segment in contour {
            polygon.push(segment.start());

            if let Some([p0, p1, p2]) = triangle(segment) {
                let side = cross(p0, p1, p2).signum();

                if side > 0.0 {
                    polygon.push(p1);
                }

                for (p, uv) in [(p0, [0., 0.]), (p1, [0.5, 0.]), (p2, [1., 1.])] {
                    vertices.push(GlyphVertex {
                        position: p.into(),
                        uv,
                        side,
                    });
                }
            }
        }

        polygon.dedup();

        if polygon.len() >= 3 {
            let to_lyon = |p: Point| point(p.x, p.y);
            interior.begin(to_lyon(polygon[0]));
            for &p in &polygon[1..] {
                interior.line_to(to_lyon(p));
            }
            interior.end(true);
        }
    }

    let mut fill = VertexBuffers::<GlyphVertex, u32>::new();
    FillTessellator::new()
        .tessellate_path(
            &interior.build(),
            &FillOptions::non_zero(),
            &mut BuffersBuilder::new(&mut fill, |vertex: FillVertex<'_>| {
                let p = vertex.position();
                GlyphVertex {
                    position: [p.x, p.y],
                    uv: [0.0; 2],
                    side: 0.0,
                }
            }),
        )
        .map_err(|error| anyhow!("glyph interior tessellation failed: {error:?}"))?;

    vertices.extend(
        fill.indices
            .into_iter()
            .map(|index| fill.vertices[index as usize]),
    );

    Ok(vertices)
}
