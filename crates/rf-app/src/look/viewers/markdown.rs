//! Markdown, rendered (headings, lists, code, links, tables) instead of the raw text the text
//! viewer would show. Very large files: the first 2 MB.

use gpui_kit::*;

use super::text::EditSwitch;
use crate::look::viewer::{MoreItem, Prepared, Probe, Viewer};

const EXTENSIONS: &[&str] = &["md", "markdown", "mdx"];
const READ_MAX: usize = 2 << 20;

#[derive(Default)]
pub struct MarkdownViewer {
    text: Option<SharedString>,
    error: Option<String>,
    path: std::path::PathBuf,
    /// E: the text viewer's editor on this file; when it ends the file is read again.
    edit: EditSwitch,
}

fn read(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take(READ_MAX as u64).read_to_end(&mut buf)?;
    // UTF-8 (with or without BOM); anything else shows as best it can
    let s = String::from_utf8_lossy(&buf).into_owned();
    Ok(s.trim_start_matches('\u{feff}').to_string())
}

impl MarkdownViewer {
    fn show(&mut self) {
        self.error = None;
        match read(&self.path) {
            Ok(t) => self.text = Some(t.into()),
            Err(e) => {
                self.text = None;
                self.error = Some(e.to_string());
            }
        }
    }
}

impl Viewer for MarkdownViewer {
    fn name(&self) -> &'static str {
        "markdown"
    }
    fn priority(&self) -> i32 {
        // ahead of the plain text viewer
        -4
    }
    fn can_handle(&self, f: &Probe) -> bool {
        f.is_file() && EXTENSIONS.contains(&f.ext.as_str())
    }
    fn prepare(&mut self, f: &Probe, _cx: &mut App) -> Prepared {
        self.path = f.path.clone();
        self.edit.reset(&f.path);
        self.show();
        Prepared::new((900.0, 760.0), super::file_name(&f.path))
    }
    fn render(&mut self, window: &mut Window, cx: &mut App) -> AnyElement {
        if self.edit.take_ended() {
            self.show();
        }
        let plain = self.plain(window);
        self.edit.wrap(plain, window, cx)
    }
    fn more_menu(&self) -> Vec<MoreItem> {
        self.edit.more_menu()
    }
    fn on_key(&mut self, key: &str, mods: &Modifiers, window: &mut Window, cx: &mut App) -> bool {
        self.edit.on_key(key, mods, window, cx)
    }
    fn editing(&self, cx: &App) -> bool {
        self.edit.editing(cx)
    }
    fn has_unsaved(&self, cx: &App) -> bool {
        self.edit.has_unsaved(cx)
    }
    fn ask_to_leave(&mut self, go: Box<dyn FnOnce(&mut Window, &mut App)>, window: &mut Window, cx: &mut App) {
        self.edit.ask_to_leave(go, window, cx)
    }
    fn take_focus(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.edit.take_focus(window, cx)
    }
    fn cleanup(&mut self) {
        self.text = None;
        self.error = None;
        self.edit.cleanup();
    }
}

impl MarkdownViewer {
    fn plain(&self, window: &mut Window) -> AnyElement {
        let p = crate::theme::Palette::for_window(window);
        match (&self.text, &self.error) {
            (Some(t), _) => div()
                .id("dl-md-scroll")
                .size_full()
                .overflow_y_scroll()
                .bg(p.content_bg)
                .text_color(p.text)
                .px_8()
                .py_6()
                .child(gpui_kit::component::text::TextView::markdown("dl-md", t.clone()))
                .into_any_element(),
            (None, e) => div().size_full().p_4().text_color(p.danger).child(e.clone().unwrap_or_default()).into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_with_or_without_bom() {
        let dir = std::env::temp_dir().join(format!("rf-md-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.md");
        std::fs::write(&f, "\u{feff}# Başlık\n\n- bir").unwrap();
        assert_eq!(super::read(&f).unwrap(), "# Başlık\n\n- bir");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
