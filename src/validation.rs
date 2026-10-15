//! Input validation: classify a PDF before any conversion work starts.
//!
//! v1 rejects password-protected (encrypted) PDFs and digitally signed PDFs
//! up front, and fails clearly on malformed files (see
//! `docs/product-requirements.md`). This module implements that gate with
//! hand-rolled byte scanning — deliberately dependency-free and bounded:
//! every scan touches at most a fixed-size window or one pass over an
//! already-in-memory buffer. No scan allocates, indexes unconditionally, or
//! loops on untrusted lengths, so no arbitrary byte buffer can panic or hang.
//!
//! # Classification and priority
//!
//! A document is classified as exactly one of
//! [`Classification::TooLarge`] → [`Classification::Malformed`] →
//! [`Classification::Encrypted`] → [`Classification::DigitallySigned`] →
//! [`Classification::Ok`], checked in that order. When a document is both
//! encrypted and signed the encryption wins, because the user-visible
//! consequence (cannot open it at all) is the more severe.
//!
//! # Heuristic limits (known, accepted for v1)
//!
//! The scanner never parses the file; it looks for well-known tokens:
//!
//! * `/Encrypt` must appear **uncompressed** to be seen. A crafted or merely
//!   object-stream-compressed file can hide its encryption dictionary from
//!   this scan (a PDF 1.5+ `ObjStm` can contain the `/Encrypt` entry
//!   compressed). Files deliberately crafted to look unencrypted while being
//!   encrypted are out of scope for the heuristic pass.
//! * The signature heuristics (`/ByteRange`, `/SigFlags`, `adbe.pkcs7`,
//!   `adbe.x509`, `ETSI.CAdES`) can in principle be evaded the same way, and
//!   could rarely fire on a literal string inside an uncompressed stream.
//!
//! These gaps close when the real PDF engine lands: use
//! [`EngineVerdict`] with [`validate_from_engine`] — the authoritative
//! parser result (PDFium, issue #7) always overrides the byte-scan
//! heuristic. The GUI must prefer the engine hook once available and treat
//! this scan as the pre-flight used until then.
//!
//! # Error messages
//!
//! Every user-facing message is fixed text that never quotes document
//! contents, per the product spec ("Errors must be useful without logging
//! document contents"). Byte sizes (not content) may appear in messages.

use std::fmt;

/// Bytes scanned at the start of a file for the `%PDF-` header.
///
/// Conforming PDFs put the header at byte 0, but readers commonly tolerate
/// junk before it (e.g. e-mail transport headers), so a small bounded
/// window is searched instead of only the first byte.
const HEADER_WINDOW: usize = 1024;

/// Bytes scanned at the end of a file for `startxref` and `%%EOF`.
///
/// `startxref` and the final `%%EOF` belong to the trailer, which by spec is
/// near the end. The window also tolerates trailing junk after `%%EOF`
/// without paying an unbounded scan.
const TAIL_WINDOW: usize = 2048;

/// Default upper bound on accepted input size: 1 GiB.
pub const DEFAULT_MAX_SIZE_BYTES: u64 = 1024 * 1024 * 1024;

/// Tunables for validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationConfig {
    /// Inputs strictly larger than this (in bytes) are rejected with
    /// [`Classification::TooLarge`] before any content is inspected.
    pub max_size_bytes: u64,
}

impl ValidationConfig {
    /// New config with the given size cap and everything else at defaults.
    pub fn with_max_size_bytes(max_size_bytes: u64) -> Self {
        Self { max_size_bytes }
    }
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: DEFAULT_MAX_SIZE_BYTES,
        }
    }
}

/// What a validation pass concluded about one input document.
///
/// Exactly one variant per document; see the [module docs](self) for the
/// priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// Acceptable input; proceed to conversion.
    Ok,
    /// Password-protected / encrypted document — v1 rejects it.
    Encrypted,
    /// Digitally signed document — v1 rejects it (edits would invalidate
    /// signatures).
    DigitallySigned,
    /// Not a structurally plausible PDF: missing header, trailer, or
    /// end-of-file marker.
    Malformed,
    /// Larger than the configured [`ValidationConfig::max_size_bytes`].
    TooLarge,
}

/// Validation failure carrying a ready-to-show, content-free message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Document is password-protected (encrypted).
    Encrypted,
    /// Document carries a digital signature.
    DigitallySigned,
    /// Document is not structurally plausible as a PDF.
    Malformed,
    /// Document exceeds the configured size cap.
    TooLarge {
        /// Actual size in bytes (never document content).
        actual_bytes: u64,
        /// Configured cap in bytes.
        limit_bytes: u64,
    },
}

impl ValidationError {
    /// User-facing message. Guaranteed to contain no document contents —
    /// only fixed explanations and, where relevant, byte sizes.
    pub fn user_message(&self) -> String {
        match self {
            Self::Encrypted => concat!(
                "This PDF is password-protected (encrypted), which chilli-pdf ",
                "cannot open yet. Remove the password with another PDF tool, ",
                "save an unprotected copy, and try again."
            )
            .to_owned(),
            Self::DigitallySigned => concat!(
                "This PDF is digitally signed. Adding a searchable text layer ",
                "would invalidate the signature, so chilli-pdf does not modify ",
                "signed documents. Ask for an unsigned copy, or remove the ",
                "signature yourself, and try again."
            )
            .to_owned(),
            Self::Malformed => concat!(
                "This file does not look like a valid PDF — it may be corrupted, ",
                "truncated, or not a PDF at all. Re-save or re-export the document ",
                "as a PDF and try again."
            )
            .to_owned(),
            Self::TooLarge {
                actual_bytes,
                limit_bytes,
            } => format!(
                "This PDF is {actual_bytes} bytes, larger than the supported \
                 limit of {limit_bytes} bytes. Convert a smaller document, or \
                 split this one first."
            ),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for ValidationError {}

/// What the real PDF engine (PDFium via issue #7) concluded about a
/// document it fully parsed. Feed this into [`validate_from_engine`].
///
/// This is the validation hook that replaces the byte-scan heuristics once
/// the engine lands: engine results are authoritative and cannot be fooled
/// by compression or crafted structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineVerdict {
    /// The document opened and parsed cleanly, with no password required.
    Clean,
    /// The engine confirmed encryption / password protection.
    Encrypted,
    /// The engine confirmed at least one digital signature.
    Signed,
    /// The engine failed to load or parse the document at all. The
    /// underlying reason stays internal (never surfaced to the user).
    Failed,
}

/// Pre-flight the file size before reading a document into memory.
///
/// The GUI should call this with the on-disk file length so oversized
/// inputs are rejected without ever being loaded.
pub fn validate_size(size_bytes: u64, config: &ValidationConfig) -> Result<(), ValidationError> {
    if size_bytes > config.max_size_bytes {
        Err(ValidationError::TooLarge {
            actual_bytes: size_bytes,
            limit_bytes: config.max_size_bytes,
        })
    } else {
        Ok(())
    }
}

/// Classify an in-memory PDF buffer. Never panics on arbitrary bytes.
pub fn classify_bytes(bytes: &[u8], config: &ValidationConfig) -> Classification {
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > config.max_size_bytes {
        return Classification::TooLarge;
    }
    if !has_pdf_header(bytes) || !has_pdf_tail(bytes) {
        return Classification::Malformed;
    }
    if contains_token(bytes, b"/Encrypt") {
        return Classification::Encrypted;
    }
    if looks_signed(bytes) {
        return Classification::DigitallySigned;
    }
    Classification::Ok
}

/// Validate an in-memory PDF buffer, mapping any non-Ok classification to
/// a user-facing error. Never panics on arbitrary bytes.
pub fn validate_bytes(bytes: &[u8], config: &ValidationConfig) -> Result<(), ValidationError> {
    match classify_bytes(bytes, config) {
        Classification::Ok => Ok(()),
        Classification::Encrypted => Err(ValidationError::Encrypted),
        Classification::DigitallySigned => Err(ValidationError::DigitallySigned),
        Classification::Malformed => Err(ValidationError::Malformed),
        Classification::TooLarge => Err(ValidationError::TooLarge {
            actual_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            limit_bytes: config.max_size_bytes,
        }),
    }
}

/// Validation hook for the real PDF engine (issue #7): map an
/// authoritative parse result to the same user-facing errors the byte
/// scan produces, with the size cap still enforced here.
///
/// Once the engine is wired in, prefer this over [`validate_bytes`]: it
/// cannot be fooled by compressed or crafted structure.
pub fn validate_from_engine(
    verdict: EngineVerdict,
    size_bytes: u64,
    config: &ValidationConfig,
) -> Result<(), ValidationError> {
    validate_size(size_bytes, config)?;
    match verdict {
        EngineVerdict::Clean => Ok(()),
        EngineVerdict::Encrypted => Err(ValidationError::Encrypted),
        EngineVerdict::Signed => Err(ValidationError::DigitallySigned),
        EngineVerdict::Failed => Err(ValidationError::Malformed),
    }
}

/// True if the buffer starts (within [`HEADER_WINDOW`]) with a `%PDF-`
/// header followed by at least one ASCII version digit.
fn has_pdf_header(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(HEADER_WINDOW)];
    window
        .windows(5)
        .position(|candidate| candidate == b"%PDF-")
        .is_some_and(|offset| {
            window
                .get(offset + 5)
                .is_some_and(|byte| byte.is_ascii_digit())
        })
}

/// True if the buffer ends (within [`TAIL_WINDOW`]) with a trailer
/// containing both `startxref` and `%%EOF`, the two structural markers
/// every conforming PDF (classic xref tables and xref streams alike)
/// carries near the end.
fn has_pdf_tail(bytes: &[u8]) -> bool {
    let start = bytes.len().saturating_sub(TAIL_WINDOW);
    let window = &bytes[start..];
    contains_token(window, b"startxref") && contains_token(window, b"%%EOF")
}

/// Bounded token search: scans `haystack` for `needle` in one pass.
///
/// `needle` is always a fixed literal from this module, so the scan cost is
/// bounded by the (already-in-memory) buffer length and can never loop on
/// attacker-controlled lengths.
fn contains_token(haystack: &[u8], needle: &[u8]) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|candidate| candidate == needle)
}

/// Signature heuristics: any of the well-known tokens that signature
/// dictionaries, `/SigFlags` in interactive forms, and PKCS signature
/// payload formats leave behind in uncompressed regions.
fn looks_signed(bytes: &[u8]) -> bool {
    let tokens: [&[u8]; 5] = [
        b"/ByteRange",
        b"/SigFlags",
        b"adbe.pkcs7",
        b"adbe.x509",
        b"ETSI.CAdES",
    ];
    tokens.iter().any(|token| contains_token(bytes, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-written minimal valid PDF (structure only; the scanner does
    /// not resolve offsets).
    fn minimal_pdf() -> Vec<u8> {
        b"%PDF-1.4\n\
          1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
          2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
          xref\n0 1\n0000000000 65535 f\n\
          trailer\n<< /Size 1 /Root 1 0 R >>\n\
          startxref\n9\n\
          %%EOF\n"
            .to_vec()
    }

    /// Minimal PDF whose trailer carries an encryption dictionary entry.
    fn encrypted_pdf() -> Vec<u8> {
        b"%PDF-1.4\n\
          1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
          2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
          xref\n0 1\n0000000000 65535 f\n\
          trailer\n<< /Size 1 /Root 1 0 R /Encrypt 5 0 R >>\n\
          startxref\n9\n\
          %%EOF\n"
            .to_vec()
    }

    /// Minimal PDF whose trailer carries an AcroForm signature flag.
    fn signed_pdf() -> Vec<u8> {
        b"%PDF-1.4\n\
          1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
          2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
          xref\n0 1\n0000000000 65535 f\n\
          trailer\n<< /Size 1 /Root 1 0 R /SigFlags 3 >>\n\
          startxref\n9\n\
          %%EOF\n"
            .to_vec()
    }

    fn truncated_header_only() -> Vec<u8> {
        b"%PDF-1.7 this file was cut off mid-download".to_vec()
    }

    fn cfg() -> ValidationConfig {
        ValidationConfig::default()
    }

    #[test]
    fn minimal_valid_pdf_is_accepted() {
        assert_eq!(classify_bytes(&minimal_pdf(), &cfg()), Classification::Ok);
        assert_eq!(validate_bytes(&minimal_pdf(), &cfg()), Ok(()));
    }

    #[test]
    fn encrypted_pdf_is_detected_and_rejected() {
        assert_eq!(
            classify_bytes(&encrypted_pdf(), &cfg()),
            Classification::Encrypted
        );
        let err = validate_bytes(&encrypted_pdf(), &cfg()).unwrap_err();
        assert_eq!(err, ValidationError::Encrypted);
        assert!(err.user_message().to_lowercase().contains("password"));
    }

    #[test]
    fn signed_pdf_is_detected_and_rejected() {
        assert_eq!(
            classify_bytes(&signed_pdf(), &cfg()),
            Classification::DigitallySigned
        );
        let err = validate_bytes(&signed_pdf(), &cfg()).unwrap_err();
        assert_eq!(err, ValidationError::DigitallySigned);
        assert!(err.user_message().to_lowercase().contains("signature"));
    }

    #[test]
    fn signature_tokens_alone_trigger_detection() {
        for token in [
            b"/ByteRange [0 10 20 5]".as_slice(),
            b"/SigFlags 1".as_slice(),
            b"/SubFilter /adbe.pkcs7.detached".as_slice(),
            b"(ETSI.CAdES.detached)".as_slice(),
        ] {
            let mut pdf = minimal_pdf();
            pdf.extend_from_slice(token);
            assert_eq!(
                classify_bytes(&pdf, &cfg()),
                Classification::DigitallySigned,
                "token {token:?} must be detected"
            );
        }
    }

    #[test]
    fn plain_text_and_empty_buffers_are_malformed() {
        assert_eq!(
            classify_bytes(b"hello world, definitely not a pdf", &cfg()),
            Classification::Malformed
        );
        assert_eq!(classify_bytes(b"", &cfg()), Classification::Malformed);
        assert_eq!(classify_bytes(b"%PDF-", &cfg()), Classification::Malformed);
    }

    #[test]
    fn truncated_pdf_is_malformed() {
        let full = minimal_pdf();
        // Cut before %%EOF can land inside the tail window.
        let cut = &full[..full.len() - 3];
        assert_eq!(classify_bytes(cut, &cfg()), Classification::Malformed);
        assert_eq!(
            validate_bytes(truncated_header_only().as_slice(), &cfg()).unwrap_err(),
            ValidationError::Malformed
        );
    }

    #[test]
    fn header_junk_before_pdf_marker_is_tolerated_within_window() {
        let junk = vec![b'x'; 512];
        let mut pdf = junk;
        pdf.extend_from_slice(&minimal_pdf());
        assert_eq!(classify_bytes(&pdf, &cfg()), Classification::Ok);
    }

    #[test]
    fn oversized_input_is_rejected_before_any_parsing() {
        let config = ValidationConfig { max_size_bytes: 10 };
        let big = vec![0u8; 11];
        assert_eq!(classify_bytes(&big, &config), Classification::TooLarge);
        let err = validate_bytes(&big, &config).unwrap_err();
        assert_eq!(
            err,
            ValidationError::TooLarge {
                actual_bytes: 11,
                limit_bytes: 10
            }
        );
        assert!(err.user_message().contains("11 bytes"));
    }

    #[test]
    fn validate_size_rejects_oversized_files_before_reading() {
        let config = ValidationConfig {
            max_size_bytes: 100,
        };
        assert_eq!(validate_size(100, &config), Ok(()));
        assert_eq!(
            validate_size(101, &config).unwrap_err(),
            ValidationError::TooLarge {
                actual_bytes: 101,
                limit_bytes: 100,
            }
        );
    }

    #[test]
    fn error_messages_never_leak_document_contents() {
        const SECRET: &str = "TOP-SECRET-DOCUMENT-CONTENT-987654321";
        // Every variant's message is fixed text: the secret embedded in
        // the document must never be able to reach the user.
        let variants = [
            ValidationError::Encrypted,
            ValidationError::DigitallySigned,
            ValidationError::Malformed,
            ValidationError::TooLarge {
                actual_bytes: 11,
                limit_bytes: 10,
            },
        ];
        for err in variants {
            assert!(!err.user_message().contains(SECRET), "{err:?}");
        }
        // The error produced for a buffer that embeds the secret carries
        // only the byte counts, never the bytes themselves.
        let mut pdf = minimal_pdf();
        pdf.extend_from_slice(SECRET.as_bytes());
        let err = validate_bytes(&pdf, &ValidationConfig { max_size_bytes: 10 }).unwrap_err();
        assert!(
            matches!(err, ValidationError::TooLarge { .. }),
            "oversized fixture must classify as TooLarge"
        );
        assert!(!err.user_message().contains(SECRET));
        assert!(!err.user_message().contains("%PDF-1.4"));
    }

    #[test]
    fn engine_hook_maps_parser_verdicts_to_the_shared_errors() {
        let config = cfg();
        assert_eq!(
            validate_from_engine(EngineVerdict::Clean, 10, &config),
            Ok(())
        );
        assert_eq!(
            validate_from_engine(EngineVerdict::Encrypted, 10, &config).unwrap_err(),
            ValidationError::Encrypted
        );
        assert_eq!(
            validate_from_engine(EngineVerdict::Signed, 10, &config).unwrap_err(),
            ValidationError::DigitallySigned
        );
        assert_eq!(
            validate_from_engine(EngineVerdict::Failed, 10, &config).unwrap_err(),
            ValidationError::Malformed
        );
        // Engine results still respect the size cap.
        assert_eq!(
            validate_from_engine(
                EngineVerdict::Clean,
                11,
                &ValidationConfig { max_size_bytes: 10 }
            )
            .unwrap_err(),
            ValidationError::TooLarge {
                actual_bytes: 11,
                limit_bytes: 10
            }
        );
    }

    #[test]
    fn error_type_implements_display_and_error() {
        let err: Box<dyn std::error::Error> = Box::new(ValidationError::Malformed);
        assert!(!err.to_string().is_empty());
    }

    /// Deterministic, dependency-free PRNG for property tests.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }

        fn next_u64(&mut self) -> u64 {
            // splitmix64
            self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        }

        fn byte(&mut self) -> u8 {
            self.next_u64() as u8
        }

        fn buffer(&mut self, len: usize) -> Vec<u8> {
            (0..len).map(|_| self.byte()).collect()
        }
    }

    #[test]
    fn random_garbage_never_panics_or_hangs() {
        let mut rng = Rng::new(0xC0FFEE);
        for _ in 0..256 {
            let len = (rng.next_u64() % 4097) as usize;
            let buffer = rng.buffer(len);
            // Any return value is fine; the property is that this call
            // always returns, promptly, for arbitrary bytes.
            let _ = classify_bytes(&buffer, &cfg());
            let _ = validate_bytes(&buffer, &cfg());
        }
    }

    #[test]
    fn truncated_fixtures_never_panic() {
        let fixtures = [
            minimal_pdf(),
            encrypted_pdf(),
            signed_pdf(),
            truncated_header_only(),
            vec![b'%'; 300],
        ];
        let mut rng = Rng::new(0xBADC0DE);
        for fixture in fixtures {
            for len in (0..=fixture.len()).step_by(3) {
                let _ = classify_bytes(&fixture[..len], &cfg());
            }
            // Also truncate at a couple of pseudo-random boundaries.
            for _ in 0..32 {
                let len = (rng.next_u64() as usize) % (fixture.len() + 1);
                let _ = validate_bytes(&fixture[..len], &cfg());
            }
        }
    }

    #[test]
    fn mutated_fixtures_never_panic() {
        let fixtures = [
            minimal_pdf(),
            encrypted_pdf(),
            signed_pdf(),
            truncated_header_only(),
        ];
        let mut rng = Rng::new(0xFEED_FACE);
        for fixture in fixtures {
            let mut mutated = fixture.clone();
            for _ in 0..128 {
                let position = (rng.next_u64() as usize) % mutated.len().max(1);
                let byte = rng.byte();
                if let Some(slot) = mutated.get_mut(position) {
                    *slot = byte;
                }
                let _ = classify_bytes(&mutated, &cfg());
            }
        }
    }

    #[test]
    fn every_random_buffer_yields_exactly_one_classification() {
        let mut rng = Rng::new(0x5EED_5EED);
        for _ in 0..128 {
            let len = (rng.next_u64() % 1024) as usize;
            let buffer = rng.buffer(len);
            let classification = classify_bytes(&buffer, &cfg());
            // Exactly-one-of is enforced by the enum; also check the
            // same input always classifies identically (determinism).
            assert_eq!(classify_bytes(&buffer, &cfg()), classification);
        }
    }
}
