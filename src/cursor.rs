use resvg::{tiny_skia, usvg};
use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::Transform,
};

const SIZE: i32 = 28;
/// Slightly translucent so the content under the pointer stays visible.
pub(crate) const ALPHA: f32 = 0.85;

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
    fn bundled_cursor_is_a_white_outlined_black_arrow() {
        let pixels = rasterize();
        assert_eq!(pixels.len(), (SIZE * SIZE * 4) as usize);
        let pixel = |x: i32, y: i32| &pixels[((y * SIZE + x) * 4) as usize..][..4];
        assert_eq!(pixel(8, 8), [0, 0, 0, 255]);
        assert!(pixels.chunks_exact(4).any(|p| p == [255, 255, 255, 255]));
        assert_eq!(pixel(SIZE - 1, SIZE - 1)[3], 0);
    }
}
