use std::time::Instant;

use anyhow::Result;
use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::RcDom;

fn main() -> Result<()> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://en.wikipedia.org/wiki/Clairo".into());

    let started = Instant::now();
    let response = net::http1::get(&url)?;
    eprintln!("http fetch took {:?}", started.elapsed());

    let started = Instant::now();
    let body = response.decoded_body()?;
    eprintln!("http body decoding took {:?}", started.elapsed());

    let started = Instant::now();
    let dom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .one(body.as_ref());
    eprintln!("html parsing took {:?}", started.elapsed());

    clairo_renderer::run(&dom.document)
}
