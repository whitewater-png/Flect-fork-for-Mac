//! Lossless metadata removal at the container level (JPEG segments, PNG chunks,
//! WebP RIFF chunks) plus EXIF/TIFF helpers. Nothing is re-encoded, so image data
//! is untouched. GPS removal really erases the bytes (not just the pointer).
//! All parsing is bounds-checked; malformed input yields `None`, never a panic.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StripOptions {
    pub exif: bool,
    pub gps: bool,
    pub comments: bool,
    pub color_profile: bool,
}

// ------------------------------------------------------------------ TIFF / EXIF

struct Tiff<'a> {
    d: &'a mut [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(d: &'a mut [u8]) -> Option<Self> {
        let le = match d.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let t = Self { d, le };
        (t.u16(2)? == 42).then_some(t)
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
    fn put_u32(&mut self, o: usize, v: u32) -> Option<()> {
        let b = if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        self.d.get_mut(o..o.checked_add(4)?)?.copy_from_slice(&b);
        Some(())
    }
    fn zero(&mut self, o: usize, len: usize) -> Option<()> {
        self.d.get_mut(o..o.checked_add(len)?)?.fill(0);
        Some(())
    }
    /// (offset of entry, tag) for each entry of the IFD at `ifd`.
    fn entries(&self, ifd: usize) -> Option<Vec<(usize, u16)>> {
        let n = self.u16(ifd)? as usize;
        (0..n)
            .map(|i| {
                let e = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
                Some((e, self.u16(e)?))
            })
            .collect()
    }
    fn value_size(&self, entry: usize) -> Option<usize> {
        const SIZES: [usize; 13] = [0, 1, 1, 2, 4, 8, 1, 1, 2, 4, 8, 4, 8];
        let ty = self.u16(entry + 2)? as usize;
        let count = self.u32(entry + 4)? as usize;
        SIZES.get(ty)?.checked_mul(count)
    }
    /// Zero the out-of-line value of an entry (values of 4 bytes or less live inside the entry).
    fn erase_value(&mut self, entry: usize) -> Option<()> {
        let size = self.value_size(entry)?;
        if size > 4 {
            let off = self.u32(entry + 8)? as usize;
            self.zero(off, size)?;
        }
        Some(())
    }
    /// Remove entry `idx` of the IFD at `ifd`, closing the gap.
    fn remove_entry(&mut self, ifd: usize, idx: usize) -> Option<()> {
        let n = self.u16(ifd)? as usize;
        let start = ifd + 2 + idx * 12;
        let end = ifd + 2 + n * 12 + 4; // includes the next-IFD pointer
        if idx >= n || end > self.d.len() {
            return None;
        }
        self.d.copy_within(start + 12..end, start);
        self.zero(end - 12, 12)?;
        let nn = (n - 1) as u16;
        let b = if self.le {
            nn.to_le_bytes()
        } else {
            nn.to_be_bytes()
        };
        self.d[ifd..ifd + 2].copy_from_slice(&b);
        Some(())
    }
}

/// EXIF orientation (1-8) from a TIFF block.
pub fn orientation(tiff: &[u8]) -> Option<u16> {
    let mut copy = tiff.to_vec();
    let t = Tiff::new(&mut copy)?;
    let ifd0 = t.u32(4)? as usize;
    for (e, tag) in t.entries(ifd0)? {
        if tag == 0x0112 {
            return t.u16(e + 8);
        }
    }
    None
}

/// Erase GPS data (and the embedded thumbnail, which can carry its own copy).
/// Returns `None` when the block cannot be parsed safely.
pub fn strip_gps(tiff: &mut [u8]) -> Option<()> {
    let mut t = Tiff::new(tiff)?;
    let ifd0 = t.u32(4)? as usize;
    // 1. GPS IFD
    let entries = t.entries(ifd0)?;
    if let Some(pos) = entries.iter().position(|(_, tag)| *tag == 0x8825) {
        let gps = t.u32(entries[pos].0 + 8)? as usize;
        for (e, _) in t.entries(gps)? {
            t.erase_value(e)?;
        }
        let n = t.u16(gps)? as usize;
        t.zero(gps, 2 + n * 12 + 4)?;
        t.remove_entry(ifd0, pos)?;
    }
    // 2. thumbnail IFD1
    let n0 = t.u16(ifd0)? as usize;
    let next_ptr = ifd0 + 2 + n0 * 12;
    let ifd1 = t.u32(next_ptr)? as usize;
    if ifd1 != 0 {
        let e1 = t.entries(ifd1)?;
        let find = |tag| e1.iter().find(|(_, g)| *g == tag).map(|(e, _)| *e);
        if let (Some(off_e), Some(len_e)) = (find(0x0201), find(0x0202)) {
            let (off, len) = (t.u32(off_e + 8)? as usize, t.u32(len_e + 8)? as usize);
            t.zero(off, len)?;
        }
        let n1 = t.u16(ifd1)? as usize;
        for (e, _) in &e1 {
            t.erase_value(*e)?;
        }
        t.zero(ifd1, 2 + n1 * 12 + 4)?;
        t.put_u32(next_ptr, 0)?;
    }
    Some(())
}

// ------------------------------------------------------------------ JPEG

pub fn orientation_jpeg(data: &[u8]) -> Option<u16> {
    jpeg_segments(data)?.into_iter().find_map(|(m, body)| {
        (m == 0xE1 && body.starts_with(b"Exif\0\0"))
            .then(|| orientation(&body[6..]))
            .flatten()
    })
}

/// (marker, body) pairs up to the start of scan.
fn jpeg_segments(data: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    if data.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let (mut i, mut out) = (2, Vec::new());
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            return None;
        }
        let m = data[i + 1];
        if m == 0xDA {
            break;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let body = data.get(i + 4..i + 2 + len)?;
        out.push((m, body));
        i += 2 + len;
    }
    Some(out)
}

pub fn strip_jpeg(data: &[u8], o: StripOptions) -> Option<Vec<u8>> {
    if data.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut out = vec![0xFF, 0xD8];
    let mut i = 2;
    while i + 2 <= data.len() {
        if data[i] != 0xFF {
            return None;
        }
        let m = data[i + 1];
        if m == 0xDA || m == 0xD9 {
            out.extend_from_slice(&data[i..]); // scan data and EOI, verbatim
            return Some(out);
        }
        if (0xD0..=0xD7).contains(&m) || m == 0x01 || m == 0xFF {
            out.extend_from_slice(&data[i..i + 2]);
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]) as usize;
        let seg = data.get(i..i + 2 + len)?;
        let body = &seg[4..];
        let keep = match m {
            0xE1 if body.starts_with(b"Exif\0\0") => {
                if o.exif {
                    false
                } else if o.gps {
                    let mut tiff = body[6..].to_vec();
                    match strip_gps(&mut tiff) {
                        Some(()) => {
                            out.extend_from_slice(&seg[..4]);
                            out.extend_from_slice(b"Exif\0\0");
                            out.extend_from_slice(&tiff);
                            i += 2 + len;
                            continue;
                        }
                        None => false, // cannot prove it is clean: drop the block
                    }
                } else {
                    true
                }
            }
            0xE1 if body.starts_with(b"http://ns.adobe.com/xap/") => !o.comments,
            0xED => !o.comments, // Photoshop / IPTC
            0xFE => !o.comments, // comment
            0xE2 if body.starts_with(b"ICC_PROFILE\0") => !o.color_profile,
            _ => true,
        };
        if keep {
            out.extend_from_slice(seg);
        }
        i += 2 + len;
    }
    Some(out)
}

// ------------------------------------------------------------------ PNG

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB88320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    !data.iter().fold(!0u32, |c, b| {
        table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8)
    })
}

fn png_chunks(data: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    if data.get(0..8)? != PNG_SIG {
        return None;
    }
    let (mut i, mut out) = (8, Vec::new());
    while i + 12 <= data.len() {
        let len = u32::from_be_bytes(data[i..i + 4].try_into().ok()?) as usize;
        let ty: [u8; 4] = data[i + 4..i + 8].try_into().ok()?;
        let body = data.get(i + 8..i + 8 + len)?;
        data.get(i + 8 + len..i + 12 + len)?;
        out.push((ty, body));
        i += 12 + len;
    }
    Some(out)
}

pub fn orientation_png(data: &[u8]) -> Option<u16> {
    png_chunks(data)?
        .into_iter()
        .find(|(t, _)| t == b"eXIf")
        .and_then(|(_, b)| orientation(b))
}

pub fn strip_png(data: &[u8], o: StripOptions) -> Option<Vec<u8>> {
    let mut out = PNG_SIG.to_vec();
    for (ty, body) in png_chunks(data)? {
        let body: Vec<u8> = match &ty {
            b"eXIf" if o.exif => continue,
            b"eXIf" if o.gps => {
                let mut b = body.to_vec();
                if strip_gps(&mut b).is_none() {
                    continue;
                }
                b
            }
            b"tEXt" | b"zTXt" | b"iTXt" | b"tIME" if o.comments => continue,
            b"iCCP" if o.color_profile => continue,
            _ => body.to_vec(),
        };
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut crc_in = ty.to_vec();
        crc_in.extend_from_slice(&body);
        out.extend_from_slice(&ty);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
    }
    Some(out)
}

// ------------------------------------------------------------------ WebP

pub fn orientation_webp(data: &[u8]) -> Option<u16> {
    webp_chunks(data)?
        .into_iter()
        .find(|(t, _)| t == b"EXIF")
        .and_then(|(_, b)| orientation(b.strip_prefix(b"Exif\0\0").unwrap_or(b)))
}

fn webp_chunks(data: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    if data.get(0..4)? != b"RIFF" || data.get(8..12)? != b"WEBP" {
        return None;
    }
    let (mut i, mut out) = (12, Vec::new());
    while i + 8 <= data.len() {
        let ty: [u8; 4] = data[i..i + 4].try_into().ok()?;
        let len = u32::from_le_bytes(data[i + 4..i + 8].try_into().ok()?) as usize;
        let body = data.get(i + 8..i + 8 + len)?;
        out.push((ty, body));
        i += 8 + len + (len & 1);
    }
    Some(out)
}

pub fn strip_webp(data: &[u8], o: StripOptions) -> Option<Vec<u8>> {
    let mut body = b"WEBP".to_vec();
    for (ty, chunk) in webp_chunks(data)? {
        let mut chunk = chunk.to_vec();
        match &ty {
            b"VP8X" if chunk.len() >= 10 => {
                if o.exif {
                    chunk[0] &= !0x08;
                }
                if o.comments {
                    chunk[0] &= !0x04;
                }
                if o.color_profile {
                    chunk[0] &= !0x20;
                }
            }
            b"EXIF" if o.exif => continue,
            b"EXIF" if o.gps => {
                let prefix = chunk.starts_with(b"Exif\0\0");
                let off = if prefix { 6 } else { 0 };
                if strip_gps(&mut chunk[off..]).is_none() {
                    continue;
                }
            }
            b"XMP " if o.comments => continue,
            b"ICCP" if o.color_profile => continue,
            _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF with Orientation=6, a GPS IFD (lat/lon rationals + ref), and a thumbnail.
    pub fn sample_tiff(orientation: u16) -> Vec<u8> {
        let mut t = vec![0u8; 400];
        t[0..4].copy_from_slice(b"II*\0");
        let w16 = |t: &mut Vec<u8>, o: usize, v: u16| t[o..o + 2].copy_from_slice(&v.to_le_bytes());
        let w32 = |t: &mut Vec<u8>, o: usize, v: u32| t[o..o + 4].copy_from_slice(&v.to_le_bytes());
        w32(&mut t, 4, 8); // IFD0 at 8
        w16(&mut t, 8, 3); // 3 entries
                           // entry: Make (0x010F) ASCII "Canon\0" > 4 bytes, out of line at 200
        w16(&mut t, 10, 0x010F);
        w16(&mut t, 12, 2);
        w32(&mut t, 14, 6);
        w32(&mut t, 18, 200);
        // entry: Orientation (0x0112) SHORT inline
        w16(&mut t, 22, 0x0112);
        w16(&mut t, 24, 3);
        w32(&mut t, 26, 1);
        w16(&mut t, 30, orientation);
        // entry: GPS pointer (0x8825) LONG -> 100
        w16(&mut t, 34, 0x8825);
        w16(&mut t, 36, 4);
        w32(&mut t, 38, 1);
        w32(&mut t, 42, 100);
        w32(&mut t, 46, 300); // next IFD = IFD1 at 300
        t[200..206].copy_from_slice(b"Canon\0");
        // GPS IFD at 100: 2 entries: LatitudeRef inline "N", Latitude 3 rationals (24 bytes) at 140
        w16(&mut t, 100, 2);
        w16(&mut t, 102, 0x0001);
        w16(&mut t, 104, 2);
        w32(&mut t, 106, 2);
        t[110] = b'N';
        w16(&mut t, 114, 0x0002);
        w16(&mut t, 116, 5);
        w32(&mut t, 118, 3);
        w32(&mut t, 122, 140);
        for b in &mut t[140..164] {
            *b = 0x77;
        }
        // IFD1 at 300: thumbnail offset 0x0201 -> 340, length 0x0202 = 20
        w16(&mut t, 300, 2);
        w16(&mut t, 302, 0x0201);
        w16(&mut t, 304, 4);
        w32(&mut t, 306, 1);
        w32(&mut t, 310, 340);
        w16(&mut t, 314, 0x0202);
        w16(&mut t, 316, 4);
        w32(&mut t, 318, 1);
        w32(&mut t, 322, 20);
        for b in &mut t[340..360] {
            *b = 0x55;
        }
        t
    }

    fn opts(exif: bool, gps: bool, comments: bool, icc: bool) -> StripOptions {
        StripOptions {
            exif,
            gps,
            comments,
            color_profile: icc,
        }
    }

    #[test]
    fn reads_orientation() {
        assert_eq!(orientation(&sample_tiff(6)), Some(6));
        assert_eq!(orientation(b"garbage"), None);
        assert_eq!(orientation(&sample_tiff(6)[..20]), None);
    }

    #[test]
    fn gps_and_thumbnail_bytes_are_really_erased_but_the_rest_survives() {
        let mut t = sample_tiff(6);
        strip_gps(&mut t).unwrap();
        assert!(
            !t.contains(&0x77) && !t.contains(&0x55),
            "GPS values and thumbnail must be zeroed"
        );
        assert_eq!(orientation(&t), Some(6));
        assert_eq!(&t[200..205], b"Canon");
        // GPS pointer entry is gone: IFD0 now has 2 entries
        assert_eq!(u16::from_le_bytes([t[8], t[9]]), 2);
        // second pass is a no-op that still succeeds
        assert!(strip_gps(&mut t).is_some());
    }

    #[test]
    fn malformed_tiff_never_panics() {
        for len in 0..sample_tiff(1).len() {
            let mut t = sample_tiff(1)[..len].to_vec();
            let _ = strip_gps(&mut t);
            let _ = orientation(&t);
        }
        let mut evil = sample_tiff(1);
        evil[38..42].copy_from_slice(&1u32.to_le_bytes());
        evil[42..46].copy_from_slice(&u32::MAX.to_le_bytes()); // GPS offset out of range
        assert!(strip_gps(&mut evil).is_none());
    }

    fn jpeg_with(segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        for (m, body) in segments {
            j.extend_from_slice(&[0xFF, *m]);
            j.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
            j.extend_from_slice(body);
        }
        j.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xAB, 0xCD, 0xFF, 0xD9]); // fake scan + EOI
        j
    }

    fn exif_segment() -> (u8, Vec<u8>) {
        (0xE1, [b"Exif\0\0".to_vec(), sample_tiff(6)].concat())
    }

    #[test]
    fn jpeg_options() {
        let src = jpeg_with(&[
            (0xE0, b"JFIF\0".to_vec()),
            exif_segment(),
            (0xE1, b"http://ns.adobe.com/xap/1.0/\0<x/>".to_vec()),
            (0xED, b"Photoshop 3.0\0".to_vec()),
            (0xFE, b"hello".to_vec()),
            (0xE2, b"ICC_PROFILE\0data".to_vec()),
        ]);
        let has = |j: &[u8], needle: &[u8]| j.windows(needle.len()).any(|w| w == needle);

        let all = strip_jpeg(&src, opts(true, true, true, true)).unwrap();
        for n in [&b"Exif"[..], b"xap", b"Photoshop", b"hello", b"ICC_PROFILE"] {
            assert!(!has(&all, n), "{:?}", String::from_utf8_lossy(n));
        }
        assert!(has(&all, b"JFIF") && all.ends_with(&[0xAB, 0xCD, 0xFF, 0xD9]));

        let keep_icc = strip_jpeg(&src, opts(true, true, true, false)).unwrap();
        assert!(has(&keep_icc, b"ICC_PROFILE"));

        let gps_only = strip_jpeg(&src, opts(false, true, false, false)).unwrap();
        assert!(has(&gps_only, b"Exif") && has(&gps_only, b"hello") && has(&gps_only, b"Canon"));
        assert!(!gps_only.contains(&0x77));
        assert_eq!(orientation_jpeg(&gps_only), Some(6));

        let nothing = strip_jpeg(&src, opts(false, false, false, false)).unwrap();
        assert_eq!(nothing, src);
    }

    #[test]
    fn jpeg_garbage_is_rejected_not_panicked() {
        assert!(strip_jpeg(b"nope", opts(true, true, true, true)).is_none());
        let src = jpeg_with(&[exif_segment()]);
        for len in 0..src.len() {
            let _ = strip_jpeg(&src[..len], opts(true, true, true, true));
            let _ = orientation_jpeg(&src[..len]);
        }
        // a segment that claims more bytes than exist
        let mut bad = vec![0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF, 1, 2, 3];
        assert!(strip_jpeg(&bad, opts(true, true, true, true)).is_none());
        bad.truncate(2);
        let _ = strip_jpeg(&bad, opts(true, true, true, true));
    }

    fn png_with(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut p = PNG_SIG.to_vec();
        for (ty, body) in chunks {
            p.extend_from_slice(&(body.len() as u32).to_be_bytes());
            p.extend_from_slice(*ty);
            p.extend_from_slice(body);
            p.extend_from_slice(&crc32(&[&ty[..], body].concat()).to_be_bytes());
        }
        p
    }

    #[test]
    fn crc32_matches_the_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn png_options_keep_valid_crcs() {
        let src = png_with(&[
            (b"IHDR", vec![0; 13]),
            (b"eXIf", sample_tiff(3)),
            (b"tEXt", b"Comment\0hi".to_vec()),
            (b"iCCP", b"p\0\0z".to_vec()),
            (b"IDAT", vec![1, 2, 3]),
            (b"IEND", vec![]),
        ]);
        let types = |p: &[u8]| {
            png_chunks(p)
                .unwrap()
                .into_iter()
                .map(|(t, _)| String::from_utf8_lossy(&t).into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(orientation_png(&src), Some(3));
        assert_eq!(
            types(&strip_png(&src, opts(true, true, true, true)).unwrap()),
            ["IHDR", "IDAT", "IEND"]
        );
        assert_eq!(
            types(&strip_png(&src, opts(true, true, true, false)).unwrap()),
            ["IHDR", "iCCP", "IDAT", "IEND"]
        );
        let gps = strip_png(&src, opts(false, true, false, false)).unwrap();
        assert!(
            types(&gps).contains(&"eXIf".to_string()) && types(&gps).contains(&"tEXt".to_string())
        );
        assert!(!gps.contains(&0x77));
        // every chunk CRC in the output is still valid (png_chunks does not check, so check here)
        let mut i = 8;
        while i + 12 <= gps.len() {
            let len = u32::from_be_bytes(gps[i..i + 4].try_into().unwrap()) as usize;
            let crc = u32::from_be_bytes(gps[i + 8 + len..i + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&gps[i + 4..i + 8 + len]));
            i += 12 + len;
        }
        assert!(strip_png(b"not a png", opts(true, true, true, true)).is_none());
    }

    #[test]
    fn webp_options_and_riff_size() {
        let mk = |ty: &[u8; 4], body: &[u8]| {
            let mut c = ty.to_vec();
            c.extend_from_slice(&(body.len() as u32).to_le_bytes());
            c.extend_from_slice(body);
            if body.len() & 1 == 1 {
                c.push(0);
            }
            c
        };
        let mut vp8x = vec![0x2C, 0, 0, 0]; // ICC | EXIF | XMP flags
        vp8x.extend_from_slice(&[0; 6]);
        let mut body = b"WEBP".to_vec();
        for c in [
            mk(b"VP8X", &vp8x),
            mk(b"EXIF", &[b"Exif\0\0".to_vec(), sample_tiff(8)].concat()),
            mk(b"XMP ", b"<x/>"),
            mk(b"ICCP", b"abc"),
            mk(b"VP8L", &[1, 2, 3]),
        ] {
            body.extend_from_slice(&c);
        }
        let mut src = b"RIFF".to_vec();
        src.extend_from_slice(&(body.len() as u32).to_le_bytes());
        src.extend_from_slice(&body);
        assert_eq!(orientation_webp(&src), Some(8));
        let out = strip_webp(&src, opts(true, true, true, true)).unwrap();
        let names: Vec<_> = webp_chunks(&out)
            .unwrap()
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(names, [*b"VP8X", *b"VP8L"]);
        assert_eq!(webp_chunks(&out).unwrap()[0].1[0], 0); // flags cleared
        assert_eq!(
            u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize,
            out.len() - 8
        );
        let gps = strip_webp(&src, opts(false, true, false, false)).unwrap();
        assert!(!gps.contains(&0x77) && orientation_webp(&gps) == Some(8));
        assert!(strip_webp(b"RIFFxxxxWAVE", opts(true, true, true, true)).is_none());
    }
}
