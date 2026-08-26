use resvg::{tiny_skia, usvg};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

const SIZE: i32 = 28;

pub(crate) fn buffer() -> MemoryRenderBuffer {
    let pixels = rasterize();
    MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Abgr8888,
        (SIZE, SIZE),
        1,
        Transform::Normal,
        None,
    )
}

fn rasterize() -> Vec<u8> {
    let tree = usvg::Tree::from_data(
        include_bytes!("../assets/arrow-cursor.svg"),
        &usvg::Options::default(),
    )
    .expect("bundled cursor SVG must be valid");
    let mut pixmap = tiny_skia::Pixmap::new(SIZE as u32, SIZE as u32).unwrap();
    let scale = SIZE as f32 / tree.size().width();
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_cursor_has_visible_and_transparent_pixels() {
        let pixels = rasterize();
        assert_eq!(pixels.len(), (SIZE * SIZE * 4) as usize);
        let mut alphas = pixels.chunks_exact(4).map(|pixel| pixel[3]);
        assert!(alphas.clone().any(|alpha| alpha > 0));
        assert!(alphas.clone().any(|alpha| alpha == 0));
        assert!(alphas.any(|alpha| alpha > 0 && alpha < 255));
    }
}
