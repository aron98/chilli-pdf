//! The embedded glyphless Type0 font, built exactly the way Tesseract's
//! searchable-PDF output does it (spike §3, "overlay font technique").
//!
//! The object graph:
//!
//! ```text
//! /ChilliOcrF1 (page /Resources /Font entry)
//!   └─ Type0 /Identity-H ────────────── UTF-16BE codes in, CIDs out
//!        ├─ /DescendantFonts [ CIDFontType2 ]
//!        │      ├─ /DW 500 ───────────── 0.5 em advance per code
//!        │      ├─ /CIDToGIDMap <stream> ─ ALL 65536 CIDs → GID 1,
//!        │      │                           Flate-compressed
//!        │      └─ /FontDescriptor
//!        │             └─ /FontFile2 <stream> ── the glyphless font bytes
//!        └─ /ToUnicode <stream> ─────── identity CMap <0000>–<FFFF>
//! ```
//!
//! Every code maps to the single (outline-less) glyph, so nothing is ever
//! rendered even before render mode 3 hides the text; `Tz` alone controls
//! the advance width, which is what makes the overlay stretch to the OCR
//! boxes.

use super::error::OverlayError;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

/// Base font name for the embedded glyphless font (like Tesseract's own).
const BASE_FONT: &str = "GlyphLessFont";

/// Built font objects: everything a page needs is the Type0 reference.
#[derive(Debug)]
pub(crate) struct GlyphlessFont {
    /// ObjectId of the Type0 font dictionary: the value stored under
    /// `ChilliOcrF1` in page `/Resources` `/Font` dictionaries.
    pub(crate) type0: ObjectId,
}

/// The identity ToUnicode CMap: maps every code <0000>–<FFFF> to itself,
/// so any Unicode text (ő/ű, surrogate pairs, everything in the BMP and
/// beyond via UTF-16) round-trips through copy-paste.
fn to_unicode_cmap() -> Vec<u8> {
    b"/CIDInit /ProcSet findresource begin\n\
      12 dict begin\n\
      begincmap\n\
      /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
      /CMapName /Adobe-Identity-UCS def\n\
      /CMapType 2 def\n\
      1 begincodespacerange\n\
      <0000> <FFFF>\n\
      endcodespacerange\n\
      1 beginbfrange\n\
      <0000> <FFFF> <0000>\n\
      endbfrange\n\
      endcmap\n\
      CMapName currentdict /CMap defineresource pop\n\
      end\n\
      end\n"
        .to_vec()
}

/// The CIDToGIDMap stream content: 65536 big-endian u16 entries, every one
/// GID 1 (the font's single `.null` glyph). Without this map PDF viewers
/// resolve all codes to GID 0, whose advance is zero, and every word
/// collapses onto its anchor (spike §6 pitfall).
fn cid_to_gid_map() -> Vec<u8> {
    let mut map = Vec::with_capacity(65_536 * 2);
    for _ in 0..65_536 {
        map.extend_from_slice(&1_u16.to_be_bytes());
    }
    map
}

/// Add the glyphless font object graph to the document and return the
/// Type0 reference to put in page resources.
///
/// The same set is shared by every page of the document, so this is
/// called exactly once per output file.
pub(crate) fn add_glyphless_font(
    doc: &mut Document,
    font_bytes: &[u8],
) -> Result<GlyphlessFont, OverlayError> {
    if !looks_like_sfnt(font_bytes) {
        return Err(OverlayError::InvalidFont);
    }

    // FontFile2: the TrueType font program verbatim. Never Flate-compress
    // a font program stream — some viewers refuse to decompress /FontFile2.
    let mut font_file_dict = Dictionary::new();
    font_file_dict.set("Length1", font_bytes.len() as i64);
    let font_file = Stream::new(font_file_dict, font_bytes.to_vec()).with_compression(false);
    let font_file_id = doc.add_object(font_file);

    // ToUnicode: the identity CMap (tiny, left uncompressed).
    let to_unicode = Stream::new(Dictionary::new(), to_unicode_cmap());
    let to_unicode_id = doc.add_object(to_unicode);

    // CIDToGIDMap: 128 KiB raw but a few hundred bytes Flate-compressed,
    // exactly the way Tesseract writes it. `Stream::compress` sets the
    // /Filter itself — pre-setting one would make it skip compressing and
    // mislabel the raw bytes.
    let mut cid_to_gid = Stream::new(Dictionary::new(), cid_to_gid_map());
    cid_to_gid
        .compress()
        .map_err(|_| OverlayError::InvalidFont)?;
    let cid_to_gid_id = doc.add_object(cid_to_gid);

    // Font descriptor — byte-for-byte the flags and metrics Tesseract's own
    // PDF writer emits for this font (verified against its output):
    // Ascent 1000 makes the extracted glyph box span exactly
    // [baseline − fs, baseline + 0.001·fs], i.e. the OCR box.
    let mut descriptor = Dictionary::new();
    descriptor.set("Type", "FontDescriptor");
    descriptor.set("FontName", BASE_FONT);
    descriptor.set("Flags", 5_i64);
    descriptor.set(
        "FontBBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(500),
            Object::Integer(1000),
        ]),
    );
    descriptor.set("ItalicAngle", 0_i64);
    descriptor.set("Ascent", 1000_i64);
    descriptor.set("Descent", -1_i64);
    descriptor.set("CapHeight", 1000_i64);
    descriptor.set("StemV", 80_i64);
    descriptor.set("FontFile2", Object::Reference(font_file_id));
    let descriptor_id = doc.add_object(descriptor);

    // CIDFontType2 descendant.
    let mut cid_system_info = Dictionary::new();
    cid_system_info.set("Registry", Object::string_literal("Adobe"));
    cid_system_info.set("Ordering", Object::string_literal("Identity"));
    cid_system_info.set("Supplement", 0_i64);

    let mut descendant = Dictionary::new();
    descendant.set("Type", "Font");
    descendant.set("Subtype", "CIDFontType2");
    descendant.set("BaseFont", BASE_FONT);
    descendant.set("CIDSystemInfo", cid_system_info);
    descendant.set("FontDescriptor", Object::Reference(descriptor_id));
    descendant.set("DW", 500_i64);
    descendant.set("CIDToGIDMap", Object::Reference(cid_to_gid_id));
    let descendant_id = doc.add_object(descendant);

    // Top-level Type0 composite font.
    let mut type0 = Dictionary::new();
    type0.set("Type", "Font");
    type0.set("Subtype", "Type0");
    type0.set("BaseFont", BASE_FONT);
    type0.set("Encoding", "Identity-H");
    type0.set("DescendantFonts", vec![Object::Reference(descendant_id)]);
    type0.set("ToUnicode", Object::Reference(to_unicode_id));
    let type0_id = doc.add_object(type0);

    Ok(GlyphlessFont { type0: type0_id })
}

/// Accept the four sfnt magic values (TrueType, OpenType with CFF, legacy
/// `true`, and font collections). The glyphless font is plain TrueType.
fn looks_like_sfnt(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && (bytes.starts_with(&[0x00, 0x01, 0x00, 0x00])
            || bytes.starts_with(b"OTTO")
            || bytes.starts_with(b"true")
            || bytes.starts_with(b"ttcf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed fixture IS the Tesseract glyphless font; these tests
    /// double as its byte-level documentation.
    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/pdf.ttf");

    #[test]
    fn cid_to_gid_map_covers_all_codes_with_gid_one() {
        let map = cid_to_gid_map();
        assert_eq!(map.len(), 131_072);
        for pair in map.chunks_exact(2) {
            assert_eq!(pair, &[0x00, 0x01]);
        }
    }

    #[test]
    fn to_unicode_cmap_is_the_identity_map() {
        let cmap = String::from_utf8(to_unicode_cmap()).unwrap();
        assert!(cmap.contains("<0000> <FFFF>"));
        assert!(cmap.contains("<0000> <FFFF> <0000>"));
        assert!(cmap.contains("begincodespacerange"));
        assert!(cmap.contains("endbfrange"));
    }

    #[test]
    fn rejects_non_font_bytes() {
        let mut doc = Document::new();
        assert_eq!(
            add_glyphless_font(&mut doc, b"").unwrap_err(),
            OverlayError::InvalidFont
        );
        assert_eq!(
            add_glyphless_font(&mut doc, b"not a font").unwrap_err(),
            OverlayError::InvalidFont
        );
    }

    #[test]
    fn font_graph_matches_the_spike_object_set() {
        let mut doc = Document::new();
        let font_bytes = std::fs::read(FIXTURE).expect("glyphless font fixture");
        let font = add_glyphless_font(&mut doc, &font_bytes).unwrap();

        // Type0 top level.
        let type0 = doc.get_object(font.type0).unwrap().as_dict().unwrap();
        assert_eq!(type0.get(b"Subtype").unwrap().as_name().unwrap(), b"Type0");
        assert_eq!(
            type0.get(b"Encoding").unwrap().as_name().unwrap(),
            b"Identity-H"
        );
        assert_eq!(
            type0.get(b"BaseFont").unwrap().as_name().unwrap(),
            b"GlyphLessFont"
        );

        // Descendant CIDFontType2 with /DW 500 and the CIDToGIDMap stream.
        let descendants = type0.get(b"DescendantFonts").unwrap().as_array().unwrap();
        assert_eq!(descendants.len(), 1);
        let descendant_id = descendants[0].as_reference().unwrap();
        let descendant = doc.get_object(descendant_id).unwrap().as_dict().unwrap();
        assert_eq!(
            descendant.get(b"Subtype").unwrap().as_name().unwrap(),
            b"CIDFontType2"
        );
        assert_eq!(descendant.get(b"DW").unwrap().as_i64().unwrap(), 500);
        let cid_to_gid_id = descendant
            .get(b"CIDToGIDMap")
            .unwrap()
            .as_reference()
            .unwrap();
        let cid_stream = doc.get_object(cid_to_gid_id).unwrap().as_stream().unwrap();
        assert_eq!(
            cid_stream.dict.get(b"Filter").unwrap().as_name().unwrap(),
            b"FlateDecode"
        );
        let cid_map = cid_stream.decompressed_content().unwrap();
        assert_eq!(cid_map.len(), 131_072);
        assert!(cid_map.chunks_exact(2).all(|c| c == [0x00, 0x01]));

        // FontDescriptor with the font program, uncompressed.
        let descriptor_id = descendant
            .get(b"FontDescriptor")
            .unwrap()
            .as_reference()
            .unwrap();
        let descriptor = doc.get_object(descriptor_id).unwrap().as_dict().unwrap();
        assert_eq!(
            descriptor.get(b"FontName").unwrap().as_name().unwrap(),
            b"GlyphLessFont"
        );
        assert_eq!(descriptor.get(b"Flags").unwrap().as_i64().unwrap(), 5);
        assert_eq!(descriptor.get(b"Ascent").unwrap().as_i64().unwrap(), 1000);
        assert_eq!(descriptor.get(b"Descent").unwrap().as_i64().unwrap(), -1);
        let bbox: Vec<i64> = descriptor
            .get(b"FontBBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_i64().unwrap())
            .collect();
        assert_eq!(bbox, vec![0, 0, 500, 1000]);
        let font_file_id = descriptor
            .get(b"FontFile2")
            .unwrap()
            .as_reference()
            .unwrap();
        let font_stream = doc.get_object(font_file_id).unwrap().as_stream().unwrap();
        assert!(
            font_stream.dict.get(b"Filter").is_err(),
            "font program must be raw"
        );
        assert_eq!(
            font_stream.dict.get(b"Length1").unwrap().as_i64().unwrap(),
            i64::try_from(font_bytes.len()).unwrap()
        );
        assert_eq!(font_stream.content, font_bytes);

        // ToUnicode identity CMap.
        let to_unicode_id = type0.get(b"ToUnicode").unwrap().as_reference().unwrap();
        let to_unicode = doc.get_object(to_unicode_id).unwrap().as_stream().unwrap();
        let cmap = String::from_utf8(to_unicode.decompressed_content().unwrap()).unwrap();
        assert!(cmap.contains("<0000> <FFFF> <0000>"));
    }
}
