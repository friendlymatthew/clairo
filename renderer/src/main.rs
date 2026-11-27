use anyhow::Result;

fn main() -> Result<()> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://en.wikipedia.org/wiki/Clairo".into());

    clairo_renderer::run(url)
}
