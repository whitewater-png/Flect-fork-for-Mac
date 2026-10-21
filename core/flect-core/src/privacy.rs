//! Share-safe images: a read-only privacy check (what personal data a file carries) and a
//! "make safe to share" task (a new `_safe` copy without it).
//!
//! The check only reads. The copy never touches the source and never overwrites anything
//! (it goes through `tasks::write_result`). Stripping reuses the container-level, lossless
//! code in `metadata` / `imaging::strip`; on top of that a *whitelist* pass drops every other
//! block a container may hold (unknown APPn segments, private PNG chunks, bytes after the end of
//! a JPEG), and the result is re-scanned: if anything personal is still found, nothing is written.
//! All parsing is bounds-checked; malformed input yields an error, never a panic.
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};

use crate::imaging::{self as im, Format, ImageError, ResizeTo};
use crate::metadata::{self, StripOptions};
use crate::protocol::Event;
use crate::settings::Settings;
use crate::tasks::{write_result, Done, TaskError};

/// Name of the read-only report task.
pub const REPORT_TASK: &str = "image.privacy";
/// Name of the "make safe to share" task (`maxEdge=<px>` resizes; absent or 0 keeps the size).
pub const SAFE_TASK: &str = "image.makeSafe";

pub fn is_task(name: &str) -> bool {
    name == REPORT_TASK || name == SAFE_TASK
}

/// Hard cap on the long edge a user can ask for (matches the imaging limit).
const MAX_EDGE_LIMIT: u32 = 32_768;

// ------------------------------------------------------------------ report model

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// `gps`, `serial`, `owner`, `camera`, `lens`, `timestamps`, `software`, `thumbnail`,
    /// `makernote`, `description`, `xmp`, `iptc`, `comment`, `hidden`, `extra`.
    pub kind: &'static str,
    /// `high` (identifies a place, a device or a person) or `medium`.
    pub level: &'static str,
    /// The values found (short, control characters removed). Empty when only presence is known.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub format: &'static str,
    pub items: Vec<Item>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.items.is_empty()
    }
    pub fn to_json(&self, path: &Path) -> Value {
        json!({
            "path": path.to_string_lossy(),
            "file": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "format": self.format,
            "personal": self.items.len(),
            "items": self.items.iter().map(|i| json!({"kind": i.kind, "level": i.level, "detail": i.detail})).collect::<Vec<_>>(),
        })
    }
}

fn clean_text(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let t = t.trim();
    if t.chars().count() > 80 {
        let cut: String = t.chars().take(79).collect();
        format!("{cut}…")
    } else {
        t.to_owned()
    }
}

fn join_unique(values: Vec<String>) -> String {
    let mut seen: Vec<String> = Vec::new();
    for v in values {
        if !v.is_empty() && !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.join(", ")
}

// ------------------------------------------------------------------ read-only EXIF

struct Entry {
    tag: u16,
    ty: u16,
    count: u32,
    at: usize,
}

struct Exif<'a> {
    d: &'a [u8],
    le: bool,
}

impl<'a> Exif<'a> {
    fn new(d: &'a [u8]) -> Option<Self> {
        let le = match d.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let e = Self { d, le };
        (e.u16(2)? == 42).then_some(e)
    }
    fn u16(&self, o: usize) -> Option<u16> {
        let b: [u8; 2] = self.d.get(o..o.checked_add(2)?)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }
    fn u32(&self, o: usize) -> Option<u32> {
        let b: [u8; 4] = self.d.get(o..o.checked_add(4)?)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }
    /// Entries of the IFD at `ifd`; stops quietly at the first entry that falls outside the block.
    fn entries(&self, ifd: usize) -> Vec<Entry> {
        let Some(n) = self.u16(ifd) else {
            return vec![];
        };
        let mut out = Vec::new();
        for i in 0..n as usize {
            let Some(at) = ifd.checked_add(2).and_then(|b| b.checked_add(i * 12)) else {
                break;
            };
            let (Some(tag), Some(ty), Some(count)) =
                (self.u16(at), self.u16(at + 2), self.u32(at + 4))
            else {
                break;
            };
            out.push(Entry { tag, ty, count, at });
        }
        out
    }
    fn find(entries: &[Entry], tag: u16) -> Option<&Entry> {
        entries.iter().find(|e| e.tag == tag)
    }
    /// The raw bytes of an entry's value (inline for 4 bytes or less, else at the offset it holds).
    fn value(&self, e: &Entry) -> Option<&'a [u8]> {
        const SIZES: [usize; 13] = [0, 1, 1, 2, 4, 8, 1, 1, 2, 4, 8, 4, 8];
        let size = SIZES.get(e.ty as usize)?.checked_mul(e.count as usize)?;
        if size <= 4 {
            self.d.get(e.at + 8..e.at + 8 + size)
        } else {
            let off = self.u32(e.at + 8)? as usize;
            self.d.get(off..off.checked_add(size)?)
        }
    }
    fn ascii(&self, e: &Entry) -> Option<String> {
        if e.ty != 2 && e.ty != 7 {
            return None;
        }
        let v = self.value(e)?;
        let end = v.iter().position(|b| *b == 0).unwrap_or(v.len());
        let s = clean_text(&String::from_utf8_lossy(&v[..end]));
        (!s.is_empty()).then_some(s)
    }
    fn rationals(&self, e: &Entry) -> Vec<f64> {
        if e.ty != 5 {
            return vec![];
        }
        let Some(v) = self.value(e) else {
            return vec![];
        };
        v.as_chunks::<8>()
            .0
            .iter()
            .map(|c| {
                let r = |b: &[u8]| {
                    let a: [u8; 4] = b.try_into().unwrap_or([0; 4]);
                    f64::from(if self.le {
                        u32::from_le_bytes(a)
                    } else {
                        u32::from_be_bytes(a)
                    })
                };
                let (n, d) = (r(&c[0..4]), r(&c[4..8]));
                if d == 0.0 {
                    0.0
                } else {
                    n / d
                }
            })
            .collect()
    }
}

fn dms(v: &[f64], reference: Option<String>, negative: &str) -> Option<f64> {
    if v.len() < 3 {
        return None;
    }
    let deg = v[0] + v[1] / 60.0 + v[2] / 3600.0;
    if !deg.is_finite() {
        return None;
    }
    Some(if reference.as_deref() == Some(negative) {
        -deg
    } else {
        deg
    })
}

/// Everything personal in one EXIF/TIFF block.
fn scan_exif(tiff: &[u8], items: &mut Vec<Item>) {
    let Some(x) = Exif::new(tiff) else {
        // an EXIF block we cannot read may still hold anything: say so
        items.push(Item {
            kind: "hidden",
            level: "medium",
            detail: "EXIF".into(),
        });
        return;
    };
    let Some(ifd0) = x.u32(4).map(|v| v as usize) else {
        return;
    };
    let e0 = x.entries(ifd0);
    let get = |entries: &[Entry], tag: u16| Exif::find(entries, tag).and_then(|e| x.ascii(e));

    let exif_ifd = Exif::find(&e0, 0x8769)
        .and_then(|e| x.u32(e.at + 8))
        .map(|o| x.entries(o as usize))
        .unwrap_or_default();
    let gps_ifd = Exif::find(&e0, 0x8825)
        .and_then(|e| x.u32(e.at + 8))
        .map(|o| x.entries(o as usize));

    // GPS
    if let Some(g) = &gps_ifd {
        if !g.is_empty() {
            let rat = |tag| {
                Exif::find(g, tag)
                    .map(|e| x.rationals(e))
                    .unwrap_or_default()
            };
            let lat = dms(&rat(2), get(g, 1), "S");
            let lon = dms(&rat(4), get(g, 3), "W");
            let detail = match (lat, lon) {
                (Some(a), Some(b)) => format!("{a:.4}, {b:.4}"),
                _ => String::new(),
            };
            items.push(Item {
                kind: "gps",
                level: "high",
                detail,
            });
        }
    }
    // serial numbers and unique ids
    let serials: Vec<String> = [
        get(&exif_ifd, 0xA431),
        get(&exif_ifd, 0xA435),
        get(&e0, 0xC62F),
        get(&exif_ifd, 0xA420),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !serials.is_empty() {
        items.push(Item {
            kind: "serial",
            level: "high",
            detail: join_unique(serials),
        });
    }
    // people
    let owners: Vec<String> = [get(&e0, 0x013B), get(&exif_ifd, 0xA430), get(&e0, 0x8298)]
        .into_iter()
        .flatten()
        .collect();
    if !owners.is_empty() {
        items.push(Item {
            kind: "owner",
            level: "high",
            detail: join_unique(owners),
        });
    }
    // device
    let cam: Vec<String> = [get(&e0, 0x010F), get(&e0, 0x0110)]
        .into_iter()
        .flatten()
        .collect();
    if !cam.is_empty() {
        items.push(Item {
            kind: "camera",
            level: "medium",
            detail: cam.join(" "),
        });
    }
    let lens: Vec<String> = [get(&exif_ifd, 0xA433), get(&exif_ifd, 0xA434)]
        .into_iter()
        .flatten()
        .collect();
    if !lens.is_empty() {
        items.push(Item {
            kind: "lens",
            level: "medium",
            detail: lens.join(" "),
        });
    }
    // when
    let stamps: Vec<String> = [
        get(&exif_ifd, 0x9003),
        get(&exif_ifd, 0x9004),
        get(&e0, 0x0132),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !stamps.is_empty() {
        items.push(Item {
            kind: "timestamps",
            level: "medium",
            detail: join_unique(stamps),
        });
    }
    if let Some(s) = get(&e0, 0x0131) {
        items.push(Item {
            kind: "software",
            level: "medium",
            detail: s,
        });
    }
    // free text
    let notes: Vec<String> = [get(&e0, 0x010E), get(&exif_ifd, 0x9286)]
        .into_iter()
        .flatten()
        .collect();
    if !notes.is_empty() {
        items.push(Item {
            kind: "description",
            level: "medium",
            detail: join_unique(notes),
        });
    }
    if Exif::find(&exif_ifd, 0x927C).is_some() {
        items.push(Item {
            kind: "makernote",
            level: "medium",
            detail: String::new(),
        });
    }
    // embedded preview (can outlive an edit and show the uncropped original)
    let n0 = x.u16(ifd0).unwrap_or(0) as usize;
    if let Some(ifd1) = x.u32(ifd0 + 2 + n0 * 12).map(|v| v as usize) {
        if ifd1 != 0 {
            let e1 = x.entries(ifd1);
            if let Some(len) = Exif::find(&e1, 0x0202).and_then(|e| x.u32(e.at + 8)) {
                items.push(Item {
                    kind: "thumbnail",
                    level: "medium",
                    detail: format!("{len} B"),
                });
            }
        }
    }
}

fn scan_xmp(xmp: &[u8], items: &mut Vec<Item>) {
    let text = String::from_utf8_lossy(xmp);
    const PROPS: [&str; 8] = [
        "dc:creator",
        "exif:GPSLatitude",
        "photoshop:Credit",
        "xmp:CreatorTool",
        "xmpMM:DocumentID",
        "xmp:CreateDate",
        "aux:SerialNumber",
        "Iptc4xmpCore:Location",
    ];
    let found: Vec<&str> = PROPS.iter().copied().filter(|p| text.contains(p)).collect();
    let high = found
        .iter()
        .any(|p| matches!(*p, "dc:creator" | "exif:GPSLatitude" | "aux:SerialNumber"));
    items.push(Item {
        kind: "xmp",
        level: if high { "high" } else { "medium" },
        detail: found.join(", "),
    });
}

// ------------------------------------------------------------------ JPEG

const JPEG_KEEP_APP: &[(u8, &[u8])] = &[
    (0xE0, b"JFIF\0"),
    (0xE2, b"ICC_PROFILE\0"),
    (0xEE, b"Adobe"),
];

fn scan_jpeg(data: &[u8]) -> Result<Vec<Item>, ImageError> {
    let bad = || ImageError::Decode("not a readable JPEG".into());
    let (segments, end) = metadata::jpeg_walk(data).ok_or_else(bad)?;
    if !segments.iter().any(|s| s.marker == 0xDA) {
        return Err(bad());
    }
    let mut items = Vec::new();
    // Every segment counts, including those between the scans of a progressive JPEG.
    for seg in segments {
        let (m, body) = (seg.marker, seg.body);
        match m {
            0xE1 if body.starts_with(b"Exif\0\0") => scan_exif(&body[6..], &mut items),
            0xE1 if body.starts_with(b"http://ns.adobe.com/xap/") => scan_xmp(body, &mut items),
            0xED => items.push(Item {
                kind: "iptc",
                level: "medium",
                detail: String::new(),
            }),
            0xFE => items.push(Item {
                kind: "comment",
                level: "medium",
                detail: clean_text(&String::from_utf8_lossy(body)),
            }),
            0xE0 if body.starts_with(b"JFXX\0") => items.push(Item {
                kind: "thumbnail",
                level: "medium",
                detail: format!("{} B", body.len()),
            }),
            0xE2 if body.starts_with(b"MPF\0") => items.push(Item {
                kind: "hidden",
                level: "medium",
                detail: "MPF".into(),
            }),
            0xE0..=0xEF
                if !JPEG_KEEP_APP
                    .iter()
                    .any(|(k, p)| *k == m && body.starts_with(p)) =>
            {
                items.push(Item {
                    kind: "hidden",
                    level: "medium",
                    detail: format!("APP{}", m - 0xE0),
                });
            }
            _ => {}
        }
    }
    if data.len() > end {
        items.push(Item {
            kind: "extra",
            level: "medium",
            detail: format!("{} B", data.len() - end),
        });
    }
    Ok(items)
}

/// Keep only the segments a viewer needs (JFIF, ICC profile, Adobe colour flag and all the
/// coding tables), and cut anything after the end of the image.
fn sanitize_jpeg(data: &[u8]) -> Option<Vec<u8>> {
    let (segs, _) = metadata::jpeg_walk(data)?;
    if !segs.iter().any(|s| s.marker == 0xDA) {
        return None;
    }
    let mut out = vec![0xFF, 0xD8];
    for s in &segs {
        let keep = if (0xE0..=0xEF).contains(&s.marker) || s.marker == 0xFE {
            JPEG_KEEP_APP
                .iter()
                .any(|(k, p)| *k == s.marker && s.body.starts_with(p))
        } else {
            true
        };
        if keep {
            out.extend_from_slice(&data[s.start..s.end]);
        }
    }
    out.extend_from_slice(&[0xFF, 0xD9]);
    Some(out)
}

// ------------------------------------------------------------------ GIF

/// A GIF block: `start..end` is its whole byte range; `kind` says what it is.
struct GifBlock {
    start: usize,
    end: usize,
    kind: GifKind,
}

enum GifKind {
    /// Header, screen descriptor, global colour table.
    Head,
    Image,
    /// Graphic control (timing/transparency) extension.
    Control,
    Comment(String),
    /// Application extension: its 11-byte identifier.
    App(Vec<u8>),
    PlainText,
    OtherExt,
}

fn gif_color_table(flags: u8) -> usize {
    if flags & 0x80 != 0 {
        3 << ((flags & 7) + 1)
    } else {
        0
    }
}

/// Skip data sub-blocks starting at `i`; returns the offset after the terminator, and the
/// concatenated payload (capped, only needed for comments).
fn gif_sub_blocks(data: &[u8], mut i: usize, payload: Option<&mut Vec<u8>>) -> Option<usize> {
    let mut payload = payload;
    loop {
        let n = *data.get(i)? as usize;
        i += 1;
        if n == 0 {
            return Some(i);
        }
        let chunk = data.get(i..i + n)?;
        if let Some(p) = payload.as_deref_mut() {
            if p.len() < 4096 {
                p.extend_from_slice(chunk);
            }
        }
        i += n;
    }
}

/// Walk a GIF; returns its blocks and the offset after the trailer (`None` when damaged).
fn gif_blocks(data: &[u8]) -> Option<(Vec<GifBlock>, usize)> {
    let mut i = 13usize;
    i += gif_color_table(*data.get(10)?);
    data.get(..i)?;
    let mut blocks = vec![GifBlock {
        start: 0,
        end: i,
        kind: GifKind::Head,
    }];
    loop {
        let start = i;
        match *data.get(i)? {
            0x3B => return Some((blocks, i + 1)),
            0x2C => {
                let flags = *data.get(i + 9)?;
                i += 10 + gif_color_table(flags);
                data.get(i)?; // LZW minimum code size
                i = gif_sub_blocks(data, i + 1, None)?;
                blocks.push(GifBlock {
                    start,
                    end: i,
                    kind: GifKind::Image,
                });
            }
            0x21 => {
                let label = *data.get(i + 1)?;
                let mut payload = Vec::new();
                i = gif_sub_blocks(data, i + 2, Some(&mut payload))?;
                let kind = match label {
                    0xF9 => GifKind::Control,
                    0xFE => GifKind::Comment(String::from_utf8_lossy(&payload).into_owned()),
                    0xFF => {
                        // first sub-block is the 11-byte application identifier
                        let id_len = *data.get(start + 2)? as usize;
                        let id = data.get(start + 3..start + 3 + id_len.min(11))?;
                        GifKind::App(id.to_vec())
                    }
                    0x01 => GifKind::PlainText,
                    _ => GifKind::OtherExt,
                };
                blocks.push(GifBlock {
                    start,
                    end: i,
                    kind,
                });
            }
            _ => return None,
        }
    }
}

/// Application extensions a viewer needs (looping).
fn gif_app_is_needed(id: &[u8]) -> bool {
    id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0"
}

fn scan_gif(data: &[u8]) -> Result<Vec<Item>, ImageError> {
    let (blocks, end) =
        gif_blocks(data).ok_or_else(|| ImageError::Decode("not a readable GIF".into()))?;
    let mut items = Vec::new();
    for b in &blocks {
        match &b.kind {
            GifKind::Comment(text) => items.push(Item {
                kind: "comment",
                level: "medium",
                detail: clean_text(text),
            }),
            GifKind::App(id) if id.starts_with(b"XMP DataXMP") => {
                scan_xmp(&data[b.start..b.end], &mut items);
                if !items.iter().any(|i| i.kind == "xmp") {
                    items.push(Item {
                        kind: "xmp",
                        level: "medium",
                        detail: String::new(),
                    });
                }
            }
            GifKind::App(id) if !gif_app_is_needed(id) => items.push(Item {
                kind: "hidden",
                level: "medium",
                detail: "GIF application extension".into(),
            }),
            GifKind::PlainText | GifKind::OtherExt => items.push(Item {
                kind: "hidden",
                level: "medium",
                detail: "GIF extension".into(),
            }),
            _ => {}
        }
    }
    if data.len() > end {
        items.push(Item {
            kind: "extra",
            level: "medium",
            detail: format!("{} B", data.len() - end),
        });
    }
    Ok(items)
}

/// Keep the picture data, timing and looping; drop comments, other extensions and trailing bytes.
fn sanitize_gif(data: &[u8]) -> Option<Vec<u8>> {
    let (blocks, _) = gif_blocks(data)?;
    let mut out = Vec::with_capacity(data.len());
    for b in &blocks {
        let keep = match &b.kind {
            GifKind::Head | GifKind::Image | GifKind::Control => true,
            GifKind::App(id) => gif_app_is_needed(id),
            _ => false,
        };
        if keep {
            out.extend_from_slice(&data[b.start..b.end]);
        }
    }
    out.push(0x3B);
    Some(out)
}

// ------------------------------------------------------------------ PNG

/// Chunks a decoder needs or that describe how to display the pixels. Everything else
/// (text, time, EXIF, C2PA, vendor previews...) is dropped by `sanitize_png`.
const PNG_KEEP: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT",
    b"bKGD", b"pHYs", b"hIST", b"acTL", b"fcTL", b"fdAT", b"cICP", b"mDCV", b"cLLI",
];

fn latin1(b: &[u8]) -> String {
    clean_text(&b.iter().map(|c| *c as char).collect::<String>())
}

fn scan_png(data: &[u8]) -> Result<Vec<Item>, ImageError> {
    let chunks = metadata::png_chunks(data)
        .ok_or_else(|| ImageError::Decode("not a readable PNG".into()))?;
    let mut items = Vec::new();
    let (mut owners, mut texts, mut hidden) = (Vec::new(), Vec::new(), Vec::new());
    for (ty, body) in chunks {
        match &ty {
            b"eXIf" => scan_exif(body, &mut items),
            b"tEXt" | b"zTXt" | b"iTXt" => {
                let nul = body.iter().position(|b| *b == 0).unwrap_or(body.len());
                let key = latin1(&body[..nul]);
                if key == "XML:com.adobe.xmp" {
                    scan_xmp(body, &mut items);
                    continue;
                }
                let value = if &ty == b"tEXt" {
                    body.get(nul + 1..).map(latin1).unwrap_or_default()
                } else {
                    String::new()
                };
                let label = if value.is_empty() {
                    key.clone()
                } else {
                    format!("{key}: {value}")
                };
                match key.as_str() {
                    "Author" | "Artist" | "Copyright" | "Source" => owners.push(label),
                    _ => texts.push(label),
                }
            }
            b"tIME" => {
                if body.len() == 7 {
                    let y = u16::from_be_bytes([body[0], body[1]]);
                    items.push(Item {
                        kind: "timestamps",
                        level: "medium",
                        detail: format!(
                            "{y:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                            body[2], body[3], body[4], body[5], body[6]
                        ),
                    });
                }
            }
            t if PNG_KEEP.contains(&t) => {}
            t => hidden.push(String::from_utf8_lossy(t).into_owned()),
        }
    }
    if !owners.is_empty() {
        items.push(Item {
            kind: "owner",
            level: "high",
            detail: join_unique(owners),
        });
    }
    if !texts.is_empty() {
        items.push(Item {
            kind: "comment",
            level: "medium",
            detail: join_unique(texts),
        });
    }
    if !hidden.is_empty() {
        items.push(Item {
            kind: "hidden",
            level: "medium",
            detail: join_unique(hidden),
        });
    }
    Ok(items)
}

fn sanitize_png(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = data.get(0..8)?.to_vec();
    for (ty, body) in metadata::png_chunks(data)? {
        if !PNG_KEEP.contains(&&ty) {
            continue;
        }
        let mut crc_in = ty.to_vec();
        crc_in.extend_from_slice(body);
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&ty);
        out.extend_from_slice(body);
        out.extend_from_slice(&metadata::crc32(&crc_in).to_be_bytes());
    }
    Some(out)
}

// ------------------------------------------------------------------ WebP

const WEBP_KEEP: &[&[u8; 4]] = &[
    b"VP8X", b"VP8 ", b"VP8L", b"ALPH", b"ANIM", b"ANMF", b"ICCP",
];

fn scan_webp(data: &[u8]) -> Result<Vec<Item>, ImageError> {
    let chunks = metadata::webp_chunks(data)
        .ok_or_else(|| ImageError::Decode("not a readable WebP".into()))?;
    let mut items = Vec::new();
    for (ty, body) in chunks {
        match &ty {
            b"EXIF" => scan_exif(body.strip_prefix(b"Exif\0\0").unwrap_or(body), &mut items),
            b"XMP " => scan_xmp(body, &mut items),
            t if WEBP_KEEP.contains(&t) => {}
            t => items.push(Item {
                kind: "hidden",
                level: "medium",
                detail: String::from_utf8_lossy(t).into_owned(),
            }),
        }
    }
    Ok(items)
}

fn sanitize_webp(data: &[u8]) -> Option<Vec<u8>> {
    let mut body = b"WEBP".to_vec();
    for (ty, chunk) in metadata::webp_chunks(data)? {
        if !WEBP_KEEP.contains(&&ty) {
            continue;
        }
        let mut chunk = chunk.to_vec();
        if &ty == b"VP8X" && chunk.len() >= 10 {
            chunk[0] &= !0x0C; // EXIF and XMP flags: those chunks are gone
        }
        body.extend_from_slice(&ty);
        body.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        body.extend_from_slice(&chunk);
        if chunk.len() & 1 == 1 {
            body.push(0);
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Some(out)
}

// ------------------------------------------------------------------ public scan

fn sniff(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8]) {
        Some("JPEG")
    } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("PNG")
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("WebP")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("GIF")
    } else {
        None
    }
}

fn rank(i: &Item) -> usize {
    const ORDER: [&str; 15] = [
        "gps",
        "serial",
        "owner",
        "camera",
        "lens",
        "timestamps",
        "software",
        "thumbnail",
        "makernote",
        "description",
        "xmp",
        "iptc",
        "comment",
        "hidden",
        "extra",
    ];
    ORDER
        .iter()
        .position(|k| *k == i.kind)
        .unwrap_or(ORDER.len())
}

/// What personal data `bytes` (a JPEG, PNG or WebP file) carries. Reads only.
pub fn scan(bytes: &[u8]) -> Result<Report, ImageError> {
    let format = sniff(bytes).ok_or_else(|| ImageError::Unsupported("format".into()))?;
    let items = match format {
        "JPEG" => scan_jpeg(bytes)?,
        "PNG" => scan_png(bytes)?,
        "GIF" => scan_gif(bytes)?,
        _ => scan_webp(bytes)?,
    };
    // One entry per kind (EXIF and PNG text can both name an owner, for instance).
    let mut merged: Vec<Item> = Vec::new();
    for item in items {
        match merged.iter_mut().find(|m| m.kind == item.kind) {
            Some(m) => {
                if item.level == "high" {
                    m.level = "high";
                }
                m.detail = join_unique(vec![std::mem::take(&mut m.detail), item.detail]);
            }
            None => merged.push(item),
        }
    }
    merged.sort_by_key(rank);
    Ok(Report {
        format,
        items: merged,
    })
}

fn read_image_file(path: &Path) -> Result<Vec<u8>, TaskError> {
    let file = fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(TaskError::NotAFile);
    }
    if meta.len() > im::MAX_INPUT_BYTES {
        return Err(TaskError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(im::MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The `privacy_report` data event for one file, or the error that stops it.
pub fn report_event(path: &Path) -> Result<Event, TaskError> {
    let report = scan(&read_image_file(path)?)?;
    Ok(Event::Data {
        name: "privacy_report".into(),
        data: report.to_json(path),
    })
}

// ------------------------------------------------------------------ make safe

pub struct Safe {
    pub data: Vec<u8>,
    pub format: Format,
    /// Personal items the source carried (what was removed).
    pub removed: usize,
    /// (from, to) pixel sizes when the long edge was reduced.
    pub resized: Option<((u32, u32), (u32, u32))>,
}

fn sanitize(data: Vec<u8>, format: Format) -> Result<Vec<u8>, ImageError> {
    let cleaned = match format {
        Format::Jpeg => sanitize_jpeg(&data),
        Format::Png => sanitize_png(&data),
        Format::WebP => sanitize_webp(&data),
        Format::Gif => sanitize_gif(&data),
        _ => Some(data),
    };
    cleaned.ok_or_else(|| ImageError::Encode("could not clean the container".into()))
}

/// A copy of the image without personal metadata, optionally reduced to `max_edge` px on the
/// long edge. JPEG/PNG/WebP are edited at the container level (pixels untouched) unless the
/// orientation has to be baked in or the size reduced; the result is re-checked and refused if
/// anything personal remains.
pub fn make_safe(
    bytes: &[u8],
    ext_hint: Option<&str>,
    max_edge: Option<u32>,
    quality: u8,
) -> Result<Safe, ImageError> {
    let removed = scan(bytes).map(|r| r.items.len()).unwrap_or(0);
    let opts = StripOptions {
        exif: true,
        gps: true,
        comments: true,
        // The colour profile is not personal and removing it shifts colours.
        color_profile: false,
    };
    let (stripped, format) = im::strip(bytes, ext_hint, opts, true, quality)?;
    let data = match stripped {
        im::Stripped::Lossless(d) | im::Stripped::Reencoded(d) => d,
    };
    let mut data = sanitize(data, format)?;
    let mut resized = None;
    if let Some(edge) = max_edge.filter(|e| *e > 0) {
        let loaded = im::load_bytes(&data, ext_hint)?;
        let (w, h) = loaded.still().dimensions();
        if w.max(h) > edge {
            let to = ResizeTo::Pixels {
                width: edge,
                height: edge,
                keep_aspect: true,
            };
            let frames = loaded
                .frames
                .iter()
                .map(|f| {
                    Ok(im::Frm {
                        img: im::resize(&f.img, to)?,
                        delay_ms: f.delay_ms,
                    })
                })
                .collect::<Result<Vec<_>, ImageError>>()?;
            let to_size = frames
                .iter()
                .map(|f| f.img.dimensions())
                .max_by_key(|d| u64::from(d.0) * u64::from(d.1))
                .unwrap_or((w, h));
            data = sanitize(im::encode_frames(&frames, format, quality)?, format)?;
            resized = Some(((w, h), to_size));
        }
    }
    // Final check on what would be written.
    if sniff(&data).is_some() {
        let left = scan(&data)?.items;
        if !left.is_empty() {
            return Err(ImageError::Encode(format!(
                "personal data remained: {}",
                left.iter().map(|i| i.kind).collect::<Vec<_>>().join(", ")
            )));
        }
    }
    Ok(Safe {
        data,
        format,
        removed,
        resized,
    })
}

/// Run one of this module's tasks on `input`.
pub fn run_file(
    name: &str,
    params: &HashMap<String, String>,
    input: &Path,
    settings: &Settings,
) -> Result<Done, TaskError> {
    let file_name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext_hint = input.extension().and_then(|e| e.to_str());
    let bytes = read_image_file(input)?;
    match name {
        REPORT_TASK => {
            let report = scan(&bytes)?;
            let info = if report.is_clean() {
                ("msg.privacyClean", vec![file_name])
            } else {
                (
                    "msg.privacyFound",
                    vec![file_name, report.items.len().to_string()],
                )
            };
            Ok(Done {
                output: input.to_path_buf(), // nothing is written
                warnings: vec![],
                infos: vec![info],
            })
        }
        SAFE_TASK => {
            let max_edge = params
                .get("maxEdge")
                .and_then(|v| v.parse::<u32>().ok())
                .map(|v| v.min(MAX_EDGE_LIMIT));
            let quality = match ext_hint.and_then(Format::from_name) {
                Some(Format::WebP) => settings.image.webp_quality.clamp(1, 100) as u8,
                _ => 92,
            };
            let safe = make_safe(&bytes, ext_hint, max_edge, quality)?;
            let output = write_result(
                input,
                &settings.output.image,
                "_safe",
                Some(safe.format.extension()),
                &safe.data,
            )?;
            let mut infos = vec![if safe.removed == 0 {
                ("msg.safeNothing", vec![file_name.clone()])
            } else {
                (
                    "msg.safeRemoved",
                    vec![file_name.clone(), safe.removed.to_string()],
                )
            }];
            if let Some((from, to)) = safe.resized {
                infos.push((
                    "msg.safeResized",
                    vec![
                        file_name,
                        format!("{}x{}", from.0, from.1),
                        format!("{}x{}", to.0, to.1),
                    ],
                ));
            }
            Ok(Done {
                output,
                warnings: vec![],
                infos,
            })
        }
        _ => Err(TaskError::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{Rgba, RgbaImage};

    // ---- a tiny little-endian TIFF/EXIF writer for building realistic test photos

    type E = (u16, u16, u32, Vec<u8>); // tag, type, count, data
    fn asc(tag: u16, s: &str) -> E {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        (tag, 2, v.len() as u32, v)
    }
    fn rats(tag: u16, v: &[(u32, u32)]) -> E {
        let d: Vec<u8> = v
            .iter()
            .flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat())
            .collect();
        (tag, 5, v.len() as u32, d)
    }
    fn long(tag: u16, v: u32) -> E {
        (tag, 4, 1, v.to_le_bytes().to_vec())
    }
    fn short(tag: u16, v: u16) -> E {
        (tag, 3, 1, v.to_le_bytes().to_vec())
    }
    fn put_ifd(buf: &mut Vec<u8>, mut entries: Vec<E>, next: u32) -> u32 {
        entries.sort_by_key(|e| e.0);
        let mut slots = Vec::new();
        for (_, _, _, data) in &entries {
            if data.len() > 4 {
                if buf.len() % 2 == 1 {
                    buf.push(0);
                }
                slots.push((true, buf.len() as u32));
                buf.extend_from_slice(data);
            } else {
                slots.push((false, 0));
            }
        }
        if buf.len() % 2 == 1 {
            buf.push(0);
        }
        let at = buf.len() as u32;
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for ((tag, ty, count, data), (out_of_line, off)) in entries.iter().zip(slots) {
            buf.extend_from_slice(&tag.to_le_bytes());
            buf.extend_from_slice(&ty.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());
            if out_of_line {
                buf.extend_from_slice(&off.to_le_bytes());
            } else {
                let mut v = data.clone();
                v.resize(4, 0);
                buf.extend_from_slice(&v);
            }
        }
        buf.extend_from_slice(&next.to_le_bytes());
        at
    }

    /// A phone-like EXIF block: GPS (35°39'31"N 139°44'43"E), serials, owner, camera, lens,
    /// timestamps, software and a thumbnail.
    fn exif_block(orientation: u16) -> Vec<u8> {
        let mut t = b"II*\0\0\0\0\0".to_vec();
        let thumb_at = t.len() as u32;
        t.extend_from_slice(&[0x5A; 40]); // thumbnail bytes
        let gps = put_ifd(
            &mut t,
            vec![
                asc(1, "N"),
                rats(2, &[(35, 1), (39, 1), (31, 1)]),
                asc(3, "E"),
                rats(4, &[(139, 1), (44, 1), (43, 1)]),
            ],
            0,
        );
        let exif = put_ifd(
            &mut t,
            vec![
                asc(0x9003, "2024:05:01 10:20:30"),
                asc(0xA431, "BODY-SERIAL-123"),
                asc(0xA434, "RF 24-70mm"),
                asc(0xA435, "LENS-SERIAL-9"),
                asc(0xA430, "Taro Yamada"),
                (0x927C, 7, 8, vec![1; 8]),
            ],
            0,
        );
        let ifd1 = put_ifd(&mut t, vec![long(0x0201, thumb_at), long(0x0202, 40)], 0);
        let ifd0 = put_ifd(
            &mut t,
            vec![
                asc(0x010F, "Canon"),
                asc(0x0110, "EOS R6"),
                short(0x0112, orientation),
                asc(0x0131, "Photoshop 25"),
                asc(0x013B, "Hanako"),
                asc(0x0132, "2024:05:02 11:00:00"),
                long(0x8769, exif),
                long(0x8825, gps),
            ],
            ifd1,
        );
        t[4..8].copy_from_slice(&ifd0.to_le_bytes());
        t
    }

    fn photo(w: u32, h: u32) -> RgbaImage {
        let mut s = 7u32;
        RgbaImage::from_fn(w, h, |x, y| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            Rgba([
                (x * 255 / w) as u8 ^ (s >> 28) as u8,
                (y * 255 / h) as u8,
                (s >> 20) as u8,
                255,
            ])
        })
    }

    fn app(m: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, m];
        v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    /// A real, decodable JPEG with extra blocks inserted after SOI and junk after EOI.
    fn dirty_jpeg(orientation: u16) -> Vec<u8> {
        let clean = im::encode(&photo(64, 32), Format::Jpeg, 90).unwrap();
        let mut j = vec![0xFF, 0xD8];
        j.extend(app(
            0xE1,
            &[b"Exif\0\0".to_vec(), exif_block(orientation)].concat(),
        ));
        j.extend(app(
            0xE1,
            b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta dc:creator exif:GPSLatitude/>",
        ));
        j.extend(app(0xED, b"Photoshop 3.0\0iptc"));
        j.extend(app(0xFE, b"shot at home"));
        j.extend(app(0xE2, b"MPF\0abcdef"));
        j.extend(app(0xEC, b"Ducky\0\0\0"));
        j.extend_from_slice(&clean[2..]);
        j.extend_from_slice(b"TRAILING-SECRET-VIDEO");
        j
    }

    fn kinds(r: &Report) -> Vec<&'static str> {
        r.items.iter().map(|i| i.kind).collect()
    }

    #[test]
    fn jpeg_report_lists_every_kind_of_personal_data() {
        let r = scan(&dirty_jpeg(1)).unwrap();
        assert_eq!(r.format, "JPEG");
        let by = |k: &str| {
            r.items
                .iter()
                .find(|i| i.kind == k)
                .unwrap_or_else(|| panic!("{k}"))
        };
        assert_eq!(by("gps").detail, "35.6586, 139.7453");
        assert_eq!(by("gps").level, "high");
        assert!(
            by("serial").detail.contains("BODY-SERIAL-123")
                && by("serial").detail.contains("LENS-SERIAL-9")
        );
        assert!(
            by("owner").detail.contains("Taro Yamada") && by("owner").detail.contains("Hanako")
        );
        assert_eq!(by("camera").detail, "Canon EOS R6");
        assert_eq!(by("lens").detail, "RF 24-70mm");
        assert!(by("timestamps").detail.contains("2024:05:01 10:20:30"));
        assert_eq!(by("software").detail, "Photoshop 25");
        assert_eq!(by("thumbnail").detail, "40 B");
        assert!(by("xmp").detail.contains("dc:creator") && by("xmp").level == "high");
        for k in ["makernote", "iptc", "comment", "hidden", "extra"] {
            by(k);
        }
        assert_eq!(by("extra").detail, "21 B");
        // gps comes first, then serial: most sensitive on top
        assert_eq!(&kinds(&r)[..2], ["gps", "serial"]);
    }

    #[test]
    fn a_plain_photo_is_clean_and_json_is_well_formed() {
        let plain = im::encode(&photo(16, 16), Format::Jpeg, 90).unwrap();
        let r = scan(&plain).unwrap();
        assert!(r.is_clean());
        let v = scan(&dirty_jpeg(1))
            .unwrap()
            .to_json(Path::new("/tmp/a.jpg"));
        assert_eq!(v["file"], "a.jpg");
        assert_eq!(v["format"], "JPEG");
        assert!(v["personal"].as_u64().unwrap() >= 10);
        assert_eq!(v["items"][0]["kind"], "gps");
    }

    #[test]
    fn make_safe_jpeg_is_lossless_when_orientation_is_normal() {
        let src = dirty_jpeg(1);
        let clean_original = im::encode(&photo(64, 32), Format::Jpeg, 90).unwrap();
        let safe = make_safe(&src, Some("jpg"), None, 92).unwrap();
        assert!(scan(&safe.data).unwrap().is_clean());
        assert!(safe.removed >= 10 && safe.resized.is_none());
        // pixel data untouched: identical to the never-dirtied encoding
        assert_eq!(safe.data, clean_original);
        let has = |needle: &[u8]| safe.data.windows(needle.len()).any(|w| w == needle);
        for n in [
            &b"Canon"[..],
            b"BODY-SERIAL",
            b"Taro",
            b"TRAILING",
            b"Ducky",
            b"shot at home",
        ] {
            assert!(!has(n), "{}", String::from_utf8_lossy(n));
        }
        assert_eq!(
            im::load_bytes(&safe.data, Some("jpg"))
                .unwrap()
                .still()
                .dimensions(),
            (64, 32)
        );
    }

    #[test]
    fn make_safe_bakes_orientation_then_leaves_nothing() {
        let safe = make_safe(&dirty_jpeg(6), Some("jpg"), None, 92).unwrap();
        assert!(scan(&safe.data).unwrap().is_clean());
        // orientation 6 swaps the axes and, with the EXIF gone, the pixels now show it
        let (w, h) = im::load_bytes(&safe.data, Some("jpg"))
            .unwrap()
            .still()
            .dimensions();
        assert_eq!((w, h), (32, 64));
    }

    #[test]
    fn make_safe_resizes_only_when_asked_and_only_down() {
        let src = dirty_jpeg(1);
        let big = make_safe(&src, Some("jpg"), Some(1000), 92).unwrap();
        assert!(big.resized.is_none(), "already smaller than the limit");
        let small = make_safe(&src, Some("jpg"), Some(32), 92).unwrap();
        assert_eq!(small.resized, Some(((64, 32), (32, 16))));
        assert!(scan(&small.data).unwrap().is_clean());
        assert_eq!(
            im::load_bytes(&small.data, Some("jpg"))
                .unwrap()
                .still()
                .dimensions(),
            (32, 16)
        );
        let off = make_safe(&src, Some("jpg"), Some(0), 92).unwrap();
        assert!(off.resized.is_none());
    }

    fn png_chunk(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut c = (body.len() as u32).to_be_bytes().to_vec();
        c.extend_from_slice(ty);
        c.extend_from_slice(body);
        c.extend_from_slice(&metadata::crc32(&[&ty[..], body].concat()).to_be_bytes());
        c
    }

    fn dirty_png() -> Vec<u8> {
        let clean = im::encode(&photo(24, 12), Format::Png, 0).unwrap();
        let chunks = metadata::png_chunks(&clean).unwrap();
        let mut out = clean[..8].to_vec();
        for (ty, body) in chunks {
            if &ty == b"IDAT" {
                out.extend(png_chunk(b"eXIf", &exif_block(1)));
                out.extend(png_chunk(b"tEXt", b"Author\0Taro"));
                out.extend(png_chunk(b"tEXt", b"Comment\0hello"));
                out.extend(png_chunk(b"tIME", &[0x07, 0xE8, 5, 1, 10, 20, 30]));
                out.extend(png_chunk(b"caBX", b"c2pa-manifest"));
            }
            out.extend(png_chunk(&ty, body));
        }
        out
    }

    #[test]
    fn png_report_and_make_safe() {
        let src = dirty_png();
        let r = scan(&src).unwrap();
        let by = |k: &str| {
            r.items
                .iter()
                .find(|i| i.kind == k)
                .unwrap_or_else(|| panic!("{k}"))
        };
        assert_eq!(by("gps").detail, "35.6586, 139.7453");
        assert!(by("owner").detail.contains("Author: Taro"));
        assert!(by("comment").detail.contains("Comment: hello"));
        assert_eq!(by("hidden").detail, "caBX");
        assert!(r
            .items
            .iter()
            .any(|i| i.kind == "timestamps" && i.detail.contains("2024-05-01 10:20:30")));

        let safe = make_safe(&src, Some("png"), None, 92).unwrap();
        assert!(scan(&safe.data).unwrap().is_clean());
        // every chunk CRC is still valid and the picture is the same
        let before = im::load_bytes(&src, Some("png")).unwrap();
        let after = im::load_bytes(&safe.data, Some("png")).unwrap();
        assert_eq!(before.still().as_raw(), after.still().as_raw());
        assert!(!safe.data.windows(4).any(|w| w == b"caBX"));
    }

    #[test]
    fn webp_report_and_make_safe() {
        let mk = |ty: &[u8; 4], body: &[u8]| {
            let mut c = ty.to_vec();
            c.extend_from_slice(&(body.len() as u32).to_le_bytes());
            c.extend_from_slice(body);
            if body.len() & 1 == 1 {
                c.push(0);
            }
            c
        };
        let clean = im::encode(&photo(16, 16), Format::WebP, 80).unwrap();
        // lossy/lossless simple file: one VP8/VP8L chunk -> wrap in VP8X with EXIF + XMP
        let image_chunks = metadata::webp_chunks(&clean).unwrap();
        let (ty, body) = &image_chunks[0];
        let vp8x = vec![0x0C, 0, 0, 0, 15, 0, 0, 15, 0, 0]; // EXIF + XMP flags, 16x16
        let mut body_all = b"WEBP".to_vec();
        for c in [
            mk(b"VP8X", &vp8x),
            mk(ty, body),
            mk(b"EXIF", &[b"Exif\0\0".to_vec(), exif_block(1)].concat()),
            mk(b"XMP ", b"<x dc:creator/>"),
        ] {
            body_all.extend_from_slice(&c);
        }
        let mut src = b"RIFF".to_vec();
        src.extend_from_slice(&(body_all.len() as u32).to_le_bytes());
        src.extend_from_slice(&body_all);
        let r = scan(&src).unwrap();
        assert!(kinds(&r).contains(&"gps") && kinds(&r).contains(&"xmp"));
        let safe = make_safe(&src, Some("webp"), None, 80).unwrap();
        assert!(scan(&safe.data).unwrap().is_clean());
        assert_eq!(
            u32::from_le_bytes(safe.data[4..8].try_into().unwrap()) as usize,
            safe.data.len() - 8
        );
    }

    #[test]
    fn jpeg_end_is_not_fooled_by_ff_d9_inside_a_table() {
        // SOI, DHT whose payload contains FF D9, SOS + data, EOI, then trailing bytes
        let mut j = vec![0xFF, 0xD8];
        j.extend(app(0xC4, &[0x00, 0xFF, 0xD9, 0x01]));
        j.extend(app(0xDA, &[1, 2, 3]));
        j.extend_from_slice(&[0x11, 0xFF, 0x00, 0x22, 0xFF, 0xD0, 0x33]);
        j.extend(app(0xC4, &[0xFF, 0xD9])); // a table between two scans
        j.extend(app(0xDA, &[4, 5]));
        j.extend_from_slice(&[0x44, 0xFF, 0xD9]);
        let (segs, end) = metadata::jpeg_walk(&j).unwrap();
        assert_eq!(end, j.len());
        assert_eq!(segs.iter().filter(|s| s.marker == 0xDA).count(), 2);
        j.extend_from_slice(b"junk");
        assert_eq!(metadata::jpeg_walk(&j).unwrap().1, j.len() - 4);
        assert_eq!(sanitize_jpeg(&j).unwrap().len(), j.len() - 4);
    }

    #[test]
    fn metadata_between_progressive_scans_is_found_and_removed() {
        let mut j = vec![0xFF, 0xD8];
        j.extend(app(0xE0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0"));
        j.extend(app(0xDA, &[1, 2, 3]));
        j.extend_from_slice(&[0x11, 0xFF, 0x00, 0x22]);
        j.extend(app(0xFE, b"hidden after first scan"));
        j.extend(app(0xE1, &[b"Exif\0\0".to_vec(), exif_block(1)].concat()));
        j.extend(app(0xDA, &[4, 5]));
        j.extend_from_slice(&[0x44, 0xFF, 0xD9]);
        let k = kinds(&scan(&j).unwrap());
        assert!(k.contains(&"comment") && k.contains(&"gps"), "{k:?}");
        let clean = sanitize_jpeg(&j).unwrap();
        assert!(!clean.windows(6).any(|w| w == b"hidden"));
        assert!(!clean.windows(4).any(|w| w == b"Exif"));
        assert!(scan(&clean).unwrap().is_clean());
        // both scans are still there
        assert_eq!(clean.windows(2).filter(|w| *w == [0xFF, 0xDA]).count(), 2);
    }

    #[test]
    fn hostile_and_truncated_input_never_panics() {
        for src in [dirty_jpeg(6), dirty_png()] {
            for len in (0..src.len()).step_by(7) {
                let part = &src[..len];
                let _ = scan(part);
                let _ = sanitize_jpeg(part);
                let _ = sanitize_png(part);
                let _ = make_safe(part, Some("jpg"), Some(10), 90);
            }
        }
        // every prefix of the EXIF block alone
        let ex = exif_block(1);
        for len in 0..ex.len() {
            let mut items = vec![];
            scan_exif(&ex[..len], &mut items);
        }
        // offsets pointing far outside the block
        let mut evil = exif_block(1);
        for o in (8..evil.len() - 4).step_by(2) {
            let saved: [u8; 4] = evil[o..o + 4].try_into().unwrap();
            evil[o..o + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let mut items = vec![];
            scan_exif(&evil, &mut items);
            evil[o..o + 4].copy_from_slice(&saved);
        }
        assert!(matches!(
            scan(b"BM........."),
            Err(ImageError::Unsupported(_))
        ));
        assert!(scan(b"GIF89a.....").is_err());
        for len in 0..gif_with_metadata().len() {
            let _ = scan(&gif_with_metadata()[..len]);
            let _ = sanitize_gif(&gif_with_metadata()[..len]);
        }
    }

    /// A 1x1 GIF with a comment, an XMP application extension, a looping extension and trailing bytes.
    fn gif_with_metadata() -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
        g.extend_from_slice(&[0x21, 0xFF, 11]);
        g.extend_from_slice(b"NETSCAPE2.0");
        g.extend_from_slice(&[3, 1, 0, 0, 0]);
        g.extend_from_slice(&[0x21, 0xFE, 12]);
        g.extend_from_slice(b"secret GIF c");
        g.extend_from_slice(&[0]);
        g.extend_from_slice(&[0x21, 0xFF, 11]);
        g.extend_from_slice(b"XMP DataXMP");
        g.extend_from_slice(&[5]);
        g.extend_from_slice(b"<x:a>");
        g.extend_from_slice(&[0]);
        g.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x44, 0x01, 0]);
        g.push(0x3B);
        g.extend_from_slice(b"TRAIL");
        g
    }

    #[test]
    fn gif_comments_and_extensions_are_found_and_removed() {
        let g = gif_with_metadata();
        let k = kinds(&scan(&g).unwrap());
        assert!(
            k.contains(&"comment") && k.contains(&"xmp") && k.contains(&"extra"),
            "{k:?}"
        );
        let clean = sanitize_gif(&g).unwrap();
        assert!(scan(&clean).unwrap().is_clean());
        assert!(!clean.windows(6).any(|w| w == b"secret"));
        assert!(clean.windows(11).any(|w| w == b"NETSCAPE2.0"));
        assert!(clean.ends_with(&[0x3B]));
        // make_safe on a real GIF also leaves a verified-clean file
        let real = im::encode(&photo(8, 8), Format::Gif, 0).unwrap();
        let safe = make_safe(&real, Some("gif"), None, 90).unwrap();
        assert!(scan(&safe.data).unwrap().is_clean());
    }

    fn settings() -> Settings {
        Settings::default()
    }

    #[test]
    fn task_writes_a_safe_copy_beside_the_source_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("旅行.jpg");
        let original = dirty_jpeg(1);
        fs::write(&src, &original).unwrap();
        let none = HashMap::new();
        let a = run_file(SAFE_TASK, &none, &src, &settings()).unwrap();
        let b = run_file(SAFE_TASK, &none, &src, &settings()).unwrap();
        assert_eq!(a.output, d.path().join("旅行_safe.jpg"));
        assert_eq!(b.output, d.path().join("旅行_safe (2).jpg"));
        assert_eq!(a.infos[0].0, "msg.safeRemoved");
        assert!(scan(&fs::read(&a.output).unwrap()).unwrap().is_clean());
        assert_eq!(fs::read(&src).unwrap(), original, "source untouched");
        assert_eq!(
            fs::read_dir(d.path()).unwrap().count(),
            3,
            "no scratch leftovers"
        );
    }

    #[test]
    fn task_honours_max_edge_and_the_image_output_folder() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("p.jpg");
        fs::write(&src, dirty_jpeg(1)).unwrap();
        let mut s = settings();
        s.output.image = "./Safe".into();
        let params = crate::tasks::parse_task("image.makeSafe?maxEdge=32").1;
        let r = run_file(SAFE_TASK, &params, &src, &s).unwrap();
        assert_eq!(r.output, d.path().join("Safe/p_safe.jpg"));
        assert_eq!(r.infos[1].0, "msg.safeResized");
        assert_eq!(r.infos[1].1[1..], ["64x32", "32x16"]);
        let (w, h) = im::load(&r.output).unwrap().still().dimensions();
        assert_eq!(w.max(h), 32);
    }

    #[test]
    fn report_task_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("p.jpg");
        fs::write(&src, dirty_jpeg(1)).unwrap();
        let r = run_file(REPORT_TASK, &HashMap::new(), &src, &settings()).unwrap();
        assert_eq!(r.output, src);
        assert_eq!(r.infos[0].0, "msg.privacyFound");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
        let ev = serde_json::to_value(report_event(&src).unwrap()).unwrap();
        assert_eq!(ev["event"], "data");
        assert_eq!(ev["name"], "privacy_report");
        assert_eq!(ev["data"]["items"][0]["kind"], "gps");
        // a clean file says so
        let plain = d.path().join("c.jpg");
        fs::write(&plain, im::encode(&photo(8, 8), Format::Jpeg, 90).unwrap()).unwrap();
        let c = run_file(REPORT_TASK, &HashMap::new(), &plain, &settings()).unwrap();
        assert_eq!(c.infos[0].0, "msg.privacyClean");
    }

    #[test]
    fn broken_files_leave_nothing_behind() {
        let d = tempfile::tempdir().unwrap();
        let bad = d.path().join("bad.jpg");
        fs::write(&bad, b"\xFF\xD8 not a jpeg").unwrap();
        for t in [REPORT_TASK, SAFE_TASK] {
            assert!(
                run_file(t, &HashMap::new(), &bad, &settings()).is_err(),
                "{t}"
            );
        }
        assert!(matches!(
            run_file(SAFE_TASK, &HashMap::new(), d.path(), &settings()),
            Err(TaskError::NotAFile | TaskError::Io(_))
        ));
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
