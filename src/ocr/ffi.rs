//! Hand-rolled `libtesseract` C-API binding, loaded at runtime.
//!
//! Only the handful of flat C functions chilli-pdf needs are bound — see the
//! [module docs](super) for why a hand-rolled binding was chosen over the
//! binding crates. Leptonica never needs binding because
//! [`TessBaseAPISetImage`](https://github.com/tesseract-ocr/tesseract/blob/main/include/tesseract/capi.h)
//! accepts raw pixel bytes directly.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

use libloading::Library;

use super::error::OcrError;
use super::types::Rect;

/// Opaque `TessBaseAPI*` handle.
type ApiHandle = *mut c_void;
/// Opaque `TessResultIterator*` handle. In the C API the result iterator
/// *is* a page iterator (C++ inheritance), so it is accepted by every
/// `TessPageIterator*` function too.
type IteratorHandle = *mut c_void;

/// `TessPageIteratorLevel::RIL_TEXTLINE`.
const RIL_TEXTLINE: c_int = 2;
/// `TessPageIteratorLevel::RIL_WORD`.
const RIL_WORD: c_int = 3;
/// `TessPageSegMode::PSM_AUTO` — fully automatic page segmentation.
const PSM_AUTO: c_int = 3;

/// Resolved entry points of a loaded `libtesseract`.
///
/// The loaded [`Library`] handles are kept alive in the final field so the
/// function pointers remain valid for the lifetime of the struct (fields drop
/// in declaration order, so the libraries are released last).
struct Api {
    create: unsafe extern "C" fn() -> ApiHandle,
    delete: unsafe extern "C" fn(ApiHandle),
    init: unsafe extern "C" fn(ApiHandle, *const c_char, *const c_char) -> c_int,
    set_image: unsafe extern "C" fn(ApiHandle, *const u8, c_int, c_int, c_int, c_int),
    set_source_resolution: unsafe extern "C" fn(ApiHandle, c_int),
    set_page_seg_mode: unsafe extern "C" fn(ApiHandle, c_int),
    recognize: unsafe extern "C" fn(ApiHandle, *mut c_void) -> c_int,
    get_iterator: unsafe extern "C" fn(ApiHandle) -> IteratorHandle,
    clear: unsafe extern "C" fn(ApiHandle),
    end: unsafe extern "C" fn(ApiHandle),
    result_iterator_delete: unsafe extern "C" fn(IteratorHandle),
    result_text: unsafe extern "C" fn(IteratorHandle, c_int) -> *mut c_char,
    delete_text: unsafe extern "C" fn(*mut c_char),
    confidence: unsafe extern "C" fn(IteratorHandle, c_int) -> f32,
    next: unsafe extern "C" fn(IteratorHandle, c_int) -> c_int,
    bounding_box: unsafe extern "C" fn(
        IteratorHandle,
        c_int,
        *mut c_int,
        *mut c_int,
        *mut c_int,
        *mut c_int,
    ) -> c_int,
    is_at_beginning_of: unsafe extern "C" fn(IteratorHandle, c_int) -> c_int,
    libraries: Vec<Library>,
}

/// A live Tesseract instance: the loaded library plus one `TessBaseAPI`.
pub(crate) struct TesseractApi {
    api: Api,
    handle: ApiHandle,
}

impl TesseractApi {
    /// Load `library_paths` (dependencies first), then create and initialise
    /// a Tesseract handle for `language` using traineddata from `tessdata_dir`.
    pub(crate) fn open(
        library_paths: &[PathBuf],
        tessdata_dir: &Path,
        language: &str,
    ) -> Result<Self, OcrError> {
        let api = load(library_paths)?;
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err(OcrError::InitialisationFailed {
                language: language.to_owned(),
            });
        }
        let mut tesseract = Self { api, handle };
        // Tesseract's datapath convention is a directory ending in `/`.
        let datapath = c_string(&format!("{}/", tessdata_dir.to_string_lossy()))?;
        let language_c = c_string(language)?;
        if unsafe { (tesseract.api.init)(tesseract.handle, datapath.as_ptr(), language_c.as_ptr()) }
            != 0
        {
            return Err(OcrError::InitialisationFailed {
                language: language.to_owned(),
            });
        }
        unsafe { (tesseract.api.set_page_seg_mode)(tesseract.handle, PSM_AUTO) };
        Ok(tesseract)
    }

    pub(crate) fn set_image(
        &self,
        data: &[u8],
        width: c_int,
        height: c_int,
        bpp: c_int,
        bpl: c_int,
    ) {
        unsafe { (self.api.set_image)(self.handle, data.as_ptr(), width, height, bpp, bpl) };
    }

    pub(crate) fn set_source_resolution(&self, dpi: c_int) {
        unsafe { (self.api.set_source_resolution)(self.handle, dpi) };
    }

    pub(crate) fn recognize(&self) -> c_int {
        unsafe { (self.api.recognize)(self.handle, ptr::null_mut()) }
    }

    pub(crate) fn clear(&self) {
        unsafe { (self.api.clear)(self.handle) };
    }

    pub(crate) fn iterator(&self) -> Option<ResultIterator<'_>> {
        let handle = unsafe { (self.api.get_iterator)(self.handle) };
        if handle.is_null() {
            None
        } else {
            Some(ResultIterator {
                api: &self.api,
                handle,
            })
        }
    }
}

impl Drop for TesseractApi {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // End releases internal models; Delete frees the handle. Calling
            // End on a handle whose Init failed is safe (it is a guarded no-op).
            unsafe {
                (self.api.end)(self.handle);
                (self.api.delete)(self.handle);
            }
        }
    }
}

/// Borrowed view over a Tesseract result iterator.
pub(crate) struct ResultIterator<'a> {
    api: &'a Api,
    handle: IteratorHandle,
}

impl ResultIterator<'_> {
    /// Is the cursor at the first element of a new text line?
    pub(crate) fn starts_new_line(&self) -> bool {
        unsafe { (self.api.is_at_beginning_of)(self.handle, RIL_TEXTLINE) != 0 }
    }

    /// Advance to the next word. Returns `false` at the end of the page.
    pub(crate) fn next_word(&self) -> bool {
        unsafe { (self.api.next)(self.handle, RIL_WORD) != 0 }
    }

    /// UTF-8 text of the current word (empty if Tesseract produced none).
    pub(crate) fn word_text(&self) -> Result<String, OcrError> {
        let ptr = unsafe { (self.api.result_text)(self.handle, RIL_WORD) };
        if ptr.is_null() {
            return Ok(String::new());
        }
        let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
        let text = std::str::from_utf8(bytes)
            .map(String::from)
            .map_err(|_| OcrError::InvalidText)?;
        unsafe { (self.api.delete_text)(ptr) };
        Ok(text)
    }

    pub(crate) fn word_confidence(&self) -> f32 {
        unsafe { (self.api.confidence)(self.handle, RIL_WORD) }
    }

    pub(crate) fn word_box(&self) -> Option<Rect> {
        self.box_at(RIL_WORD)
    }

    pub(crate) fn line_box(&self) -> Option<Rect> {
        self.box_at(RIL_TEXTLINE)
    }

    fn box_at(&self, level: c_int) -> Option<Rect> {
        let (mut left, mut top, mut right, mut bottom) = (0, 0, 0, 0);
        let ok = unsafe {
            (self.api.bounding_box)(
                self.handle,
                level,
                &mut left,
                &mut top,
                &mut right,
                &mut bottom,
            )
        };
        if ok == 0 {
            None
        } else {
            Some(Rect::new(left, top, right, bottom))
        }
    }
}

impl Drop for ResultIterator<'_> {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { (self.api.result_iterator_delete)(self.handle) };
        }
    }
}

fn load(library_paths: &[PathBuf]) -> Result<Api, OcrError> {
    if library_paths.is_empty() {
        return Err(OcrError::LibraryNotFound {
            searched: Vec::new(),
        });
    }
    let mut libraries = Vec::with_capacity(library_paths.len());
    for path in library_paths {
        let library = unsafe { Library::new(path) }.map_err(|err| OcrError::LibraryLoad {
            path: path.clone(),
            message: err.to_string(),
        })?;
        libraries.push(library);
    }
    // The last entry is libtesseract itself; earlier entries are its
    // dependencies (e.g. libleptonica), preloaded so a bundled copy can be
    // resolved when it is not on the system loader path.
    let tesseract = libraries.last().expect("library list is non-empty");
    Ok(Api {
        create: symbol::<unsafe extern "C" fn() -> ApiHandle>(tesseract, b"TessBaseAPICreate\0")?,
        delete: symbol::<unsafe extern "C" fn(ApiHandle)>(tesseract, b"TessBaseAPIDelete\0")?,
        init: symbol::<unsafe extern "C" fn(ApiHandle, *const c_char, *const c_char) -> c_int>(
            tesseract,
            b"TessBaseAPIInit3\0",
        )?,
        set_image: symbol::<unsafe extern "C" fn(ApiHandle, *const u8, c_int, c_int, c_int, c_int)>(
            tesseract,
            b"TessBaseAPISetImage\0",
        )?,
        set_source_resolution: symbol::<unsafe extern "C" fn(ApiHandle, c_int)>(
            tesseract,
            b"TessBaseAPISetSourceResolution\0",
        )?,
        set_page_seg_mode: symbol::<unsafe extern "C" fn(ApiHandle, c_int)>(
            tesseract,
            b"TessBaseAPISetPageSegMode\0",
        )?,
        recognize: symbol::<unsafe extern "C" fn(ApiHandle, *mut c_void) -> c_int>(
            tesseract,
            b"TessBaseAPIRecognize\0",
        )?,
        get_iterator: symbol::<unsafe extern "C" fn(ApiHandle) -> IteratorHandle>(
            tesseract,
            b"TessBaseAPIGetIterator\0",
        )?,
        clear: symbol::<unsafe extern "C" fn(ApiHandle)>(tesseract, b"TessBaseAPIClear\0")?,
        end: symbol::<unsafe extern "C" fn(ApiHandle)>(tesseract, b"TessBaseAPIEnd\0")?,
        result_iterator_delete: symbol::<unsafe extern "C" fn(IteratorHandle)>(
            tesseract,
            b"TessResultIteratorDelete\0",
        )?,
        result_text: symbol::<unsafe extern "C" fn(IteratorHandle, c_int) -> *mut c_char>(
            tesseract,
            b"TessResultIteratorGetUTF8Text\0",
        )?,
        delete_text: symbol::<unsafe extern "C" fn(*mut c_char)>(tesseract, b"TessDeleteText\0")?,
        confidence: symbol::<unsafe extern "C" fn(IteratorHandle, c_int) -> f32>(
            tesseract,
            b"TessResultIteratorConfidence\0",
        )?,
        next: symbol::<unsafe extern "C" fn(IteratorHandle, c_int) -> c_int>(
            tesseract,
            b"TessResultIteratorNext\0",
        )?,
        bounding_box: symbol::<
            unsafe extern "C" fn(
                IteratorHandle,
                c_int,
                *mut c_int,
                *mut c_int,
                *mut c_int,
                *mut c_int,
            ) -> c_int,
        >(tesseract, b"TessPageIteratorBoundingBox\0")?,
        is_at_beginning_of: symbol::<unsafe extern "C" fn(IteratorHandle, c_int) -> c_int>(
            tesseract,
            b"TessPageIteratorIsAtBeginningOf\0",
        )?,
        libraries,
    })
}

/// Resolve one C symbol, with a content-free error when it is missing.
fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, OcrError> {
    let symbol = unsafe { library.get::<T>(name) }.map_err(|_| OcrError::MissingSymbol {
        symbol: symbol_name(name),
    })?;
    Ok(*symbol)
}

fn symbol_name(name: &[u8]) -> &'static str {
    let end = name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len());
    std::str::from_utf8(&name[..end]).unwrap_or("<unknown>")
}

fn c_string(text: &str) -> Result<CString, OcrError> {
    CString::new(text).map_err(|_| OcrError::InvalidPath {
        value: text.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_library_list_is_reported() {
        let err = load(&[]).unwrap_err();
        assert!(matches!(err, OcrError::LibraryNotFound { .. }), "{err:?}");
    }

    #[test]
    fn missing_library_file_is_reported() {
        let path = PathBuf::from("/nonexistent/chilli-pdf-libtesseract-test.so");
        let err = load(std::slice::from_ref(&path)).unwrap_err();
        assert!(
            matches!(err, OcrError::LibraryLoad { ref path, .. } if path == &PathBuf::from("/nonexistent/chilli-pdf-libtesseract-test.so")),
            "{err:?}"
        );
    }
}
