//! P0 PoC (item 5): can a Rust-only stack cover Flect's image features?
//! Prints one line per check: OK/NG, sizes, timing. Not production code.
use std::time::Instant;

use image::{codecs::ico::IcoEncoder, ExtendedColorType, ImageEncoder, Rgba, RgbaImage};

fn sample(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let a = if x < w / 8 { 0 } else { 255 };
        Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, ((x ^ y) & 0xff) as u8, a])
    })
}

fn report(name: &str, r: Result<usize, String>, t: Instant) {
    match r {
        Ok(n) => println!("OK  {name:<28} {n:>9} bytes  {:>7.1} ms", t.elapsed().as_secs_f64() * 1e3),
        Err(e) => println!("NG  {name:<28} {e}"),
    }
}

fn main() {
    let img = sample(2000, 1500);
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();

    let t = Instant::now();
    let mut png = Vec::new();
    let r = img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map(|_| png.len());
    report("png encode (alpha)", r.map_err(|e| e.to_string()), t);

    let t = Instant::now();
    let mut buf = Vec::new();
    let r = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 80)
        .encode_image(&rgb).map(|_| buf.len());
    report("jpeg encode q80", r.map_err(|e| e.to_string()), t);

    let t = Instant::now();
    let mut buf = Vec::new();
    let r = image::codecs::webp::WebPEncoder::new_lossless(&mut buf)
        .write_image(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgba8)
        .map(|_| buf.len());
    report("webp lossless (image crate)", r.map_err(|e| e.to_string()), t);

    let t = Instant::now();
    let lossy = webp::Encoder::from_rgba(img.as_raw(), img.width(), img.height()).encode(75.0);
    report("webp lossy q75 (libwebp)", Ok(lossy.len()), t);

    let t = Instant::now();
    let dec = webp::Decoder::new(&lossy).decode().map(|d| (d.width(), d.height()));
    report("webp lossy decode", if dec == Some((2000, 1500)) { Ok(lossy.len()) } else { Err("size mismatch".into()) }, t);

    let t = Instant::now();
    let mut buf = Vec::new();
    let small = image::imageops::resize(&img, 256, 256, image::imageops::FilterType::Lanczos3);
    let r = IcoEncoder::new(&mut buf)
        .write_image(small.as_raw(), 256, 256, ExtendedColorType::Rgba8).map(|_| buf.len());
    report("ico 256x256", r.map_err(|e| e.to_string()), t);

    let t = Instant::now();
    let mut buf = Vec::new();
    let r = image::codecs::gif::GifEncoder::new(&mut buf)
        .encode(small.as_raw(), 256, 256, ExtendedColorType::Rgba8);
    let r = r.map(|_| buf.len());
    report("gif encode", r.map_err(|e| e.to_string()), t);

    // SVG -> raster
    let t = Instant::now();
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#4f46e5"/><circle cx="100" cy="50" r="30" fill="#fff"/></svg>"##;
    let r = (|| -> Result<usize, String> {
        let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).map_err(|e| e.to_string())?;
        let mut pm = resvg::tiny_skia::Pixmap::new(400, 200).ok_or("pixmap")?;
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(2.0, 2.0), &mut pm.as_mut());
        Ok(pm.encode_png().map_err(|e| e.to_string())?.len())
    })();
    report("svg render (resvg)", r, t);

    // SIMD resize vs image crate
    let t = Instant::now();
    let src = image::DynamicImage::ImageRgba8(img.clone());
    let mut dst = fast_image_resize::images::Image::new(1000, 750, fast_image_resize::PixelType::U8x4);
    let r = fast_image_resize::Resizer::new()
        .resize(&src, &mut dst, &fast_image_resize::ResizeOptions::new()
            .resize_alg(fast_image_resize::ResizeAlg::Convolution(fast_image_resize::FilterType::Lanczos3)))
        .map(|_| dst.buffer().len());
    report("resize 2000->1000 fast_image_resize", r.map_err(|e| e.to_string()), t);

    let t = Instant::now();
    let out = image::imageops::resize(&img, 1000, 750, image::imageops::FilterType::Lanczos3);
    report("resize 2000->1000 image crate", Ok(out.as_raw().len()), t);

    // arbitrary-angle rotation, transparent corners
    let t = Instant::now();
    let rot = imageproc::geometric_transformations::rotate_about_center(
        &img, 37f32.to_radians(), imageproc::geometric_transformations::Interpolation::Bilinear, Rgba([0, 0, 0, 0]));
    let corner = rot.get_pixel(0, 0)[3];
    report("rotate 37deg (corner alpha=0)", if corner == 0 { Ok(rot.as_raw().len()) } else { Err(format!("corner alpha {corner}")) }, t);
}
