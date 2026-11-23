use anyhow::{Context, Result};
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

fn main() -> Result<()> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://en.wikipedia.org/wiki/Clairo".into());

    let response = net::http1::get(&url)?;
    let body = response.decoded_body()?;

    clairo_renderer::run(&page_line(body.as_ref())?)
}

// this is all just faff to grab some text from a valid wikipedia html...
fn page_line(html: &[u8]) -> Result<String> {
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .one(html);

    let heading = descendants(&dom.document)
        .find(|node| is_element(node, "h1") && has_id(node, "firstHeading"))
        .or_else(|| descendants(&dom.document).find(|node| is_element(node, "h1")))
        .context("page has no heading")?;

    let mut line = text_content(&heading);
    let content = descendants(&dom.document)
        .find(|node| has_id(node, "mw-content-text"))
        .unwrap_or_else(|| dom.document.clone());
    if let Some(paragraph) = descendants(&content)
        .filter(|node| is_element(node, "p"))
        .map(|node| text_content(&node))
        .find(|text| !text.is_empty())
    {
        let end = paragraph
            .find(". ")
            .map_or(paragraph.len(), |index| index + 1);
        line.push(' ');
        line.push_str(&paragraph[..end]);
    }

    Ok(line)
}

fn descendants(root: &Handle) -> impl Iterator<Item = Handle> {
    let mut pending = vec![root.clone()];

    std::iter::from_fn(move || {
        let node = pending.pop()?;
        pending.extend(node.children.borrow().iter().rev().cloned());

        Some(node)
    })
}

fn is_element(node: &Handle, tag: &str) -> bool {
    matches!(&node.data, NodeData::Element { name, .. } if name.local.as_ref() == tag)
}

fn has_id(node: &Handle, id: &str) -> bool {
    matches!(&node.data, NodeData::Element { attrs, .. } if attrs.borrow().iter().any(|attr| {
        attr.name.local.as_ref() == "id" && attr.value.as_ref() == id
    }))
}

fn text_content(root: &Handle) -> String {
    let mut text = String::new();
    let mut pending = vec![root.clone()];

    while let Some(node) = pending.pop() {
        if let NodeData::Text { contents } = &node.data {
            text.push_str(&contents.borrow());
        } else if !["sup", "style", "script"]
            .iter()
            .any(|tag| is_element(&node, tag))
        {
            pending.extend(node.children.borrow().iter().rev().cloned());
        }
    }

    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
