//! The text editor's round trip of a file: what the file was written in (encoding, byte order mark, line breaks, a last line break
//! or none) is read once, and a save writes the edited text back the same way. A file nobody changed comes out byte for byte as it
//! went in; a file that would not come out like that is not offered for editing at all (`open` says why).
//!
//! The editor works on text whose line breaks are all `\n`. Each line break of the file is remembered: on save the lines the edit
//! did not touch get their own break back (a file with both CRLF and LF lines keeps both), new lines get the break most of the file
//! uses.

use std::borrow::Cow;

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

/// What the encoding guess looks at (the same as the viewer's, so both read the file the same way).
const SNIFF: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
}

impl Eol {
    fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
        }
    }
}

/// The line breaks of the whole file, for the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breaks {
    /// One line, no break.
    None,
    Lf,
    CrLf,
    Mixed,
}

/// Why a file is not offered for editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// NUL characters: a binary file that looks like text.
    NotText,
    /// The bytes do not decode cleanly in the encoding found, or decoding and encoding again would not give the same bytes.
    Lossy,
    /// A carriage return on its own (old Mac line breaks): the editor would not keep it apart from a CRLF.
    LoneCr,
}

/// A character the file's encoding cannot hold (a save in that encoding would lose it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unrepresentable(pub char);

/// How a file was written, and its text as loaded or last saved.
#[derive(Clone, Debug)]
pub struct TextFormat {
    enc: &'static Encoding,
    bom: bool,
    /// The text on disk (breaks as `\n`), and the break after each of its lines.
    base: String,
    eols: Vec<Eol>,
    /// The break new lines get.
    new_eol: Eol,
}

/// The break of the system the file has none to copy from.
fn platform_eol() -> Eol {
    if cfg!(windows) { Eol::CrLf } else { Eol::Lf }
}

/// Reads a file's bytes for editing: its text (breaks as `\n`) and how it was written.
pub fn open(bytes: &[u8]) -> Result<(String, TextFormat), Refusal> {
    let (enc, bom) = crate::textload::detect(&bytes[..bytes.len().min(SNIFF)]);
    let body = &bytes[bom..];
    let text = enc.decode_without_bom_handling_and_without_replacement(body).ok_or(Refusal::Lossy)?;
    if text.contains('\0') {
        return Err(Refusal::NotText);
    }
    // the strict test: the text written back in the same encoding is the same bytes
    if encode_body(enc, &text).ok().as_deref() != Some(body) {
        return Err(Refusal::Lossy);
    }
    let mut eols = Vec::new();
    let pieces: Vec<&str> = text.split('\n').collect();
    let last = pieces.len() - 1;
    for (i, p) in pieces.iter().enumerate() {
        let (inner, crlf) = match p.strip_suffix('\r') {
            Some(inner) if i < last => (inner, true),
            _ => (*p, false),
        };
        if inner.contains('\r') {
            return Err(Refusal::LoneCr);
        }
        if i < last {
            eols.push(if crlf { Eol::CrLf } else { Eol::Lf });
        }
    }
    let crlf = eols.iter().filter(|e| **e == Eol::CrLf).count();
    let new_eol = if eols.is_empty() { platform_eol() } else if crlf * 2 > eols.len() { Eol::CrLf } else { Eol::Lf };
    let base = if crlf > 0 { text.replace("\r\n", "\n") } else { text.into_owned() };
    Ok((base.clone(), TextFormat { enc, bom: bom > 0, base, eols, new_eol }))
}

/// The text in `enc`, without a byte order mark. UTF-16 is written here: encoding_rs only decodes it.
fn encode_body(enc: &'static Encoding, s: &str) -> Result<Vec<u8>, Unrepresentable> {
    if enc == UTF_8 {
        return Ok(s.as_bytes().to_vec());
    }
    if enc == UTF_16LE || enc == UTF_16BE {
        let le = enc == UTF_16LE;
        let mut out = Vec::with_capacity(s.len() * 2);
        for u in s.encode_utf16() {
            out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        return Ok(out);
    }
    let (bytes, used, errors) = enc.encode(s);
    if errors || used != enc {
        // (only on this path: which character it was, for the question)
        let bad = s.chars().find(|c| enc.encode(c.encode_utf8(&mut [0; 4])).2).unwrap_or('\u{FFFD}');
        return Err(Unrepresentable(bad));
    }
    Ok(bytes.into_owned())
}

fn bom_of(enc: &'static Encoding) -> &'static [u8] {
    if enc == UTF_16LE {
        &[0xFF, 0xFE]
    } else if enc == UTF_16BE {
        &[0xFE, 0xFF]
    } else {
        &[0xEF, 0xBB, 0xBF]
    }
}

impl TextFormat {
    /// The encoding's name as the status line shows it ("UTF-8 BOM", "windows-1254").
    pub fn encoding_label(&self) -> String {
        if self.bom { format!("{} BOM", self.enc.name()) } else { self.enc.name().to_string() }
    }

    pub fn breaks(&self) -> Breaks {
        match (self.eols.contains(&Eol::Lf), self.eols.contains(&Eol::CrLf)) {
            (false, false) => Breaks::None,
            (true, false) => Breaks::Lf,
            (false, true) => Breaks::CrLf,
            (true, true) => Breaks::Mixed,
        }
    }

    /// The bytes of `text` written the way the file was, and the format after that save (the caller keeps it only once the
    /// bytes are on disk). A character the encoding cannot hold is an error: nothing is lost without the user saying so.
    pub fn encode(&self, text: &str) -> Result<(Vec<u8>, TextFormat), Unrepresentable> {
        self.encode_in(self.enc, self.bom, text)
    }

    /// The same in UTF-8 without a byte order mark, for a text the file's own encoding cannot hold.
    pub fn encode_utf8(&self, text: &str) -> (Vec<u8>, TextFormat) {
        self.encode_in(UTF_8, false, text).expect("UTF-8 holds every character")
    }

    fn encode_in(&self, enc: &'static Encoding, bom: bool, text: &str) -> Result<(Vec<u8>, TextFormat), Unrepresentable> {
        // a CRLF pasted in is a line break like any other
        let text: Cow<str> = if text.contains('\r') { Cow::Owned(text.replace("\r\n", "\n")) } else { Cow::Borrowed(text) };
        let eols = self.eols_for(&text);
        let mut out = String::with_capacity(text.len() + eols.len());
        for (i, piece) in text.split('\n').enumerate() {
            if i > 0 {
                out.push_str(eols[i - 1].as_str());
            }
            out.push_str(piece);
        }
        let body = encode_body(enc, &out)?;
        let mut bytes = Vec::with_capacity(body.len() + 3);
        if bom {
            bytes.extend_from_slice(bom_of(enc));
        }
        bytes.extend_from_slice(&body);
        Ok((bytes, TextFormat { enc, bom, base: text.into_owned(), eols, new_eol: self.new_eol }))
    }

    /// The break after each line of `new`. The lines before and after the edited part are matched with the file's (from the start
    /// and from the end) and keep their own breaks; inside the edited part a line keeps the break of the line at its place when
    /// the part has as many lines as before (an edit within lines), else it gets the file's usual break.
    fn eols_for(&self, new: &str) -> Vec<Eol> {
        let old: Vec<&str> = self.base.split('\n').collect();
        let new: Vec<&str> = new.split('\n').collect();
        let (no, nn) = (old.len(), new.len());
        let mut p = 0;
        while p < no && p < nn && old[p] == new[p] {
            p += 1;
        }
        let mut s = 0;
        while s < no - p && s < nn - p && old[no - 1 - s] == new[nn - 1 - s] {
            s += 1;
        }
        (0..nn - 1)
            .map(|i| {
                let j = if i < p || no == nn {
                    Some(i)
                } else if i >= nn - s {
                    Some(i + no - nn)
                } else {
                    None
                };
                j.and_then(|j| self.eols.get(j).copied()).unwrap_or(self.new_eol)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le_bom(s: &str) -> Vec<u8> {
        let mut v = vec![0xFF, 0xFE];
        for u in s.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    }

    /// Opened and saved with nothing changed, every kind of file is the same bytes; an edit changes only what was edited.
    #[test]
    fn round_trip_keeps_every_byte() {
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("utf-8", "çay\nşeker\n".as_bytes().to_vec(), "UTF-8"),
            ("utf-8 bom", b"\xEF\xBB\xBFbir\r\niki\r\n".to_vec(), "UTF-8 BOM"),
            ("utf-16 le bom", utf16le_bom("satır\r\nüç\r\n"), "UTF-16LE BOM"),
            ("windows-1254", vec![0xFE, 0xFD, 0xF0, 0x20, 0x61, b'\r', b'\n', 0xDD, b'\r', b'\n'], "windows-1254"),
            ("crlf", b"a\r\nb\r\nc\r\n".to_vec(), "UTF-8"),
            ("lf", b"a\nb\nc\n".to_vec(), "UTF-8"),
            ("no last break", b"a\nb".to_vec(), "UTF-8"),
            ("mixed", b"a\r\nb\nc\r\nd".to_vec(), "UTF-8"),
            ("empty", Vec::new(), "UTF-8"),
        ];
        for (name, bytes, enc) in cases {
            let (text, f) = open(&bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(!text.contains('\r'), "{name}: the editor gets \\n breaks");
            assert_eq!(f.encoding_label(), enc, "{name}");
            let (out, next) = f.encode(&text).unwrap();
            assert_eq!(out, bytes, "{name}: unchanged text is the same bytes");
            assert_eq!(next.encode(&text).unwrap().0, bytes, "{name}: and again after a save");
        }

        // an edit in the middle: the other lines keep their own breaks, the last line keeps having none
        let (text, f) = open(b"one\r\ntwo\nthree\r\nfour").unwrap();
        assert_eq!(f.breaks(), Breaks::Mixed);
        assert_eq!(f.encode(&text.replace("two", "TWO")).unwrap().0, b"one\r\nTWO\nthree\r\nfour");
        // lines put in get the file's usual break; a CRLF pasted in becomes that break too
        let (text, f) = open(b"a\r\nb\r\nc\r\n").unwrap();
        assert_eq!(f.encode(&text.replace("b\n", "b\nx\ny\r\n")).unwrap().0, b"a\r\nb\r\nx\r\ny\r\nc\r\n");
        // a legacy code page saves edits in it; a character it cannot hold is refused, and UTF-8 is the way out
        let (text, f) = open(&[0xFE, 0xFD, 0xF0, b'\n']).unwrap();
        assert_eq!(f.encode(&text.replace('ş', "ğş")).unwrap().0, vec![0xF0, 0xFE, 0xFD, 0xF0, b'\n']);
        assert_eq!(f.encode("日").unwrap_err(), Unrepresentable('日'));
        assert_eq!(f.encode_utf8("日\n").0, "日\n".as_bytes());
        // what is not offered: a broken UTF-16 file, a lone CR, NUL characters
        assert_eq!(open(&[0xFF, 0xFE, b'a', 0, b'b']).unwrap_err(), Refusal::Lossy);
        assert_eq!(open(b"a\rb\r\n").unwrap_err(), Refusal::LoneCr);
        assert_eq!(open(b"a\0b").unwrap_err(), Refusal::NotText);
    }
}
