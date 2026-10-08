//! The Luna Moth logo

use resvg::{tiny_skia, usvg};

const LOGO_SVG: &[u8] = include_bytes!("../assets/luna-moth.svg");

/// Draw the logo into a `size` by `size` square of RGBA pixels (with unmultiplied alpha)
pub fn rasterize(size: u32) -> Vec<u8> {
    let tree =
        usvg::Tree::from_data(LOGO_SVG, &usvg::Options::default()).expect("bundled logo is valid");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("logo size is not zero");
    let logo_size = tree.size();
    let scale = size as f32 / logo_size.width().max(logo_size.height());
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_the_logo() {
        let size = 256;
        let rgba = rasterize(size);
        assert_eq!(rgba.len(), (size * size * 4) as usize);
        // Something got drawn, on a transparent background
        assert!(rgba.chunks(4).any(|pixel| pixel[3] == 255));
        assert_eq!(rgba[3], 0);
    }
}
