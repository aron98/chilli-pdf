//! Merging the overlay into the original PDF, object by object.
//!
//! The overlay is written by loading the ORIGINAL input file with `lopdf`
//! (pdfium-render is a reader/render API and cannot write objects) and
//! appending each page's overlay content as an additional content stream,
//! wrapped in `q … Q`, referenced from the page's `/Contents` array. The
//! original content streams are never decoded or rewritten, so the
//! rendered appearance — image streams, boxes, rotation — is preserved
//! byte for byte.
//!
//! The font objects are added once per document and shared by all pages.

use super::OverlayPage;
use super::content;
use super::error::OverlayError;
use super::font;
use super::layout;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::path::Path;

/// Write `pages`' invisible-text overlays into the PDF at `input` and save
/// the merged document to `output`.
pub(crate) fn merge_overlay(
    input: &Path,
    output: &Path,
    pages: &[OverlayPage],
    font_bytes: &[u8],
) -> Result<(), OverlayError> {
    let mut doc = Document::load(input).map_err(|_| OverlayError::ReadFailed {
        path: input.to_owned(),
    })?;
    if doc.trailer.has(b"Encrypt") {
        return Err(OverlayError::EncryptedUnsupported {
            path: input.to_owned(),
        });
    }

    let page_ids = doc.get_pages();
    if page_ids.len() != pages.len() {
        return Err(OverlayError::PageCountMismatch {
            path: input.to_owned(),
            provided: pages.len(),
            in_document: page_ids.len(),
        });
    }

    // One font object set per document, shared by every page.
    let font = font::add_glyphless_font(&mut doc, font_bytes)?;

    // `get_pages` numbers pages 1-based in document order; zip pairs the
    // i-th page object with the caller's i-th OCR result.
    for (index, (page_id, overlay_page)) in page_ids.values().zip(pages.iter()).enumerate() {
        let words = layout::layout_page(&overlay_page.ocr, &overlay_page.metadata, index)?;
        if words.is_empty() {
            continue; // Nothing recognised: leave the page untouched.
        }
        let ops = content::overlay_ops(&words);
        let overlay_stream_id = doc.add_object(Stream::new(Dictionary::new(), ops));
        attach_overlay(&mut doc, *page_id, overlay_stream_id, font.type0, index)?;
    }

    doc.save(output)
        .map(|_| ())
        .map_err(|_| OverlayError::WriteFailed {
            path: output.to_owned(),
        })
}

/// Point one page at the overlay content stream and font resource.
fn attach_overlay(
    doc: &mut Document,
    page_id: ObjectId,
    overlay_stream_id: ObjectId,
    font_id: ObjectId,
    page: usize,
) -> Result<(), OverlayError> {
    let update_failed = || OverlayError::ContentUpdateFailed { page };

    // Snapshot the current /Contents value (borrow of the page dictionary
    // ends here) and decide the new value before mutating.
    let contents: Option<Object> = doc
        .get_object_mut(page_id)
        .map_err(|_| update_failed())?
        .as_dict_mut()
        .map_err(|_| update_failed())?
        .get(b"Contents")
        .ok()
        .cloned();

    // Append the overlay stream to the /Contents array, leaving the
    // original stream(s) untouched.
    let new_contents = match contents {
        Some(Object::Reference(original)) => Object::Array(vec![
            Object::Reference(original),
            Object::Reference(overlay_stream_id),
        ]),
        Some(Object::Array(mut existing)) => {
            existing.push(Object::Reference(overlay_stream_id));
            Object::Array(existing)
        }
        // Missing or malformed /Contents: the page shows nothing, so
        // the overlay becomes its whole content.
        _ => Object::Reference(overlay_stream_id),
    };

    doc.get_object_mut(page_id)
        .map_err(|_| update_failed())?
        .as_dict_mut()
        .map_err(|_| update_failed())?
        .set("Contents", new_contents);

    // Register the font under /Resources /Font, normalizing inline
    // dictionaries into indirect objects along the way.
    let resources_id = ensure_indirect_dict(doc, page_id, b"Resources", page)?;
    let font_dict_id = ensure_indirect_dict(doc, resources_id, b"Font", page)?;
    doc.get_object_mut(font_dict_id)
        .map_err(|_| update_failed())?
        .as_dict_mut()
        .map_err(|_| update_failed())?
        .set(content::FONT_RESOURCE_NAME, Object::Reference(font_id));
    Ok(())
}

/// Resolve `key` in the dictionary object `owner` to an indirect reference
/// to a Dictionary, creating or normalizing the object if needed.
///
/// PIL/pdftk-style producers may inline `/Resources << /Font << … >> >>`
/// directly in the page dictionary; mutably walking both levels through
/// lopdf references is far simpler once they are indirect.
fn ensure_indirect_dict(
    doc: &mut Document,
    owner: ObjectId,
    key: &[u8],
    page: usize,
) -> Result<ObjectId, OverlayError> {
    let update_failed = || OverlayError::ContentUpdateFailed { page };

    // Snapshot the current value, ending the mutable borrow before any
    // `add_object` call below.
    let existing: Option<Object> = doc
        .get_object_mut(owner)
        .map_err(|_| update_failed())?
        .as_dict_mut()
        .map_err(|_| update_failed())?
        .get(key)
        .ok()
        .cloned();

    match existing {
        Some(Object::Reference(id)) => {
            // Already indirect; the target must be a dictionary.
            if doc
                .get_object(id)
                .map_err(|_| update_failed())?
                .as_dict()
                .is_ok()
            {
                Ok(id)
            } else {
                Err(update_failed())
            }
        }
        Some(Object::Dictionary(dict)) => {
            // Inline dictionary: move it into its own object and replace
            // the entry with a reference (same semantics, easier to walk).
            let id = doc.add_object(dict);
            set_dict_entry(doc, owner, key, id, page)?;
            Ok(id)
        }
        _ => {
            // Absent (or a non-dictionary value a conforming producer
            // would never write): create an empty dictionary.
            let id = doc.add_object(Dictionary::new());
            set_dict_entry(doc, owner, key, id, page)?;
            Ok(id)
        }
    }
}

/// Point `owner`'s `key` at the object `id`.
fn set_dict_entry(
    doc: &mut Document,
    owner: ObjectId,
    key: &[u8],
    id: ObjectId,
    page: usize,
) -> Result<(), OverlayError> {
    doc.get_object_mut(owner)
        .map_err(|_| OverlayError::ContentUpdateFailed { page })?
        .as_dict_mut()
        .map_err(|_| OverlayError::ContentUpdateFailed { page })?
        .set(key, Object::Reference(id));
    Ok(())
}
