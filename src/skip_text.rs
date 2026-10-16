//! Heuristic for skipping pages that already contain meaningful text.
//!
//! OCR-ing a page that already has a digital text layer produces worse text
//! than the original and wastes time, so the conversion pipeline checks every
//! page first. The rule is deliberately conservative: a page only counts as
//! already searchable when its extracted text clears **both** a word-count
//! and a non-whitespace-character threshold. Anything else — including a
//! single stray character, a page number, or a short watermark — is treated
//! as needing OCR.
//!
//! Known limitation (v1): partially searchable pages (a caption or header on
//! an otherwise scanned page) are out of scope. When in doubt the heuristic
//! OCRs the page, because OCR-ing a mostly-digital page degrades its text
//! far less predictably than skipping OCR on a mostly-scanned page.
//!
//! The actual text extraction is performed by the PDF backend (see the
//! `pdf` module); this module is pure logic over the extracted string, so it
//! is fully unit-testable without any PDF library present.

/// Decision for a single page: run OCR, or leave the page untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTextVerdict {
    /// The page has no meaningful text layer; OCR must run.
    NeedsOcr,
    /// The page already carries a usable text layer; pass it through
    /// untouched (no re-render, no overlay).
    AlreadySearchable,
}

impl PageTextVerdict {
    /// Convenience predicate used by the conversion worker's progress
    /// reporting.
    pub fn needs_ocr(self) -> bool {
        self == Self::NeedsOcr
    }
}

/// Tunable thresholds behind the skip heuristic.
///
/// The defaults are deliberately strict: they let ordinary digital pages
/// (hundreds of characters) pass while stray glyphs, page numbers, and
/// typical one- or two-word watermarks still trigger OCR. The conversion
/// worker may tune these on sample documents; they are runtime parameters,
/// not compile-time constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkipThresholds {
    /// Minimum number of words (runs of alphanumeric characters).
    pub min_words: usize,
    /// Minimum number of non-whitespace, non-control characters.
    pub min_non_whitespace_chars: usize,
}

impl Default for SkipThresholds {
    fn default() -> Self {
        Self {
            min_words: 3,
            min_non_whitespace_chars: 16,
        }
    }
}

/// Classify one page's previously extracted text.
///
/// A page is considered already searchable only when its text contains at
/// least [`SkipThresholds::min_words`] words **and** at least
/// [`SkipThresholds::min_non_whitespace_chars`] non-whitespace characters.
/// Everything else — empty text, a single stray glyph, a page number, a
/// watermark — yields [`PageTextVerdict::NeedsOcr`].
///
/// # Examples
///
/// ```
/// use chilli_pdf::skip_text::{classify_page, PageTextVerdict, SkipThresholds};
///
/// let t = &SkipThresholds::default();
/// // A stray character never marks a page searchable.
/// assert_eq!(classify_page("©", t), PageTextVerdict::NeedsOcr);
/// // A real paragraph does.
/// assert_eq!(
///     classify_page("Csirkepaprikás készülhet szarvasgombával.", t),
///     PageTextVerdict::AlreadySearchable,
/// );
/// ```
pub fn classify_page(text: &str, thresholds: &SkipThresholds) -> PageTextVerdict {
    let non_whitespace_chars = text
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .count();

    if non_whitespace_chars < thresholds.min_non_whitespace_chars {
        return PageTextVerdict::NeedsOcr;
    }

    let words = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .count();

    if words < thresholds.min_words {
        return PageTextVerdict::NeedsOcr;
    }

    PageTextVerdict::AlreadySearchable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn needs_ocr(text: &str) -> bool {
        classify_page(text, &SkipThresholds::default()).needs_ocr()
    }

    #[test]
    fn empty_text_needs_ocr() {
        assert!(needs_ocr(""));
        assert!(needs_ocr("     "));
        assert!(needs_ocr("\n\t\r\n"));
    }

    #[test]
    fn single_stray_character_needs_ocr() {
        assert!(needs_ocr("x"));
        assert!(needs_ocr("©"));
        assert!(needs_ocr("ő"));
    }

    #[test]
    fn page_number_needs_ocr() {
        assert!(needs_ocr("42"));
        assert!(needs_ocr("— 7 —"));
    }

    #[test]
    fn single_word_watermark_needs_ocr() {
        // 12 characters but only one word: still OCR.
        assert!(needs_ocr("Confidential"));
        assert!(needs_ocr("DRAFT"));
    }

    #[test]
    fn repeated_watermark_word_needs_ocr() {
        // Three words, but under the character threshold.
        assert!(needs_ocr("DRAFT DRAFT DRAFT"));
    }

    #[test]
    fn short_caption_without_words_needs_ocr() {
        // 18 non-whitespace chars but zero alphanumeric words.
        assert!(needs_ocr("---------++++++++"));
    }

    #[test]
    fn ordinary_english_paragraph_is_searchable() {
        let text = "The quick brown fox jumps over the lazy dog.";
        assert!(!needs_ocr(text));
    }

    #[test]
    fn hungarian_text_with_special_chars_is_searchable() {
        // ő and ű must round-trip as ordinary letters.
        let text = "Tűzoltó készített szűrőt és megszűrte a fűszert.";
        assert!(!needs_ocr(text));
    }

    #[test]
    fn realistic_digital_page_is_searchable() {
        let text = "\n\tInvoice number: 2024/128\n\nTotal due: 1 250 000 Ft\n";
        assert!(!needs_ocr(text));
    }

    #[test]
    fn whitespace_and_newlines_do_not_count_as_chars() {
        // Same paragraph, spread over many lines — same verdict.
        let flat = "Csirkepaprikás készülhet szarvasgombával.";
        let multiline = "Csirkepaprikás\n\nkészülhet\n\tszarvasgombával.\n";
        assert_eq!(needs_ocr(flat), needs_ocr(multiline));
        assert!(!needs_ocr(multiline));
    }

    #[test]
    fn control_characters_do_not_count_as_chars() {
        // 20 NUL bytes: nothing meaningful, must not pass.
        assert!(needs_ocr(&"\0".repeat(20)));
    }

    #[test]
    fn mixed_document_pages_are_classified_independently() {
        let long_page = "Chapter 1 — Introduction".repeat(20);
        let pages = [long_page.as_str(), "\u{0}", "DRAFT 2026"];
        let verdicts: Vec<_> = pages
            .iter()
            .map(|p| classify_page(p, &SkipThresholds::default()))
            .collect();
        assert_eq!(verdicts[0], PageTextVerdict::AlreadySearchable);
        assert_eq!(verdicts[1], PageTextVerdict::NeedsOcr);
        // A scanned page whose only extractable text is a short watermark
        // still gets OCR'd — exactly the stray-text case the spec calls out.
        assert_eq!(verdicts[2], PageTextVerdict::NeedsOcr);
    }

    #[test]
    fn custom_thresholds_can_tighten_or_loosen() {
        let loose = SkipThresholds {
            min_words: 1,
            min_non_whitespace_chars: 1,
        };
        assert_eq!(
            classify_page("x", &loose),
            PageTextVerdict::AlreadySearchable
        );

        let strict = SkipThresholds {
            min_words: 8,
            min_non_whitespace_chars: 80,
        };
        let text = "A short but real sentence with several words.";
        assert_eq!(classify_page(text, &strict), PageTextVerdict::NeedsOcr);
    }

    #[test]
    fn verdict_predicate_matches_variant() {
        assert!(PageTextVerdict::NeedsOcr.needs_ocr());
        assert!(!PageTextVerdict::AlreadySearchable.needs_ocr());
    }
}
