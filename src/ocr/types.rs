//! Structured OCR output.

/// Axis-aligned bounding box in page pixels.
///
/// `left`/`top` is the top-left corner; `right`/`bottom` the bottom-right, in
/// the same pixel coordinate space as the input [`super::OcrImage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }
}

/// A single recognised word.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    /// The word text, exactly as Tesseract decoded it (UTF-8).
    pub text: String,
    /// Recognition confidence in the range `0.0..=100.0`.
    pub confidence: f32,
    /// Bounding box of the word, in page pixels.
    pub rect: Rect,
}

/// One recognised text line.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    /// Bounding box of the whole line, in page pixels.
    pub rect: Rect,
    /// Words on this line, in reading order.
    pub words: Vec<OcrWord>,
}

impl OcrLine {
    /// Reconstruct the line text by joining its words with single spaces.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, word) in self.words.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&word.text);
        }
        out
    }
}

/// All recognised text on one page.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrPage {
    /// Page width in pixels (matches the input image).
    pub width: u32,
    /// Page height in pixels (matches the input image).
    pub height: u32,
    /// Resolution the page was recognised at, in dots per inch.
    pub dpi: u32,
    /// Recognised lines, in reading order.
    pub lines: Vec<OcrLine>,
}

impl OcrPage {
    /// An empty page result with the given dimensions.
    pub const fn empty(width: u32, height: u32, dpi: u32) -> Self {
        Self {
            width,
            height,
            dpi,
            lines: Vec::new(),
        }
    }

    /// `true` when no words were recognised.
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|line| line.words.is_empty())
    }

    /// Iterate over every word on the page, in reading order.
    pub fn words(&self) -> impl Iterator<Item = &OcrWord> {
        self.lines.iter().flat_map(|line| line.words.iter())
    }

    /// Reconstruct the full page text: words joined by spaces, lines by `\n`.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&line.text());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, l: i32, t: i32, r: i32, b: i32) -> OcrWord {
        OcrWord {
            text: text.into(),
            confidence: 90.0,
            rect: Rect::new(l, t, r, b),
        }
    }

    #[test]
    fn rect_dimensions() {
        let r = Rect::new(10, 20, 30, 50);
        assert_eq!(r.width(), 20);
        assert_eq!(r.height(), 30);
    }

    #[test]
    fn page_text_joins_words_and_lines() {
        let page = OcrPage {
            width: 100,
            height: 50,
            dpi: 300,
            lines: vec![
                OcrLine {
                    rect: Rect::new(0, 0, 50, 10),
                    words: vec![word("hello", 0, 0, 25, 10), word("world", 26, 0, 50, 10)],
                },
                OcrLine {
                    rect: Rect::new(0, 11, 30, 20),
                    words: vec![word("again", 0, 11, 30, 20)],
                },
            ],
        };
        assert_eq!(page.text(), "hello world\nagain");
        assert_eq!(page.words().count(), 3);
        assert!(!page.is_empty());
    }

    #[test]
    fn empty_page() {
        let page = OcrPage::empty(10, 10, 300);
        assert!(page.is_empty());
        assert_eq!(page.text(), "");
    }
}
