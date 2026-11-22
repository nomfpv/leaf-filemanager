//! File categorization → symbol icon (Lucide) + type label.
//! Native shell icons/thumbnails come later via rf-win; this is the instant, zero-I/O fallback.

use crate::t;
use gpui_kit::assets::IconName as L;
use rf_core::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Folder,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Document,
    Spreadsheet,
    Pdf,
    Executable,
    Text,
    Other,
}

pub fn category(e: &Entry) -> Category {
    if e.is_dir() {
        return Category::Folder;
    }
    if e.ts_source {
        return Category::Code;
    }
    category_for_ext(&e.ext)
}

pub fn category_for_ext(ext: &str) -> Category {
    use Category::*;
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "heic" | "heif" | "avif"
        | "svg" | "psd" | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "jfif" | "exr" | "jxl" | "svgz" | "psb" | "hif" => Image,
        // every camera RAW format leaf-decode reads (raf, orf, rw2, ...): one list, owned by the decoder
        _ if leaf_decode::is_raw_ext(ext) => Image,
        "mp4" | "mov" | "mkv" | "avi" | "wmv" | "webm" | "m4v" | "mpg" | "mpeg" | "flv" | "mxf" | "braw"
        | "r3d" | "ts" | "m2ts" | "3gp" | "prproj" | "aep" | "drp" => Video,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" | "aiff" | "aif" | "mid" => Audio,
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" | "iso" | "img" | "vhd"
        | "vhdx" => Archive,
        "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "js" | "jsx" | "ts_" | "tsx" | "py" | "go" | "java" | "kt"
        | "swift" | "rb" | "php" | "html" | "htm" | "css" | "scss" | "json" | "toml" | "yaml" | "yml"
        | "xml" | "sh" | "ps1" | "bat" | "cmd" | "lua" | "sql" | "vue" | "svelte" | "dart" | "zig" | "jsx_"
        | "mjs" | "cjs" | "ini" | "cfg" | "lock" => Code,
        "doc" | "docx" | "odt" | "rtf" | "pages" | "ppt" | "pptx" | "key" | "odp" => Document,
        "xls" | "xlsx" | "csv" | "ods" | "numbers" => Spreadsheet,
        "pdf" => Pdf,
        "exe" | "msi" | "appx" | "msix" | "dll" | "sys" | "lnk" | "url" | "com" | "scr" => Executable,
        "txt" | "md" | "log" | "nfo" | "srt" | "vtt" => Text,
        _ => Other,
    }
}

/// Files the shell can render a content thumbnail for.
pub fn has_thumbnail(e: &Entry) -> bool {
    if e.is_dir() || e.is_cloud_placeholder() {
        return false;
    }
    matches!(category(e), Category::Image | Category::Video | Category::Pdf)
        || matches!(&*e.ext, "docx" | "pptx" | "xlsx" | "psd" | "blend" | "3mf" | "stl" | "glb")
        || leaf_sys::design::is_design_ext(&e.ext)
}

pub fn icon_for(cat: Category) -> L {
    use Category::*;
    match cat {
        Folder => L::Folder,
        Image => L::FileImage,
        Video => L::FileVideoCamera,
        Audio => L::FileMusic,
        Archive => L::FileArchive,
        Code => L::FileCode,
        Document => L::FileText,
        Spreadsheet => L::FileSpreadsheet,
        Pdf => L::FileText,
        Executable => L::AppWindow,
        Text => L::FileText,
        Other => L::File,
    }
}

pub fn type_label(e: &Entry) -> String {
    if e.ts_source {
        return t!("TypeScript source").into();
    }
    type_label_of(e.is_dir(), &e.ext)
}

/// [`type_label`] for a folder or for a file's extension (lower case, no dot).
pub fn type_label_of(dir: bool, ext: &str) -> String {
    // short names, as in the mockups ("Folder", "JPEG image", "ZIP archive", "Rust source"): the Type column stays narrow
    if dir {
        return t!("Folder").into();
    }
    let named = match ext {
        "" => t!("File"),
        "exe" | "com" | "appx" | "msix" => t!("Application"),
        "msi" => t!("Installer"),
        "lnk" | "url" => t!("Shortcut"),
        "txt" | "log" => t!("Text file"),
        "md" => t!("Markdown"),
        "pdf" => t!("PDF document"),
        "csv" => t!("CSV file"),
        "rs" => t!("Rust source"),
        "sh" | "ps1" | "bat" | "cmd" => t!("Shell script"),
        "doc" | "docx" | "odt" | "rtf" => t!("Text document"),
        "ppt" | "pptx" | "odp" | "key" => t!("Presentation"),
        "xls" | "xlsx" | "ods" | "numbers" => t!("Spreadsheet"),
        "jpg" | "jpeg" | "jfif" => t!("JPEG image"),
        "zst" => t!("Zstd archive"),
        _ => "",
    };
    if !named.is_empty() {
        return named.into();
    }
    let kind = match category_for_ext(ext) {
        Category::Image => t!("image"),
        Category::Video => t!("video"),
        Category::Audio => t!("audio"),
        Category::Archive => t!("archive"),
        _ => t!("file"),
    };
    format!("{} {kind}", ext.to_uppercase())
}
