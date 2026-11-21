//! Linux backend of rf_win::look::pdf. Status: STUB

use crate::thumbs::Bgra;
use leaf_common::{stub, stub_err};
use std::path::Path;

// TYPE-COPY(rf-win look/pdf.rs): the PDFium document dropped
pub struct Pdf {
    _p: (),
}

impl Pdf {
    pub fn open(_path: &Path, _password: Option<&str>) -> anyhow::Result<Pdf> {
        stub_err!("look::pdf::Pdf::open")
    }
    pub fn page_count(&self) -> u32 {
        stub!("look::pdf::Pdf::page_count")
    }
    pub fn page_size(&self, _index: u32) -> anyhow::Result<(f32, f32)> {
        stub_err!("look::pdf::Pdf::page_size")
    }
    pub fn render(&self, _index: u32, _width: u32) -> anyhow::Result<Bgra> {
        stub_err!("look::pdf::Pdf::render")
    }
}

// TYPE-COPY(rf-win src/look/pdf.rs:26)
#[derive(Debug, PartialEq, Eq)]
pub enum PdfError {
    Password,
    Unreadable,
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PdfError::Password => "the PDF is protected with a password",
            PdfError::Unreadable => "the file can't be read as a PDF",
        })
    }
}

impl std::error::Error for PdfError {}
