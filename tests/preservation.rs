//! Integration tests for issue #14: the overlay merge must preserve the
//! original document's metadata, bookmarks, and links.
//!
//! The fixtures under `tests/fixtures/preservation/` are committed,
//! hand-written minimal PDFs (regenerable with `make_fixtures.py`),
//! each carrying one structure the merge must not disturb: an Info
//! dictionary with Unicode values, a three-level outline, internal and
//! external link annotations, and a rotated page combined with
//! metadata. Every test runs the real path-based overlay API on a
//! fixture, reloads the OUTPUT with lopdf, and compares it object by
//! object against the reloaded INPUT.

use chilli_pdf::ocr::{OcrLine, OcrPage, OcrWord, Rect};
use chilli_pdf::overlay::{OverlayPage, write_overlay};
use chilli_pdf::pdf::{PageBox, PageMetadata, PageRotation};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::path::PathBuf;

// --- helpers ---

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/preservation");
const FONT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pdf.ttf");
/// Letter crop box (also the media box of the unrotated fixtures).
const LETTER: PageBox = PageBox {
    left: 0.0,
    bottom: 0.0,
    right: 612.0,
    top: 792.0,
};
/// The rotated fixture's landscape media box [0 0 792 612].
const LANDSCAPE: PageBox = PageBox {
    left: 0.0,
    bottom: 0.0,
    right: 792.0,
    top: 612.0,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(FIXTURES).join(name)
}

fn glyphless_font() -> Vec<u8> {
    std::fs::read(FONT).expect("glyphless font fixture is committed")
}

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("tmpdir");
    dir.join(name)
}

/// One small OCR page: a single word box in a 144 DPI letter render.
fn one_word_ocr() -> OcrPage {
    OcrPage {
        width: 1224,
        height: 1584,
        dpi: 144,
        lines: vec![OcrLine {
            rect: Rect::new(144, 144, 420, 180),
            words: vec![OcrWord {
                text: "hello".to_owned(),
                confidence: 95.0,
                rect: Rect::new(144, 144, 420, 180),
            }],
        }],
    }
}

fn letter_overlay_pages(count: usize) -> Vec<OverlayPage> {
    (0..count)
        .map(|_| OverlayPage {
            ocr: one_word_ocr(),
            metadata: PageMetadata {
                rotation: PageRotation::None,
                crop_box: LETTER,
            },
        })
        .collect()
}

fn rotated_overlay_page() -> Vec<OverlayPage> {
    vec![OverlayPage {
        ocr: one_word_ocr(),
        metadata: PageMetadata {
            rotation: PageRotation::Degrees90,
            crop_box: LANDSCAPE,
        },
    }]
}

/// Run the overlay on `input`, asserting the input file is untouched,
/// and return the output path.
fn merged(input: &str, out_name: &str, pages: Vec<OverlayPage>) -> PathBuf {
    let input_path = fixture(input);
    let before = std::fs::read(&input_path).expect("fixture readable");
    let output = tmp(out_name);
    write_overlay(&input_path, &output, &pages, &glyphless_font())
        .unwrap_or_else(|e| panic!("overlaying {input}: {e}"));
    assert_eq!(
        std::fs::read(&input_path).expect("reread fixture"),
        before,
        "{input}: the input file must never be touched"
    );
    output
}

fn deref<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
    doc.dereference(object).expect("dereference").1
}

fn resolved_dict<'a>(doc: &'a Document, object: &'a Object) -> &'a Dictionary {
    deref(doc, object).as_dict().expect("dictionary")
}

fn page_dict(doc: &Document, page: usize) -> Dictionary {
    let id = *doc.get_pages().values().nth(page).expect("page");
    doc.get_object(id)
        .expect("page object")
        .as_dict()
        .expect("page dictionary")
        .clone()
}

/// The (object id, dictionary) the trailer's /Info entry points at.
fn info_dict(doc: &Document) -> (ObjectId, Dictionary) {
    let reference = doc
        .trailer
        .get(b"Info")
        .expect("trailer /Info")
        .as_reference()
        .expect("indirect /Info object");
    let dict = doc
        .get_object(reference)
        .expect("info object")
        .as_dict()
        .expect("info dictionary")
        .clone();
    (reference, dict)
}

/// Semantic object equality: string payloads compare by decoded bytes
/// regardless of literal/hex encoding; arrays and dictionaries compare
/// element by element; everything else uses `Object`'s own equality.
fn equivalent(a: &Object, b: &Object) -> bool {
    match (a, b) {
        (Object::String(a, _), Object::String(b, _)) => a == b,
        (Object::Array(a), Object::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| equivalent(x, y))
        }
        (Object::Dictionary(a), Object::Dictionary(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_ok_and(|other| equivalent(value, other)))
        }
        _ => a == b,
    }
}

/// Decode a PDF text string in UTF-16BE form (byte-order mark + units).
fn utf16_string(bytes: &[u8]) -> String {
    assert!(bytes.starts_with(&[0xFE, 0xFF]), "missing UTF-16BE BOM");
    let units: Vec<u16> = bytes[2..]
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).expect("valid UTF-16BE")
}

fn string_bytes(object: &Object) -> &[u8] {
    match object {
        Object::String(bytes, _) => bytes,
        other => panic!("expected a string, got {other:?}"),
    }
}

/// The page a destination (array or GoTo action) points at, by index in
/// document order.
fn dest_page_index(doc: &Document, dest: &Object) -> usize {
    let target = match dest {
        Object::Array(items) => items.first().expect("dest page entry"),
        Object::Dictionary(action) => action
            .get(b"D")
            .expect("action /D")
            .as_array()
            .expect("dest array")
            .first()
            .expect("dest page entry"),
        other => panic!("unexpected destination: {other:?}"),
    };
    let page = target.as_reference().expect("dest page reference");
    // get_pages() keys pages 1..=n in document order, so the position of
    // the object id among its values IS the zero-based page index.
    doc.get_pages()
        .values()
        .position(|id| *id == page)
        .expect("dest points at a page")
}

/// The page's raw /Contents entries (references preserved as stored:
/// a single stream stays a single reference, an array stays an array).
fn contents_entries(dict: &Dictionary) -> Vec<Object> {
    match dict.get(b"Contents").expect("page contents") {
        Object::Array(items) => items.clone(),
        single => vec![single.clone()],
    }
}

// --- metadata fixture ---

#[test]
fn info_dict_entries_survive_the_overlay() {
    let output = merged("metadata.pdf", "metadata_info.pdf", letter_overlay_pages(2));
    let before = Document::load(fixture("metadata.pdf")).unwrap();
    let after = Document::load(&output).unwrap();

    let (id_before, info_before) = info_dict(&before);
    let (id_after, info_after) = info_dict(&after);

    // The Info dictionary is the same object, not renumbered, and still
    // referenced indirectly from the trailer.
    assert_eq!(id_before, id_after, "the Info object must keep its id");
    let mut keys_before: Vec<&[u8]> = info_before.iter().map(|(k, _)| k.as_slice()).collect();
    let mut keys_after: Vec<&[u8]> = info_after.iter().map(|(k, _)| k.as_slice()).collect();
    keys_before.sort();
    keys_after.sort();
    assert_eq!(keys_before, keys_after, "the Info key set changed");

    for (key, value) in info_before.iter() {
        let survived = info_after.get(key).expect("Info entry preserved");
        assert!(
            equivalent(value, survived),
            "Info entry {:?} changed",
            String::from_utf8_lossy(key)
        );
    }

    // The Unicode title round-trips byte for byte (UTF-16BE with BOM),
    // double-acute letters included.
    let title = utf16_string(string_bytes(info_after.get(b"Title").expect("title entry")));
    assert_eq!(title, "Teszt dokumentum: ő és ű");
    // Dates and the custom key are strings too.
    for key in [
        b"CreationDate".as_slice(),
        b"ModDate".as_slice(),
        b"Company".as_slice(),
    ] {
        let (a, b) = (
            info_before.get(key).expect("entry"),
            info_after.get(key).expect("entry"),
        );
        assert_eq!(string_bytes(a), string_bytes(b));
    }

    // The overlay was actually applied: both pages' /Contents grew by
    // exactly one stream.
    for page in 0..2 {
        let before_contents = contents_entries(&page_dict(&before, page));
        let after_contents = contents_entries(&page_dict(&after, page));
        assert_eq!(after_contents.len(), before_contents.len() + 1);
        assert_eq!(before_contents[0], after_contents[0]);
    }
}

#[test]
fn existing_font_resources_are_kept_beside_the_overlay_font() {
    let output = merged(
        "metadata.pdf",
        "metadata_fonts.pdf",
        letter_overlay_pages(2),
    );
    let after = Document::load(&output).unwrap();

    for page in 0..2 {
        let dict = page_dict(&after, page);
        let resources = resolved_dict(&after, dict.get(b"Resources").expect("resources"));
        let fonts = resolved_dict(&after, resources.get(b"Font").expect("font dict"));

        // The page's original font entry is untouched...
        let f1 = resolved_dict(&after, fonts.get(b"F1").expect("original font"));
        assert_eq!(f1.get(b"Subtype").unwrap().as_name().unwrap(), b"Type1");
        assert_eq!(
            f1.get(b"BaseFont").unwrap().as_name().unwrap(),
            b"Helvetica"
        );
        // ...and the overlay font was added next to it.
        assert!(
            fonts.has(b"ChilliOcrF1"),
            "page {page}: the overlay font resource is missing"
        );
    }
}

// --- outline fixture ---

/// One outline item as seen from the document, in walk order.
#[derive(Debug, PartialEq)]
struct OutlineItem {
    id: ObjectId,
    depth: usize,
    title: Vec<u8>,
    parent: Option<ObjectId>,
    /// `/Dest` array or `/A` action, as stored.
    destination: Option<Object>,
}

fn walk_outline(doc: &Document, item: &Object, depth: usize, out: &mut Vec<OutlineItem>) {
    let (id, object) = doc.dereference(item).expect("outline item");
    let id = id.expect("outline items are indirect");
    let dict = object.as_dict().expect("outline dictionary");
    let title = string_bytes(dict.get(b"Title").expect("title")).to_vec();
    let parent = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    let destination = dict
        .get(b"Dest")
        .ok()
        .cloned()
        .or_else(|| dict.get(b"A").ok().cloned());
    out.push(OutlineItem {
        id,
        depth,
        title,
        parent,
        destination,
    });
    if let Ok(child) = dict.get(b"First") {
        walk_outline(doc, child, depth + 1, out);
    }
    if let Ok(next) = dict.get(b"Next") {
        walk_outline(doc, next, depth, out);
    }
}

fn outline_items(doc: &Document) -> Vec<OutlineItem> {
    let catalog = doc.catalog().expect("catalog");
    let mut items = Vec::new();
    let outlines = catalog.get(b"Outlines").expect("catalog /Outlines");
    let root_id = outlines.as_reference().expect("indirect /Outlines");
    let root = doc
        .get_object(root_id)
        .expect("outlines object")
        .as_dict()
        .expect("outlines dictionary");
    // The root itself must survive with the same id.
    items.push(OutlineItem {
        id: root_id,
        depth: 0,
        title: Vec::new(),
        parent: None,
        destination: None,
    });
    if let Ok(first) = root.get(b"First") {
        walk_outline(doc, first, 1, &mut items);
    }
    items
}

#[test]
fn outline_titles_and_destinations_survive_the_overlay() {
    let output = merged("outline.pdf", "outline_merged.pdf", letter_overlay_pages(2));
    let before = Document::load(fixture("outline.pdf")).unwrap();
    let after = Document::load(&output).unwrap();

    let items_before = outline_items(&before);
    let items_after = outline_items(&after);
    assert_eq!(items_before.len(), items_after.len());

    // Every item keeps its object id (no renumbering), depth, parent,
    // and title bytes.
    for (b, a) in items_before.iter().zip(&items_after) {
        assert_eq!(b.id, a.id, "outline items must keep their object ids");
        assert_eq!(b.depth, a.depth);
        assert_eq!(b.title, a.title);
        assert_eq!(b.parent, a.parent);
    }

    // Three levels deep, pointing at pages 1 and 2 (zero-based 0 and 1).
    // Walk order: root, level-1 item 1, level-2, level-3, level-1 item 2.
    let depths: Vec<usize> = items_after.iter().map(|item| item.depth).collect();
    assert_eq!(depths, vec![0, 1, 2, 3, 1]);
    let dests: Vec<usize> = items_after
        .iter()
        .skip(1)
        .map(|item| dest_page_index(&after, item.destination.as_ref().expect("dest")))
        .collect();
    assert_eq!(dests, vec![0, 0, 0, 1]);

    // Destinations are stored unchanged (params included); the walk's
    // first entry is the Outlines root, which has no destination.
    for (b, a) in items_before.iter().skip(1).zip(items_after.iter().skip(1)) {
        assert!(equivalent(
            b.destination.as_ref().expect("dest"),
            a.destination.as_ref().expect("dest")
        ));
    }
}

// --- links fixture ---

/// One link annotation as seen from a page, in /Annots order.
#[derive(Debug)]
struct LinkAnnotation {
    id: ObjectId,
    rect: Vec<f64>,
    /// `/Dest` array, when present.
    dest: Option<Object>,
    /// `/A` action dictionary, when present.
    action: Option<Object>,
}

fn link_annotations(doc: &Document, page: usize) -> Vec<LinkAnnotation> {
    let dict = page_dict(doc, page);
    let annots = dict
        .get(b"Annots")
        .expect("page annotations")
        .as_array()
        .expect("annots array")
        .clone();
    annots
        .iter()
        .map(|entry| {
            let (id, object) = doc.dereference(entry).expect("annotation");
            let id = id.expect("annotations are indirect");
            let dict = object.as_dict().expect("annotation dictionary");
            assert_eq!(
                dict.get(b"Subtype").unwrap().as_name().unwrap(),
                b"Link",
                "annotation {id:?}"
            );
            let rect = dict
                .get(b"Rect")
                .expect("annotation /Rect")
                .as_array()
                .expect("rect array")
                .iter()
                .map(|o| o.as_i64().expect("rect number") as f64)
                .collect();
            LinkAnnotation {
                id,
                rect,
                dest: dict.get(b"Dest").ok().cloned(),
                action: dict.get(b"A").ok().cloned(),
            }
        })
        .collect()
}

#[test]
fn internal_and_external_links_survive_the_overlay() {
    let output = merged("links.pdf", "links_merged.pdf", letter_overlay_pages(2));
    let before = Document::load(fixture("links.pdf")).unwrap();
    let after = Document::load(&output).unwrap();

    for page in 0..2 {
        let before_links = link_annotations(&before, page);
        let after_links = link_annotations(&after, page);
        assert_eq!(
            before_links.len(),
            after_links.len(),
            "page {page}: link count changed"
        );
        for (b, a) in before_links.iter().zip(&after_links) {
            assert_eq!(b.id, a.id, "annotations must keep their object ids");
            assert_eq!(b.rect, a.rect, "hit rectangle changed");
            for (b, a) in [
                (b.dest.as_ref(), a.dest.as_ref()),
                (b.action.as_ref(), a.action.as_ref()),
            ] {
                let (Some(b), Some(a)) = (b, a) else { continue };
                assert!(equivalent(b, a), "link target changed");
            }
        }
    }

    // The internal links still target page 2 by its ORIGINAL object id,
    // and the external link still carries the same URI action.
    let page_two = *before.get_pages().values().nth(1).expect("second page");
    let links = link_annotations(&after, 0);
    assert_eq!(links.len(), 2);
    for link in &links {
        let target = link.dest.as_ref().or(link.action.as_ref()).expect("target");
        let pointed = match target {
            Object::Array(items) => items[0].as_reference().unwrap(),
            Object::Dictionary(action) => action.get(b"D").unwrap().as_array().unwrap()[0]
                .as_reference()
                .unwrap(),
            other => panic!("unexpected link target: {other:?}"),
        };
        assert_eq!(pointed, page_two, "internal link destination was broken");
    }
    let external = &link_annotations(&after, 1)[0];
    let action = external.action.as_ref().expect("URI action");
    let dict = action.as_dict().expect("action dictionary");
    assert_eq!(dict.get(b"S").unwrap().as_name().unwrap(), b"URI");
}

// --- rotated + metadata fixture ---

#[test]
fn rotated_page_with_metadata_survives_the_overlay() {
    let output = merged(
        "rotated_metadata.pdf",
        "rotated_merged.pdf",
        rotated_overlay_page(),
    );
    let before = Document::load(fixture("rotated_metadata.pdf")).unwrap();
    let after = Document::load(&output).unwrap();

    assert_eq!(after.get_pages().len(), 1);

    let before_page = page_dict(&before, 0);
    let after_page = page_dict(&after, 0);

    // Rotation and geometry are untouched.
    assert_eq!(
        before_page.get(b"Rotate").unwrap(),
        after_page.get(b"Rotate").unwrap(),
        "/Rotate changed"
    );
    assert_eq!(
        after_page.get(b"Rotate").unwrap().as_i64().unwrap(),
        90,
        "the fixture must stay rotated"
    );
    assert_eq!(
        before_page.get(b"MediaBox").unwrap(),
        after_page.get(b"MediaBox").unwrap(),
        "/MediaBox changed"
    );

    // The Info dictionary is preserved alongside the rotation.
    let (id_before, info_before) = info_dict(&before);
    let (id_after, info_after) = info_dict(&after);
    assert_eq!(id_before, id_after);
    for (key, value) in info_before.iter() {
        let survived = info_after.get(key).expect("Info entry preserved");
        assert!(
            equivalent(value, survived),
            "Info entry {:?} changed",
            String::from_utf8_lossy(key)
        );
    }
    let title = utf16_string(string_bytes(info_after.get(b"Title").expect("title entry")));
    assert_eq!(title, "Forgatott teszt ű");

    // /Contents grew by exactly the overlay stream and the original
    // stream bytes are unchanged.
    let before_contents = contents_entries(&before_page);
    let after_contents = contents_entries(&after_page);
    assert_eq!(after_contents.len(), before_contents.len() + 1);
    assert_eq!(before_contents[0], after_contents[0]);
    let original_stream = deref(&after, &after_contents[0]).as_stream().unwrap();
    let expected = deref(&before, &before_contents[0]).as_stream().unwrap();
    assert_eq!(original_stream.content, expected.content);
}

// --- structural checks across every fixture ---

#[test]
fn page_tree_and_order_are_unchanged_for_every_fixture() {
    let cases: &[(&str, &str, Vec<OverlayPage>)] = &[
        ("metadata.pdf", "tree_metadata.pdf", letter_overlay_pages(2)),
        ("outline.pdf", "tree_outline.pdf", letter_overlay_pages(2)),
        ("links.pdf", "tree_links.pdf", letter_overlay_pages(2)),
        (
            "rotated_metadata.pdf",
            "tree_rotated.pdf",
            rotated_overlay_page(),
        ),
    ];
    for (input, out_name, pages) in cases {
        let output = merged(input, out_name, pages.clone());
        let before = Document::load(fixture(input)).unwrap();
        let after = Document::load(&output).unwrap();

        // Same page count and the same page objects in the same order.
        assert_eq!(
            before.get_pages(),
            after.get_pages(),
            "{input}: page tree changed"
        );
        let before_pages = page_dict(&before, 0)
            .get(b"Parent")
            .expect("parent")
            .as_reference()
            .expect("indirect pages object");
        let after_pages = page_dict(&after, 0)
            .get(b"Parent")
            .expect("parent")
            .as_reference()
            .expect("indirect pages object");
        assert_eq!(before_pages, after_pages, "{input}: pages object moved");
        let kids = |doc: &Document, id: ObjectId| {
            doc.get_object(id)
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"Kids")
                .unwrap()
                .as_array()
                .unwrap()
                .clone()
        };
        assert_eq!(
            kids(&before, before_pages),
            kids(&after, after_pages),
            "{input}: /Kids order changed"
        );

        for page in 0..before.get_pages().len() {
            let b = page_dict(&before, page);
            let a = page_dict(&after, page);
            for key in [
                b"MediaBox".as_slice(),
                b"CropBox".as_slice(),
                b"Rotate".as_slice(),
            ] {
                if let Ok(expected) = b.get(key) {
                    assert_eq!(
                        expected,
                        a.get(key).expect("entry preserved"),
                        "{input} page {page}: {key:?} changed"
                    );
                }
            }

            // /Contents gained exactly one overlay stream, appended.
            let b_contents = contents_entries(&b);
            let a_contents = contents_entries(&a);
            assert_eq!(
                a_contents.len(),
                b_contents.len() + 1,
                "{input} page {page}: overlay stream not appended"
            );
            assert_eq!(b_contents[0], a_contents[0]);
            let overlay = deref(&after, &a_contents[1])
                .as_stream()
                .expect("overlay stream");
            let text = String::from_utf8(overlay.content.clone()).unwrap();
            assert!(
                text.starts_with("q\nBT 3 Tr /ChilliOcrF1"),
                "{input} page {page}: unexpected overlay content"
            );
        }
    }
}
