//! Linux backend of rf_win::look::pdf. Status: REAL when poppler's `pdfinfo` and `pdftoppm` are installed (else open fails: "not available")

use crate::thumbs::Bgra;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

// TYPE-COPY(rf-win look/pdf.rs): the document, read through `pdfinfo` and `pdftoppm` (poppler) when they are installed
pub struct Pdf {
    path: PathBuf,
    password: Option<String>,
    sizes: Vec<(f32, f32)>,
}

/// Runs `prog` with `args` (never a shell); `None` when it is not installed or runs longer than `secs` (then killed).
fn run(prog: &str, args: &[&OsStr], secs: u64, limit: u64) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(prog).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let t_out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out.by_ref().take(limit).read_to_end(&mut v);
        v
    });
    let t_err = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err.by_ref().take(1 << 16).read_to_end(&mut v);
        v
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some((ok, t_out.join().unwrap_or_default(), t_err.join().unwrap_or_default()))
}

fn password_args<'a>(pw: &'a Option<String>, v: &mut Vec<&'a OsStr>) {
    if let Some(p) = pw {
        v.push(OsStr::new("-upw"));
        v.push(OsStr::new(p));
    }
}

/// The page sizes (points) from `pdfinfo -f 1 -l <last>` lines like `Page    1 size: 595.276 x 841.89 pts`.
fn parse_sizes(text: &str) -> Vec<(f32, f32)> {
    let mut v = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Page ") else { continue };
        let Some((_, after)) = rest.split_once("size:") else { continue };
        let mut it = after.split_whitespace();
        let w = it.next().and_then(|x| x.parse::<f32>().ok());
        let _x = it.next();
        let h = it.next().and_then(|x| x.parse::<f32>().ok());
        if let (Some(w), Some(h)) = (w, h) {
            v.push((w, h));
        }
    }
    v
}

impl Pdf {
    pub fn open(path: &Path, password: Option<&str>) -> anyhow::Result<Pdf> {
        let pw = password.map(str::to_owned);
        let mut args: Vec<&OsStr> = vec![OsStr::new("-f"), OsStr::new("1"), OsStr::new("-l"), OsStr::new("100000")];
        password_args(&pw, &mut args);
        args.push(path.as_os_str());
        let (ok, out, err) = run("pdfinfo", &args, 20, 8 << 20).ok_or_else(|| anyhow::anyhow!("pdfinfo is not available"))?;
        if !ok {
            let e = String::from_utf8_lossy(&err).to_lowercase();
            if e.contains("password") {
                return Err(PdfError::Password.into());
            }
            return Err(PdfError::Unreadable.into());
        }
        let sizes = parse_sizes(&String::from_utf8_lossy(&out));
        if sizes.is_empty() {
            return Err(PdfError::Unreadable.into());
        }
        Ok(Pdf { path: path.to_path_buf(), password: pw, sizes })
    }
    pub fn page_count(&self) -> u32 {
        self.sizes.len() as u32
    }
    pub fn page_size(&self, index: u32) -> anyhow::Result<(f32, f32)> {
        self.sizes.get(index as usize).copied().ok_or_else(|| anyhow::anyhow!("no page {index}"))
    }
    /// Page `index` (from 0) as a picture `width` pixels wide.
    pub fn render(&self, index: u32, width: u32) -> anyhow::Result<Bgra> {
        anyhow::ensure!((index as usize) < self.sizes.len(), "no page {index}");
        let (pw, ph) = self.sizes[index as usize];
        let width = width.clamp(1, 8192);
        let height = ((ph / pw.max(1.0)) * width as f32).round().max(1.0) as u32;
        let page = (index + 1).to_string();
        let (w, h) = (width.to_string(), height.to_string());
        let mut args: Vec<&OsStr> = ["-png", "-f", &page, "-l", &page, "-singlefile", "-scale-to-x", &w, "-scale-to-y", &h].iter().map(|s| OsStr::new(*s)).collect();
        password_args(&self.password, &mut args);
        args.push(self.path.as_os_str());
        args.push(OsStr::new("-"));
        let (ok, png, _) = run("pdftoppm", &args, 30, 128 << 20).ok_or_else(|| anyhow::anyhow!("pdftoppm is not available"))?;
        anyhow::ensure!(ok && !png.is_empty(), PdfError::Unreadable);
        let img = image::load_from_memory(&png)?.to_rgba8();
        let (width, height) = img.dimensions();
        let mut data = img.into_raw();
        for px in data.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok(Bgra { width, height, data })
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

#[cfg(test)]
mod tests {
    #[test]
    fn page_sizes_are_read_from_pdfinfo_lines() {
        let v = super::parse_sizes("Pages:  2
Page    1 size: 595.276 x 841.89 pts (A4)
Page    2 size: 200 x 100 pts
");
        assert_eq!(v, vec![(595.276, 841.89), (200.0, 100.0)]);
    }
}
