//! Recognition-language model shared by the GUI and the OCR pipeline.

/// Supported OCR language presets.
///
/// Each preset maps 1:1 to the Tesseract traineddata files that chilli-pdf
/// bundles for fully offline use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguagePreset {
    /// Modern English (`eng`).
    English,
    /// Hungarian (`hun`).
    Hungarian,
    /// Combined English + Hungarian recognition.
    ///
    /// Tesseract receives both traineddata files and resolves the best
    /// match per word, which is what mixed-language documents need.
    EnglishHungarian,
}

impl LanguagePreset {
    /// Every preset the GUI offers, in display order.
    pub const ALL: [Self; 3] = [Self::English, Self::Hungarian, Self::EnglishHungarian];

    /// Short human-readable label for the language combo box.
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Hungarian => "Hungarian",
            Self::EnglishHungarian => "English + Hungarian",
        }
    }

    /// Tesseract language codes, e.g. `["eng", "hun"]`.
    pub fn tessdata_codes(self) -> &'static [&'static str] {
        match self {
            Self::English => &["eng"],
            Self::Hungarian => &["hun"],
            Self::EnglishHungarian => &["eng", "hun"],
        }
    }

    /// Stable identifier used in saved settings.
    pub fn id(self) -> &'static str {
        match self {
            Self::English => "eng",
            Self::Hungarian => "hun",
            Self::EnglishHungarian => "eng+hun",
        }
    }

    /// Look a preset up by its stable [`Self::id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.id() == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_distinct_and_human_readable() {
        let labels: Vec<_> = LanguagePreset::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(labels.len(), 3);
        assert_eq!(labels, vec!["English", "Hungarian", "English + Hungarian"]);
    }

    #[test]
    fn combined_preset_bundles_both_traineddata_files() {
        assert_eq!(
            LanguagePreset::EnglishHungarian.tessdata_codes(),
            &["eng", "hun"]
        );
    }

    #[test]
    fn ids_round_trip() {
        for preset in LanguagePreset::ALL {
            assert_eq!(LanguagePreset::from_id(preset.id()), Some(preset));
        }
        assert_eq!(
            LanguagePreset::from_id("hun"),
            Some(LanguagePreset::Hungarian)
        );
        assert_eq!(LanguagePreset::from_id("de"), None);
    }
}
