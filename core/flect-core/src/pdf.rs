//! PDF merging with `lopdf` (memory-safe parser). Rendering, text extraction and preview are done
//! by the macOS app with PDFKit, which handles CJK fonts and CMaps far better than a crate could.
use std::fs;
use std::io;
use std::path::Path;

use lopdf::{Dictionary, Document, Object, ObjectId};

pub const MAX_INPUT_BYTES: u64 = 512 << 20;
pub const MAX_PAGES: usize = 100_000;
/// Page-tree attributes a page may inherit from its ancestors.
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

#[derive(Debug)]
pub enum PdfError {
    Encrypted,
    Invalid(String),
    NoPages,
    TooLarge,
    Io(io::Error),
}

impl PdfError {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Encrypted => "msg.pdfEncrypted",
            Self::Invalid(_) => "msg.pdfInvalid",
            Self::NoPages => "msg.noPages",
            Self::TooLarge => "msg.fileTooLarge",
            Self::Io(_) => "msg.ioError",
        }
    }
}

impl From<io::Error> for PdfError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<lopdf::Error> for PdfError {
    fn from(e: lopdf::Error) -> Self {
        Self::Invalid(e.to_string())
    }
}

fn load(path: &Path) -> Result<Document, PdfError> {
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(PdfError::Invalid("not a file".into()));
    }
    if meta.len() > MAX_INPUT_BYTES {
        return Err(PdfError::TooLarge);
    }
    let doc = Document::load_mem(&fs::read(path)?)?;
    if doc.is_encrypted() {
        return Err(PdfError::Encrypted);
    }
    Ok(doc)
}

/// Copy inherited attributes down into each page so it survives being re-parented.
fn flatten_inherited(doc: &mut Document) {
    let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
    for page in pages {
        let mut missing: Vec<(Vec<u8>, Object)> = Vec::new();
        let (mut cur, mut depth) = (page, 0);
        while depth < 64 {
            let Ok(dict) = doc.get_dictionary(cur) else {
                break;
            };
            if cur != page {
                for key in INHERITED {
                    let present = doc.get_dictionary(page).is_ok_and(|p| p.has(key))
                        || missing.iter().any(|(k, _)| k == key);
                    if !present {
                        if let Ok(v) = dict.get(key) {
                            missing.push((key.to_vec(), v.clone()));
                        }
                    }
                }
            }
            match dict.get(b"Parent").and_then(Object::as_reference) {
                Ok(parent) => cur = parent,
                Err(_) => break,
            }
            depth += 1;
        }
        if let Ok(p) = doc.get_dictionary_mut(page) {
            for (k, v) in missing {
                p.set(k, v);
            }
        }
    }
}

fn is_structural(o: &Object) -> bool {
    matches!(o.type_name().ok(), Some(b"Catalog" | b"Pages" | b"Page"))
}

/// Concatenate the documents' pages in the given order.
pub fn merge(paths: &[&Path]) -> Result<Vec<u8>, PdfError> {
    let mut merged = Document::with_version("1.7");
    let mut next_id = 1u32;
    let mut pages: Vec<(ObjectId, Dictionary)> = Vec::new();
    for path in paths {
        let mut doc = load(path)?;
        flatten_inherited(&mut doc);
        doc.renumber_objects_with(next_id);
        next_id = doc.max_id + 1;
        let order: Vec<ObjectId> = doc.get_pages().into_values().collect();
        if order.is_empty() {
            return Err(PdfError::NoPages);
        }
        if pages.len() + order.len() > MAX_PAGES {
            return Err(PdfError::TooLarge);
        }
        for id in &order {
            if let Ok(d) = doc.get_dictionary(*id) {
                pages.push((*id, d.clone()));
            }
        }
        for (id, obj) in doc.objects {
            if !is_structural(&obj) {
                merged.objects.insert(id, obj);
            }
        }
    }
    if pages.is_empty() {
        return Err(PdfError::NoPages);
    }
    let pages_id: ObjectId = (next_id, 0);
    let catalog_id: ObjectId = (next_id + 1, 0);
    let mut kids = Vec::with_capacity(pages.len());
    for (id, mut dict) in pages {
        dict.set("Parent", pages_id);
        merged.objects.insert(id, Object::Dictionary(dict));
        kids.push(Object::Reference(id));
    }
    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", "Pages");
    pages_dict.set("Count", kids.len() as i64);
    pages_dict.set("Kids", kids);
    merged
        .objects
        .insert(pages_id, Object::Dictionary(pages_dict));
    let mut catalog = Dictionary::new();
    catalog.set("Type", "Catalog");
    catalog.set("Pages", pages_id);
    merged
        .objects
        .insert(catalog_id, Object::Dictionary(catalog));
    merged.trailer.set("Root", catalog_id);
    merged.max_id = catalog_id.0;
    let mut out = Vec::new();
    merged.save_to(&mut out)?;
    Ok(out)
}

pub fn page_count(path: &Path) -> Result<usize, PdfError> {
    Ok(load(path)?.get_pages().len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Stream};

    /// A document whose pages inherit MediaBox and Resources from the Pages node.
    fn make(pages: &[&str], media: [i64; 4]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(
            dictionary! {"Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica"},
        );
        let resources = doc.add_object(dictionary! {"Font" => dictionary! {"F1" => font}});
        let mut kids = Vec::new();
        for text in pages {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 12.into()]),
                    Operation::new("Td", vec![50.into(), 700.into()]),
                    Operation::new("Tj", vec![Object::string_literal(*text)]),
                    Operation::new("ET", vec![]),
                ],
            };
            let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let page = doc.add_object(
                dictionary! {"Type" => "Page", "Parent" => pages_id, "Contents" => content_id},
            );
            kids.push(page.into());
        }
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! {"Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids, "MediaBox" => media.iter().map(|v| Object::Integer(*v)).collect::<Vec<_>>(), "Resources" => resources}));
        let catalog = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    fn page_texts(bytes: &[u8]) -> Vec<String> {
        let doc = Document::load_mem(bytes).unwrap();
        doc.get_pages()
            .into_values()
            .map(|id| String::from_utf8_lossy(&doc.get_page_content(id)).into_owned())
            .collect()
    }

    fn write(d: &Path, name: &str, bytes: Vec<u8>) -> std::path::PathBuf {
        let p = d.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn merges_in_the_given_order_and_keeps_inherited_geometry() {
        let d = tempfile::tempdir().unwrap();
        let a = write(d.path(), "a.pdf", make(&["A1", "A2"], [0, 0, 612, 792]));
        let b = write(d.path(), "b.pdf", make(&["B1"], [0, 0, 300, 400]));
        let out = merge(&[&b, &a]).unwrap();
        let texts = page_texts(&out);
        assert_eq!(texts.len(), 3);
        assert!(
            texts[0].contains("B1") && texts[1].contains("A1") && texts[2].contains("A2"),
            "{texts:?}"
        );
        let doc = Document::load_mem(&out).unwrap();
        let sizes: Vec<Vec<i64>> = doc
            .get_pages()
            .into_values()
            .map(|id| {
                doc.get_dictionary(id)
                    .unwrap()
                    .get(b"MediaBox")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_i64().unwrap())
                    .collect()
            })
            .collect();
        assert_eq!(
            sizes,
            [
                vec![0, 0, 300, 400],
                vec![0, 0, 612, 792],
                vec![0, 0, 612, 792]
            ],
            "each page keeps the size it inherited from its own document"
        );
        // fonts still resolve for every page (resources were flattened, not lost)
        for id in doc.get_pages().into_values() {
            let (inline, refs) = doc.get_page_resources(id).unwrap();
            let has_font = inline.is_some_and(|r| r.has(b"Font"))
                || refs
                    .iter()
                    .any(|r| doc.get_dictionary(*r).is_ok_and(|d| d.has(b"Font")));
            assert!(has_font, "page {id:?} lost its resources");
        }
        assert_eq!(
            doc.catalog()
                .unwrap()
                .get(b"Type")
                .unwrap()
                .as_name()
                .unwrap(),
            b"Catalog"
        );
    }

    #[test]
    fn same_file_twice_and_many_files() {
        let d = tempfile::tempdir().unwrap();
        let a = write(d.path(), "a.pdf", make(&["X"], [0, 0, 100, 100]));
        let out = merge(&[&a, &a, &a]).unwrap();
        assert_eq!(page_texts(&out).len(), 3);
        let again = write(d.path(), "m.pdf", out);
        assert_eq!(page_count(&again).unwrap(), 3);
        assert_eq!(page_texts(&merge(&[&again, &a]).unwrap()).len(), 4);
    }

    #[test]
    fn rejects_garbage_encrypted_and_empty() {
        let d = tempfile::tempdir().unwrap();
        let good = write(d.path(), "g.pdf", make(&["G"], [0, 0, 10, 10]));
        let bad = write(d.path(), "bad.pdf", b"%PDF-1.4 this is not a pdf".to_vec());
        assert_eq!(merge(&[&good, &bad]).unwrap_err().key(), "msg.pdfInvalid");
        assert_eq!(
            merge(&[&d.path().join("missing.pdf")]).unwrap_err().key(),
            "msg.ioError"
        );
        assert_eq!(merge(&[]).unwrap_err().key(), "msg.noPages");
        let mut doc = Document::load_mem(&fs::read(&good).unwrap()).unwrap();
        doc.trailer
            .set("Encrypt", Object::Dictionary(Dictionary::new()));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        let enc = write(d.path(), "e.pdf", buf);
        assert!(
            merge(&[&enc]).is_err(),
            "a document with an Encrypt entry is never merged as if it were plain"
        );
        // truncated files never panic
        let bytes = fs::read(&good).unwrap();
        for cut in [0, 5, bytes.len() / 3, bytes.len() - 5] {
            let t = write(d.path(), "t.pdf", bytes[..cut].to_vec());
            let _ = merge(&[&t]);
        }
    }

    #[test]
    fn a_page_tree_loop_does_not_hang() {
        let d = tempfile::tempdir().unwrap();
        let mut doc = Document::load_mem(&make(&["L"], [0, 0, 10, 10])).unwrap();
        let pages_id = doc
            .catalog()
            .unwrap()
            .get(b"Pages")
            .unwrap()
            .as_reference()
            .unwrap();
        doc.get_dictionary_mut(pages_id)
            .unwrap()
            .set("Parent", pages_id); // Pages node is its own parent
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        let p = write(d.path(), "loop.pdf", buf);
        let started = std::time::Instant::now();
        let _ = merge(&[&p]);
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
