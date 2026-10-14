//! Renders the brand SVGs to PNGs: `cargo run -p pm-ui --example render_brand`.
//! Output goes next to the SVGs in `assets/brand/`.
#![allow(clippy::unwrap_used, clippy::print_stdout, reason = "dev tool")]

use std::path::Path;

fn render(svg: &Path, out: &Path, width: u32) {
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_data(&std::fs::read(svg).unwrap(), &opt).unwrap();
    let size = tree.size();
    let scale = width as f32 / size.width();
    let height = (size.height() * scale).round() as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(out).unwrap();
    println!("{}", out.display());
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand");
    for size in [1024, 512, 256, 128, 64, 32, 16] {
        render(
            &dir.join("postmortem-icon.svg"),
            &dir.join(format!("postmortem-icon-{size}.png")),
            size,
        );
    }
    render(
        &dir.join("postmortem-wordmark.svg"),
        &dir.join("postmortem-wordmark.png"),
        1720,
    );
}
