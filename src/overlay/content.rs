//! Overlay content-stream generation: PDF text operators per word.
//!
//! Each word becomes one `BT … ET` block that sets render mode 3
//! (invisible), selects the glyphless font, stretches it to the OCR box
//! with `Tz`, places the baseline with `Tm`, and shows UTF-16BE codes with
//! `Tj`. The whole page's blocks are wrapped in one `q … Q` pair so the
//! overlay can never leak graphics state into the original content.

use super::layout::WordLayout;

/// Name of the glyphless font in page `/Resources` dictionaries.
pub(crate) const FONT_RESOURCE_NAME: &str = "ChilliOcrF1";

/// Render mode 3: neither fill nor stroke (invisible text). Mode 10
/// (clip) would be wrong — the glyphless font has no outlines to add.
const RENDER_MODE_INVISIBLE: u8 = 3;

/// Render the laid-out words as a PDF content stream, wrapped in `q … Q`.
pub(crate) fn overlay_ops(words: &[WordLayout]) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut out = String::from("q\n");
    for word in words {
        let [a, b, c, d, e, f] = word.text_matrix;
        let _ = writeln!(
            out,
            "BT {} Tr /{} {:.2} Tf {:.2} Tz {:.2} {:.2} {:.2} {:.2} {:.2} {:.2} Tm <{}> Tj ET",
            RENDER_MODE_INVISIBLE,
            FONT_RESOURCE_NAME,
            word.font_size,
            word.horizontal_scale,
            a,
            b,
            c,
            d,
            e,
            f,
            word.hex_text,
        );
    }
    out.push_str("Q\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word() -> WordLayout {
        WordLayout {
            font_size: 13.68,
            horizontal_scale: 105.26,
            text_matrix: [0.0, 1.0, -1.0, 0.0, 89.04, 72.0],
            hex_text: "00540068006900730020".into(),
        }
    }

    /// Golden operator string, matching the spike's proven sample
    /// (`docs/spike-report.md`, overlay font technique section).
    #[test]
    fn operators_match_the_spike_sample() {
        let ops = overlay_ops(&[word()]);
        assert_eq!(
            String::from_utf8(ops).unwrap(),
            "q\n\
             BT 3 Tr /ChilliOcrF1 13.68 Tf 105.26 Tz 0.00 1.00 -1.00 0.00 89.04 72.00 \
             Tm <00540068006900730020> Tj ET\n\
             Q\n"
        );
    }

    #[test]
    fn empty_page_produces_bare_state_wrapper() {
        let ops = overlay_ops(&[]);
        assert_eq!(ops, b"q\nQ\n");
    }
}
