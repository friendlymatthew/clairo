use std::{collections::HashMap, rc::Rc};

use markup5ever_rcdom::{Handle, Node, NodeData};

pub(crate) enum Display {
    Block,
    Inline,
    None,
}

#[derive(Clone, Copy)]
pub(crate) enum LineHeight {
    Normal,
    Px(f32),
    Number(f32),
}

pub(crate) enum Width {
    Auto,
    Px(f32),
    Percent(f32),
}

pub(crate) struct ComputedStyle {
    pub(crate) display: Display,
    pub(crate) font_size_px: f32,
    pub(crate) line_height: LineHeight,
    pub(crate) width: Width,
}

#[derive(Default)]
pub(crate) struct ElementStyles(HashMap<*const Node, ComputedStyle>);

impl ElementStyles {
    pub(crate) fn compute(root: &Handle) -> Self {
        let mut styles = Self::default();
        let base_style = ComputedStyle {
            display: Display::Inline,
            font_size_px: 16.0,
            line_height: LineHeight::Normal,
            width: Width::Auto,
        };

        walk(root, &base_style, &mut styles);
        styles
    }

    pub(crate) fn get(&self, node: &Handle) -> Option<&ComputedStyle> {
        self.0.get(&Rc::as_ptr(node))
    }
}

fn default_display(tag: &str) -> Display {
    match tag {
        "head" | "script" | "style" | "title" | "template" => Display::None,
        "html" | "body" | "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Display::Block,
        _ => Display::Inline,
    }
}

fn walk(node: &Handle, parent_style: &ComputedStyle, styles: &mut ElementStyles) {
    if let NodeData::Element { name, .. } = &node.data {
        let tag: &str = name.local.as_ref();
        let mut style = ComputedStyle {
            display: default_display(tag),
            font_size_px: parent_style.font_size_px,
            line_height: parent_style.line_height,
            width: Width::Auto,
        };

        if tag == "h1" {
            style.font_size_px = parent_style.font_size_px * 2.0;
        }

        for child in node.children.borrow().iter() {
            walk(child, &style, styles);
        }

        styles.0.insert(Rc::as_ptr(node), style);

        return;
    }

    for child in node.children.borrow().iter() {
        walk(child, parent_style, styles);
    }
}
