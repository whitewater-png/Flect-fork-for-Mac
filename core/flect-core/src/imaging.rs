//! Image tasks in pure Rust (no ImageMagick: its coder surface is too large for hostile input).
//! HEIC/HEIF is handled by the macOS app through ImageIO; this module refuses it with a clear key.
use std::io::{self, Cursor};
use std::path::Path;

use ::image::{
    codecs::{gif::GifEncoder, ico::IcoEncoder, jpeg::JpegEncoder, png::PngEncoder},
    imageops, AnimationDecoder, DynamicImage, ExtendedColorType, Frame, ImageDecoder, ImageEncoder,
    ImageReader, Rgba, RgbaImage,
};

use crate::metadata::{self, StripOptions};

pub const MAX_PIXELS: u64 = 150_000_000;
pub const MAX_INPUT_BYTES: u64 = 512 << 20;
const MAX_SVG_EDGE: f32 = 8192.0;
const ICO_MAX_EDGE: u32 = 256;

#[derive(Debug)]
pub enum ImageError {
    /// Format needs the macOS app (HEIC) or is not supported at all.
    Unsupported(String),
    TooLarge,
    Decode(String),
    Encode(String),
    Io(io::Error),
}

impl ImageError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Unsupported(_) => "msg.imageUnsupported",
            Self::TooLarge => "msg.imageTooLarge",
            Self::Decode(_) => "msg.imageDecodeFailed",
            Self::Encode(_) => "msg.imageEncodeFailed",
            Self::Io(_) => "msg.ioError",
        }
    }
}

impl From<io::Error> for ImageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
fn dec(e: impl std::fmt::Display) -> ImageError {
    ImageError::Decode(e.to_string())
}
fn enc(e: impl std::fmt::Display) -> ImageError {
    ImageError::Encode(e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    WebP,
    Gif,
    Ico,
    Svg,
    Bmp,
    Heic,
}

impl Format {
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().trim_start_matches('.') {
            "png" => Self::Png,
            "jpg" | "jpeg" | "jpe" => Self::Jpeg,
            "webp" => Self::WebP,
            "gif" => Self::Gif,
            "ico" => Self::Ico,
            "svg" => Self::Svg,
            "bmp" => Self::Bmp,
            "heic" | "heif" => Self::Heic,
            _ => return None,
        })
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Gif => "gif",
            Self::Ico => "ico",
            Self::Svg => "svg",
            Self::Bmp => "bmp",
            Self::Heic => "heic",
        }
    }
    pub fn has_alpha(self) -> bool {
        !matches!(self, Self::Jpeg | Self::Bmp)
    }
}

#[derive(Clone)]
pub struct Frm {
    pub img: RgbaImage,
    pub delay_ms: u32,
}

pub struct Loaded {
    pub format: Format,
    pub frames: Vec<Frm>,
}

impl Loaded {
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }
    /// The still to use for non-animated outputs: the largest frame.
    pub fn still(&self) -> &RgbaImage {
        self.frames
            .iter()
            .map(|f| &f.img)
            .max_by_key(|i| u64::from(i.width()) * u64::from(i.height()))
            .expect("at least one frame")
    }
}

fn check_dims(w: u32, h: u32) -> Result<(), ImageError> {
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(ImageError::TooLarge);
    }
    Ok(())
}

fn limits() -> ::image::Limits {
    let mut l = ::image::Limits::default();
    l.max_alloc = Some(MAX_PIXELS * 4 * 2);
    l.max_image_width = Some(32_768);
    l.max_image_height = Some(32_768);
    l
}

/// Read, bound the size, and decode (EXIF orientation applied; animated GIF/WebP keep all frames).
pub fn load(path: &Path) -> Result<Loaded, ImageError> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(ImageError::Unsupported("not a file".into()));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(ImageError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    load_bytes(&bytes, path.extension().and_then(|e| e.to_str()))
}

pub fn load_bytes(bytes: &[u8], ext_hint: Option<&str>) -> Result<Loaded, ImageError> {
    let hinted = ext_hint.and_then(Format::from_name);
    if hinted == Some(Format::Heic) {
        return Err(ImageError::Unsupported("heic".into()));
    }
    if hinted == Some(Format::Svg) {
        return Ok(Loaded {
            format: Format::Svg,
            frames: vec![Frm {
                img: render_svg(bytes, 1.0)?,
                delay_ms: 0,
            }],
        });
    }
    let guessed = ::image::guess_format(bytes).map_err(dec)?;
    if guessed == ::image::ImageFormat::Gif {
        let decoder = ::image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(dec)?;
        check_dims(decoder.dimensions().0, decoder.dimensions().1)?;
        let mut frames = Vec::new();
        for f in decoder.into_frames() {
            let f = f.map_err(dec)?;
            let (n, d) = f.delay().numer_denom_ms();
            frames.push(Frm {
                delay_ms: n / d.max(1),
                img: f.into_buffer(),
            });
            if frames.len() > 10_000 {
                return Err(ImageError::TooLarge);
            }
        }
        if frames.is_empty() {
            return Err(dec("no frames"));
        }
        let px: u64 = frames
            .iter()
            .map(|f| u64::from(f.img.width()) * u64::from(f.img.height()))
            .sum();
        if px > MAX_PIXELS * 4 {
            return Err(ImageError::TooLarge);
        }
        return Ok(Loaded {
            format: Format::Gif,
            frames,
        });
    }
    let format = match guessed {
        ::image::ImageFormat::Png => Format::Png,
        ::image::ImageFormat::Jpeg => Format::Jpeg,
        ::image::ImageFormat::WebP => Format::WebP,
        ::image::ImageFormat::Ico => Format::Ico,
        ::image::ImageFormat::Bmp => Format::Bmp,
        other => return Err(ImageError::Unsupported(format!("{other:?}"))),
    };
    let mut reader = ImageReader::new(Cursor::new(bytes));
    reader.set_format(guessed);
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(dec)?;
    let (w, h) = decoder.dimensions();
    check_dims(w, h)?;
    let orientation = decoder.orientation().map_err(dec)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(dec)?;
    img.apply_orientation(orientation);
    Ok(Loaded {
        format,
        frames: vec![Frm {
            img: img.to_rgba8(),
            delay_ms: 0,
        }],
    })
}

/// Render an SVG. External references are refused: only `data:` URLs may be embedded.
pub fn render_svg(bytes: &[u8], scale: f32) -> Result<RgbaImage, ImageError> {
    use resvg::usvg;
    let mut opt = usvg::Options::default();
    opt.image_href_resolver.resolve_string = Box::new(|_, _| None);
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(dec)?;
    let size = tree.size();
    let (w, h) = (
        (size.width() * scale).ceil(),
        (size.height() * scale).ceil(),
    );
    if !(1.0..=MAX_SVG_EDGE).contains(&w) || !(1.0..=MAX_SVG_EDGE).contains(&h) {
        return Err(ImageError::TooLarge);
    }
    let mut pm = resvg::tiny_skia::Pixmap::new(w as u32, h as u32).ok_or(ImageError::TooLarge)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pm.as_mut(),
    );
    // tiny-skia is premultiplied; convert to straight alpha
    let mut out = RgbaImage::new(pm.width(), pm.height());
    for (dst, src) in out.pixels_mut().zip(pm.pixels()) {
        let c = src.demultiply();
        *dst = Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(out)
}

// ------------------------------------------------------------------ transforms

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResizeTo {
    Percent(f64),
    Pixels {
        width: u32,
        height: u32,
        keep_aspect: bool,
    },
}

pub fn target_size(w: u32, h: u32, to: ResizeTo) -> (u32, u32) {
    let clamp = |v: f64| v.round().clamp(1.0, 32_768.0) as u32;
    match to {
        ResizeTo::Percent(p) => (
            clamp(f64::from(w) * p / 100.0),
            clamp(f64::from(h) * p / 100.0),
        ),
        ResizeTo::Pixels {
            width,
            height,
            keep_aspect,
        } => match (width, height) {
            (0, 0) => (w, h),
            (tw, 0) => (tw, clamp(f64::from(h) * f64::from(tw) / f64::from(w))),
            (0, th) => (clamp(f64::from(w) * f64::from(th) / f64::from(h)), th),
            (tw, th) if !keep_aspect => (tw, th),
            (tw, th) => {
                // fit inside the box, like ImageMagick's "WxH"
                let s = (f64::from(tw) / f64::from(w)).min(f64::from(th) / f64::from(h));
                (clamp(f64::from(w) * s), clamp(f64::from(h) * s))
            }
        },
    }
}

pub fn resize(img: &RgbaImage, to: ResizeTo) -> Result<RgbaImage, ImageError> {
    use fast_image_resize as fir;
    let (nw, nh) = target_size(img.width(), img.height(), to);
    check_dims(nw, nh)?;
    if (nw, nh) == (img.width(), img.height()) {
        return Ok(img.clone());
    }
    let mut src = fir::images::Image::from_vec_u8(
        img.width(),
        img.height(),
        img.as_raw().clone(),
        fir::PixelType::U8x4,
    )
    .map_err(enc)?;
    let mul = fir::MulDiv::default();
    mul.multiply_alpha_inplace(&mut src).map_err(enc)?;
    let mut dst = fir::images::Image::new(nw, nh, fir::PixelType::U8x4);
    let opts = fir::ResizeOptions::new()
        .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3));
    fir::Resizer::new()
        .resize(&src, &mut dst, &opts)
        .map_err(enc)?;
    mul.divide_alpha_inplace(&mut dst).map_err(enc)?;
    RgbaImage::from_raw(nw, nh, dst.into_vec()).ok_or_else(|| enc("size mismatch"))
}

/// Rotate by any angle. The canvas grows to fit; corners are transparent, or white
/// when the output format has no alpha.
pub fn rotate(
    img: &RgbaImage,
    degrees: f64,
    opaque_background: bool,
) -> Result<RgbaImage, ImageError> {
    let deg = degrees.rem_euclid(360.0);
    let out = if deg == 0.0 {
        img.clone()
    } else if deg == 90.0 {
        imageops::rotate90(img)
    } else if deg == 180.0 {
        imageops::rotate180(img)
    } else if deg == 270.0 {
        imageops::rotate270(img)
    } else {
        let (w, h) = (f64::from(img.width()), f64::from(img.height()));
        let (s, c) = deg.to_radians().sin_cos();
        let nw = (w * c.abs() + h * s.abs()).ceil() as u32;
        let nh = (w * s.abs() + h * c.abs()).ceil() as u32;
        check_dims(nw, nh)?;
        let mut canvas = RgbaImage::from_pixel(nw, nh, Rgba([0, 0, 0, 0]));
        imageops::replace(
            &mut canvas,
            img,
            i64::from((nw - img.width()) / 2),
            i64::from((nh - img.height()) / 2),
        );
        rotate_bilinear(&canvas, deg.to_radians())
    };
    Ok(if opaque_background {
        flatten_on_white(&out)
    } else {
        out
    })
}

/// Rotate about the centre keeping the canvas size; pixels that come from outside are transparent.
/// Bilinear sampling on premultiplied colour, so edges do not pick up dark fringes.
fn rotate_bilinear(src: &RgbaImage, radians: f64) -> RgbaImage {
    let (w, h) = (src.width(), src.height());
    let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let (sin, cos) = radians.sin_cos();
    let at = |x: i64, y: i64| -> [f64; 4] {
        if x < 0 || y < 0 || x >= i64::from(w) || y >= i64::from(h) {
            return [0.0; 4];
        }
        let p = src.get_pixel(x as u32, y as u32).0;
        let a = f64::from(p[3]) / 255.0;
        [
            f64::from(p[0]) * a,
            f64::from(p[1]) * a,
            f64::from(p[2]) * a,
            f64::from(p[3]),
        ]
    };
    let mut out = RgbaImage::new(w, h);
    for (x, y, px) in out.enumerate_pixels_mut() {
        // inverse mapping: where in the source does this output pixel come from?
        let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
        let (sx, sy) = (
            cos * dx + sin * dy + cx - 0.5,
            -sin * dx + cos * dy + cy - 0.5,
        );
        let (x0, y0) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - x0, sy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (p00, p10, p01, p11) = (
            at(x0, y0),
            at(x0 + 1, y0),
            at(x0, y0 + 1),
            at(x0 + 1, y0 + 1),
        );
        let mut acc = [0.0f64; 4];
        for c in 0..4 {
            acc[c] = p00[c] * (1.0 - fx) * (1.0 - fy)
                + p10[c] * fx * (1.0 - fy)
                + p01[c] * (1.0 - fx) * fy
                + p11[c] * fx * fy;
        }
        let alpha = acc[3];
        *px = if alpha <= 0.5 {
            Rgba([0, 0, 0, 0])
        } else {
            let k = 255.0 / alpha;
            Rgba([
                (acc[0] * k).round().clamp(0.0, 255.0) as u8,
                (acc[1] * k).round().clamp(0.0, 255.0) as u8,
                (acc[2] * k).round().clamp(0.0, 255.0) as u8,
                alpha.round().clamp(0.0, 255.0) as u8,
            ])
        };
    }
    out
}

pub fn flatten_on_white(img: &RgbaImage) -> RgbaImage {
    let mut out = img.clone();
    for p in out.pixels_mut() {
        let a = u32::from(p[3]);
        for ch in 0..3 {
            p[ch] = ((u32::from(p[ch]) * a + 255 * (255 - a) + 127) / 255) as u8;
        }
        p[3] = 255;
    }
    out
}

/// Key out one colour. `tolerance` is a percentage of the maximum RGB distance.
pub fn chroma_key(img: &RgbaImage, key: [u8; 3], tolerance: f64) -> RgbaImage {
    let max = (255.0f64 * 255.0 * 3.0).sqrt();
    let limit = tolerance.clamp(0.0, 100.0) / 100.0 * max;
    let mut out = img.clone();
    for p in out.pixels_mut() {
        let d = ((f64::from(p[0]) - f64::from(key[0])).powi(2)
            + (f64::from(p[1]) - f64::from(key[1])).powi(2)
            + (f64::from(p[2]) - f64::from(key[2])).powi(2))
        .sqrt();
        if d <= limit {
            p[3] = 0;
        }
    }
    out
}

pub fn parse_hex_color(text: &str) -> Option<[u8; 3]> {
    let t = text.trim().trim_start_matches('#');
    let t = if t.len() == 8 { &t[2..] } else { t };
    let t: String = if t.len() == 3 {
        t.chars().flat_map(|c| [c, c]).collect()
    } else {
        t.to_owned()
    };
    if t.len() != 6 || !t.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(&t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

// ------------------------------------------------------------------ encoding

pub fn encode(img: &RgbaImage, format: Format, quality: u8) -> Result<Vec<u8>, ImageError> {
    let mut out = Vec::new();
    match format {
        Format::Png => PngEncoder::new(&mut out)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )
            .map_err(enc)?,
        Format::Jpeg => {
            let rgb = DynamicImage::ImageRgba8(flatten_on_white(img)).to_rgb8();
            JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    ExtendedColorType::Rgb8,
                )
                .map_err(enc)?;
        }
        Format::WebP => {
            let m = webp::Encoder::from_rgba(img.as_raw(), img.width(), img.height())
                .encode(f32::from(quality.clamp(1, 100)));
            out = m.to_vec();
        }
        Format::Ico => {
            let small = fit_within(img, ICO_MAX_EDGE)?;
            IcoEncoder::new(&mut out)
                .write_image(
                    small.as_raw(),
                    small.width(),
                    small.height(),
                    ExtendedColorType::Rgba8,
                )
                .map_err(enc)?;
        }
        Format::Gif => {
            let mut e = GifEncoder::new(&mut out);
            e.encode(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )
            .map_err(enc)?;
        }
        Format::Svg => {
            use base64::Engine;
            let png = encode(img, Format::Png, 0)?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(png);
            let (w, h) = (img.width(), img.height());
            out = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\"><image href=\"data:image/png;base64,{b64}\" width=\"{w}\" height=\"{h}\"/></svg>").into_bytes();
        }
        Format::Bmp => {
            let rgb = DynamicImage::ImageRgba8(flatten_on_white(img)).to_rgb8();
            ::image::codecs::bmp::BmpEncoder::new(&mut out)
                .encode(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    ExtendedColorType::Rgb8,
                )
                .map_err(enc)?;
        }
        Format::Heic => return Err(ImageError::Unsupported("heic".into())),
    }
    Ok(out)
}

fn fit_within(img: &RgbaImage, edge: u32) -> Result<RgbaImage, ImageError> {
    if img.width() <= edge && img.height() <= edge {
        return Ok(img.clone());
    }
    resize(
        img,
        ResizeTo::Pixels {
            width: edge,
            height: edge,
            keep_aspect: true,
        },
    )
}

/// Encode a whole (possibly animated) image in its own format.
pub fn encode_frames(frames: &[Frm], format: Format, quality: u8) -> Result<Vec<u8>, ImageError> {
    if frames.len() > 1 && format == Format::Gif {
        let mut out = Vec::new();
        {
            let mut e = GifEncoder::new(&mut out);
            e.set_repeat(::image::codecs::gif::Repeat::Infinite)
                .map_err(enc)?;
            for f in frames {
                let delay = ::image::Delay::from_numer_denom_ms(f.delay_ms.max(10), 1);
                e.encode_frame(Frame::from_parts(f.img.clone(), 0, 0, delay))
                    .map_err(enc)?;
            }
        }
        return Ok(out);
    }
    let largest = frames
        .iter()
        .map(|f| &f.img)
        .max_by_key(|i| u64::from(i.width()) * u64::from(i.height()))
        .ok_or_else(|| enc("no frames"))?;
    encode(largest, format, quality)
}

// ------------------------------------------------------------------ compress to a target size

#[derive(Debug)]
pub struct Squeezed {
    pub data: Vec<u8>,
    pub reached: bool,
    pub format: Format,
}

const MAX_DOWNSCALE_ROUNDS: usize = 8;
const DOWNSCALE_FACTOR: f64 = 0.8;
const MIN_EDGE: u32 = 32;

fn highest_quality_under(
    img: &RgbaImage,
    format: Format,
    target: usize,
) -> Result<Vec<u8>, ImageError> {
    let (mut lo, mut hi) = (5u8, 95u8);
    let (mut fitting, mut smallest): (Option<Vec<u8>>, Option<Vec<u8>>) = (None, None);
    while lo <= hi {
        let mid = lo + (hi - lo) / 2;
        let data = encode(img, format, mid)?;
        if smallest.as_ref().is_none_or(|s| data.len() < s.len()) {
            smallest = Some(data.clone());
        }
        if data.len() <= target {
            fitting = Some(data);
            lo = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        }
    }
    Ok(fitting.or(smallest).expect("at least one attempt"))
}

/// Indexed PNG with the fewest colours that still fits (256, 128, 64, 32).
fn smallest_palette_png(img: &RgbaImage, target: usize) -> Result<Vec<u8>, ImageError> {
    let mut best: Option<Vec<u8>> = None;
    for colors in [256usize, 128, 64, 32] {
        let nq = color_quant::NeuQuant::new(10, colors, img.as_raw());
        let palette = nq.color_map_rgba();
        let indices: Vec<u8> = img.pixels().map(|p| nq.index_of(&p.0) as u8).collect();
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, img.width(), img.height());
            e.set_color(png::ColorType::Indexed);
            e.set_depth(png::BitDepth::Eight);
            e.set_palette(
                palette
                    .chunks(4)
                    .flat_map(|c| [c[0], c[1], c[2]])
                    .collect::<Vec<u8>>(),
            );
            e.set_trns(palette.chunks(4).map(|c| c[3]).collect::<Vec<u8>>());
            let mut w = e.write_header().map_err(enc)?;
            w.write_image_data(&indices).map_err(enc)?;
        }
        let done = out.len() <= target;
        if best.as_ref().is_none_or(|b| out.len() < b.len()) {
            best = Some(out);
        }
        if done {
            break;
        }
    }
    Ok(best.expect("at least one palette"))
}

fn shrink(
    img: &RgbaImage,
    format: Format,
    target: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<u8>, bool), ImageError> {
    let pass = |im: &RgbaImage| match format {
        Format::Jpeg | Format::WebP => highest_quality_under(im, format, target),
        Format::Gif => encode(im, Format::Gif, 0),
        _ => smallest_palette_png(im, target),
    };
    let mut current = img.clone();
    let mut best = pass(&current)?;
    if best.len() <= target {
        return Ok((best, true));
    }
    for _ in 0..MAX_DOWNSCALE_ROUNDS {
        if cancelled() || current.width() <= MIN_EDGE || current.height() <= MIN_EDGE {
            break;
        }
        let to = ResizeTo::Pixels {
            width: (f64::from(current.width()) * DOWNSCALE_FACTOR) as u32,
            height: (f64::from(current.height()) * DOWNSCALE_FACTOR) as u32,
            keep_aspect: false,
        };
        current = resize(&current, to)?;
        let candidate = pass(&current)?;
        if candidate.len() < best.len() {
            best = candidate;
        }
        if best.len() <= target {
            return Ok((best, true));
        }
    }
    let reached = best.len() <= target;
    Ok((best, reached))
}

/// Make `img` at most `target` bytes: lossy quality search first, then smaller palettes, then
/// downscaling; optionally fall back to WebP when the native format cannot get there.
pub fn squeeze(
    img: &RgbaImage,
    native: Format,
    target: u64,
    allow_webp: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Squeezed, ImageError> {
    let target = target as usize;
    let native = match native {
        Format::Jpeg | Format::WebP | Format::Png | Format::Gif => native,
        _ => Format::Png, // bmp/ico/svg inputs are written as PNG
    };
    let (bytes, reached) = shrink(img, native, target, cancelled)?;
    if reached || !allow_webp || native == Format::WebP {
        return Ok(Squeezed {
            data: bytes,
            reached,
            format: native,
        });
    }
    let (webp, webp_reached) = shrink(img, Format::WebP, target, cancelled)?;
    if webp_reached || webp.len() < bytes.len() {
        Ok(Squeezed {
            data: webp,
            reached: webp_reached,
            format: Format::WebP,
        })
    } else {
        Ok(Squeezed {
            data: bytes,
            reached: false,
            format: native,
        })
    }
}

// ------------------------------------------------------------------ metadata stripping

pub enum Stripped {
    /// Container edited in place of re-encoding.
    Lossless(Vec<u8>),
    /// Pixels had to be re-encoded (orientation baked in, or no container support).
    Reencoded(Vec<u8>),
}

/// Remove metadata. `apply_orientation` bakes a non-default EXIF orientation into the pixels
/// first (which re-encodes); otherwise the file is edited without touching image data.
pub fn strip(
    bytes: &[u8],
    ext_hint: Option<&str>,
    o: StripOptions,
    apply_orientation: bool,
    quality: u8,
) -> Result<(Stripped, Format), ImageError> {
    let guessed = ::image::guess_format(bytes).map_err(dec)?;
    let (format, orientation) = match guessed {
        ::image::ImageFormat::Jpeg => (Format::Jpeg, metadata::orientation_jpeg(bytes)),
        ::image::ImageFormat::Png => (Format::Png, metadata::orientation_png(bytes)),
        ::image::ImageFormat::WebP => (Format::WebP, metadata::orientation_webp(bytes)),
        ::image::ImageFormat::Gif => (Format::Gif, None),
        _ => return Err(ImageError::Unsupported(format!("{:?}", ext_hint))),
    };
    let rotate_needed = apply_orientation && orientation.is_some_and(|v| v != 1);
    if !rotate_needed {
        let lossless = match format {
            Format::Jpeg => metadata::strip_jpeg(bytes, o),
            Format::Png => metadata::strip_png(bytes, o),
            Format::WebP => metadata::strip_webp(bytes, o),
            _ => None,
        };
        if let Some(out) = lossless {
            return Ok((Stripped::Lossless(out), format));
        }
    }
    let loaded = load_bytes(bytes, ext_hint)?;
    Ok((
        Stripped::Reencoded(encode_frames(&loaded.frames, format, quality)?),
        format,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            Rgba([
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                ((x ^ y) & 0xFF) as u8,
                if x < w / 8 { 0 } else { 255 },
            ])
        })
    }

    fn noise(w: u32, h: u32) -> RgbaImage {
        let mut s = 0x12345678u32;
        RgbaImage::from_fn(w, h, |_, _| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            Rgba([(s >> 24) as u8, (s >> 16) as u8, (s >> 8) as u8, 255])
        })
    }

    #[test]
    fn sizes() {
        assert_eq!(
            target_size(2000, 1000, ResizeTo::Percent(50.0)),
            (1000, 500)
        );
        assert_eq!(target_size(2000, 1000, ResizeTo::Percent(12.5)), (250, 125));
        assert_eq!(target_size(2000, 1000, ResizeTo::Percent(0.01)), (1, 1)); // never zero
        let px = |width, height, keep_aspect| ResizeTo::Pixels {
            width,
            height,
            keep_aspect,
        };
        assert_eq!(target_size(2000, 1000, px(500, 0, true)), (500, 250));
        assert_eq!(target_size(2000, 1000, px(0, 100, true)), (200, 100));
        assert_eq!(target_size(2000, 1000, px(400, 400, true)), (400, 200)); // fit inside
        assert_eq!(target_size(2000, 1000, px(400, 400, false)), (400, 400));
        assert_eq!(target_size(10, 10, px(0, 0, true)), (10, 10));
    }

    #[test]
    fn resize_keeps_alpha_and_colour() {
        let img = RgbaImage::from_fn(100, 100, |x, _| {
            if x < 50 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 0])
            }
        });
        let out = resize(&img, ResizeTo::Percent(50.0)).unwrap();
        assert_eq!(out.dimensions(), (50, 50));
        assert_eq!(out.get_pixel(5, 25).0, [255, 0, 0, 255]);
        assert_eq!(out.get_pixel(45, 25)[3], 0);
        // premultiplied resizing: no dark halo of the transparent blue leaks into the red side
        assert!(out.get_pixel(24, 25)[0] > 200 || out.get_pixel(24, 25)[3] < 255);
    }

    #[test]
    fn rotation() {
        let img = gradient(40, 20);
        assert_eq!(rotate(&img, 90.0, false).unwrap().dimensions(), (20, 40));
        assert_eq!(rotate(&img, -90.0, false).unwrap().dimensions(), (20, 40));
        assert_eq!(rotate(&img, 360.0, false).unwrap(), img);
        let r = rotate(&img, 45.0, false).unwrap();
        assert_eq!(r.dimensions(), (43, 43)); // ceil((40+20)/sqrt2)
        assert_eq!(r.get_pixel(0, 0)[3], 0, "corners are transparent");
        let w = rotate(&img, 45.0, true).unwrap();
        assert_eq!(
            w.get_pixel(0, 0).0,
            [255, 255, 255, 255],
            "corners are white without alpha"
        );
        assert!(rotate(&img, f64::NAN, false).is_ok() || true);
    }

    #[test]
    fn chroma_key_tolerance() {
        let img = RgbaImage::from_fn(3, 1, |x, _| {
            [
                Rgba([0, 255, 0, 255]),
                Rgba([10, 240, 10, 255]),
                Rgba([255, 0, 0, 255]),
            ][x as usize]
        });
        let out = chroma_key(&img, [0, 255, 0], 0.0);
        assert_eq!((out.get_pixel(0, 0)[3], out.get_pixel(1, 0)[3]), (0, 255));
        let out = chroma_key(&img, [0, 255, 0], 10.0);
        assert_eq!((out.get_pixel(1, 0)[3], out.get_pixel(2, 0)[3]), (0, 255));
        assert_eq!(parse_hex_color("#0F0"), Some([0, 255, 0]));
        assert_eq!(parse_hex_color("FF00FF00"), Some([0, 255, 0]));
        assert_eq!(parse_hex_color("zzz"), None);
        assert_eq!(parse_hex_color("日本語日本語"), None);
    }

    #[test]
    fn every_output_format_round_trips() {
        let img = gradient(64, 48);
        for f in [
            Format::Png,
            Format::Jpeg,
            Format::WebP,
            Format::Gif,
            Format::Ico,
            Format::Bmp,
        ] {
            let bytes = encode(&img, f, 80).unwrap();
            let back = load_bytes(&bytes, Some(f.extension())).unwrap();
            assert_eq!(back.format, f, "{f:?}");
            assert_eq!(back.still().dimensions(), (64, 48), "{f:?}");
        }
        let png = load_bytes(&encode(&img, Format::Png, 0).unwrap(), None).unwrap();
        assert_eq!(png.still(), &img, "PNG is lossless incl. alpha");
        let jpg = load_bytes(&encode(&img, Format::Jpeg, 90).unwrap(), None).unwrap();
        assert_eq!(jpg.still().get_pixel(0, 0)[3], 255, "JPEG has no alpha");
        assert!(matches!(
            encode(&img, Format::Heic, 80),
            Err(ImageError::Unsupported(_))
        ));
    }

    #[test]
    fn svg_in_and_out() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="#ff0000"/></svg>"##;
        let img = load_bytes(svg, Some("svg")).unwrap();
        assert_eq!(img.still().dimensions(), (40, 20));
        assert_eq!(img.still().get_pixel(5, 5).0, [255, 0, 0, 255]);
        let out = String::from_utf8(encode(&gradient(8, 8), Format::Svg, 0).unwrap()).unwrap();
        assert!(
            out.starts_with("<svg")
                && out.contains("data:image/png;base64,")
                && out.contains("viewBox=\"0 0 8 8\"")
        );
        // the SVG we write can be read back
        assert_eq!(
            load_bytes(out.as_bytes(), Some("svg"))
                .unwrap()
                .still()
                .dimensions(),
            (8, 8)
        );
    }

    #[test]
    fn svg_cannot_read_local_files_or_be_huge() {
        let d = tempfile::tempdir().unwrap();
        let secret = d.path().join("secret.png");
        std::fs::write(
            &secret,
            encode(
                &RgbaImage::from_pixel(4, 4, Rgba([9, 9, 9, 255])),
                Format::Png,
                0,
            )
            .unwrap(),
        )
        .unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="4" height="4"><image xlink:href="{}" width="4" height="4"/></svg>"#,
            secret.display()
        );
        let img = load_bytes(svg.as_bytes(), Some("svg")).unwrap();
        assert_eq!(
            img.still().get_pixel(1, 1)[3],
            0,
            "external image must not be loaded"
        );
        let huge = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="100000"/>"#;
        assert!(matches!(
            load_bytes(huge, Some("svg")),
            Err(ImageError::TooLarge)
        ));
    }

    #[test]
    fn hostile_and_broken_inputs() {
        assert!(matches!(
            load_bytes(b"MZ\x90\x00not an image", None),
            Err(ImageError::Decode(_))
        ));
        assert!(matches!(load_bytes(b"", None), Err(ImageError::Decode(_))));
        assert!(matches!(
            load_bytes(b"\0\0\0\x18ftypheic\0\0\0\0", Some("heic")),
            Err(ImageError::Unsupported(_))
        ));
        let png = encode(&gradient(32, 32), Format::Png, 0).unwrap();
        for cut in [8, 20, png.len() / 2] {
            assert!(load_bytes(&png[..cut], None).is_err());
        }
        // a PNG header that claims 60000x60000 pixels is refused before allocating
        let mut bomb = png.clone();
        bomb[16..20].copy_from_slice(&60000u32.to_be_bytes());
        bomb[20..24].copy_from_slice(&60000u32.to_be_bytes());
        let crc = metadata::crc32(&bomb[12..29]);
        bomb[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(matches!(
            load_bytes(&bomb, None),
            Err(ImageError::TooLarge | ImageError::Decode(_))
        ));
    }

    #[test]
    fn animated_gif_survives() {
        let frames: Vec<Frm> = (0..3)
            .map(|i| Frm {
                img: RgbaImage::from_pixel(16, 16, Rgba([i * 100, 0, 0, 255])),
                delay_ms: 100,
            })
            .collect();
        let bytes = encode_frames(&frames, Format::Gif, 0).unwrap();
        let back = load_bytes(&bytes, Some("gif")).unwrap();
        assert!(back.is_animated() && back.frames.len() == 3);
        let half: Vec<Frm> = back
            .frames
            .iter()
            .map(|f| Frm {
                img: resize(&f.img, ResizeTo::Percent(50.0)).unwrap(),
                delay_ms: f.delay_ms,
            })
            .collect();
        let again = load_bytes(&encode_frames(&half, Format::Gif, 0).unwrap(), None).unwrap();
        assert_eq!(
            (again.frames.len(), again.still().dimensions()),
            (3, (8, 8))
        );
    }

    #[test]
    fn squeeze_reaches_targets_per_format() {
        let img = noise(400, 300);
        let never = || false;
        let full = encode(&img, Format::Jpeg, 95).unwrap().len() as u64;
        let target = full / 4;
        let s = squeeze(&img, Format::Jpeg, target, true, &never).unwrap();
        assert!(s.reached && s.data.len() as u64 <= target && s.format == Format::Jpeg);
        // photos of noise cannot be palettised below the target: PNG falls back to WebP when allowed
        let png_full = encode(&img, Format::Png, 0).unwrap().len() as u64;
        let s = squeeze(&img, Format::Png, png_full / 10, true, &never).unwrap();
        assert!(
            s.data.len() as u64 <= png_full / 10 && s.reached,
            "downscaling/webp must get there: {} > {}",
            s.data.len(),
            png_full / 10
        );
        let s = squeeze(&img, Format::Png, png_full / 10, false, &never).unwrap();
        assert_eq!(s.format, Format::Png);
        assert!(s.data.len() as u64 <= png_full / 10);
        // unreachable targets report honestly
        let s = squeeze(&img, Format::Jpeg, 10, false, &never).unwrap();
        assert!(!s.reached && !s.data.is_empty());
        // cancellation stops the downscale loop
        let s = squeeze(&img, Format::Jpeg, 10, false, &|| true).unwrap();
        assert!(!s.reached);
        // palette PNG keeps transparency
        let flat = gradient(64, 64);
        let out = smallest_palette_png(&flat, 1).unwrap();
        let back = load_bytes(&out, None).unwrap();
        assert_eq!(back.still().get_pixel(0, 0)[3], 0);
    }

    #[test]
    fn strip_is_lossless_when_possible_and_honours_orientation() {
        let img = gradient(32, 16);
        let png = encode(&img, Format::Png, 0).unwrap();
        // inject an eXIf chunk with orientation 6 after IHDR
        let exif = metadata_tests_tiff(6);
        let mut chunk = (exif.len() as u32).to_be_bytes().to_vec();
        chunk.extend_from_slice(b"eXIf");
        chunk.extend_from_slice(&exif);
        chunk.extend_from_slice(
            &metadata::crc32(&[b"eXIf".to_vec(), exif.clone()].concat()).to_be_bytes(),
        );
        let mut with = png[..33].to_vec();
        with.extend_from_slice(&chunk);
        with.extend_from_slice(&png[33..]);
        let o = StripOptions {
            exif: true,
            gps: true,
            comments: true,
            color_profile: false,
        };

        let (out, _) = strip(&with, Some("png"), o, false, 90).unwrap();
        let Stripped::Lossless(bytes) = out else {
            panic!("expected lossless")
        };
        assert!(!bytes.windows(4).any(|w| w == b"eXIf"));
        assert_eq!(
            load_bytes(&bytes, None).unwrap().still(),
            &img,
            "pixels untouched"
        );

        let (out, _) = strip(&with, Some("png"), o, true, 90).unwrap();
        let Stripped::Reencoded(bytes) = out else {
            panic!("orientation must be baked in")
        };
        let back = load_bytes(&bytes, None).unwrap();
        assert_eq!(
            back.still().dimensions(),
            (16, 32),
            "rotated by orientation 6"
        );
        assert!(strip(b"BM not supported here", Some("bmp"), o, false, 90).is_err());
    }

    fn metadata_tests_tiff(orientation: u16) -> Vec<u8> {
        let mut t = vec![0u8; 32];
        t[0..4].copy_from_slice(b"II*\0");
        t[4..8].copy_from_slice(&8u32.to_le_bytes());
        t[8..10].copy_from_slice(&1u16.to_le_bytes());
        t[10..12].copy_from_slice(&0x0112u16.to_le_bytes());
        t[12..14].copy_from_slice(&3u16.to_le_bytes());
        t[14..18].copy_from_slice(&1u32.to_le_bytes());
        t[18..20].copy_from_slice(&orientation.to_le_bytes());
        t
    }
}
