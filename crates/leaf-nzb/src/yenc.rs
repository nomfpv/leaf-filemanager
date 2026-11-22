//! yEnc: the encoding binary Usenet posts use. One article holds one part of a file: `=ybegin` (name, file size, part),
//! `=ypart` (where the part sits in the file, 1-based and inclusive), the encoded lines, `=yend` (the part's size and CRC32).

/// One decoded article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// The file's name as the poster's yEnc header says it.
    pub name: String,
    /// The whole file's size.
    pub file_size: u64,
    /// Where `data` goes in the file (0-based).
    pub offset: u64,
    pub data: Vec<u8>,
    /// The part's CRC32 matched (`true` when the article carries none to check against).
    pub crc_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No `=ybegin` line: not a yEnc article.
    NotYenc,
    /// `=ybegin` without `=yend`: the article was cut.
    Truncated,
    /// The decoded size differs from what `=yend` says.
    Size { expected: u64, got: u64 },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotYenc => f.write_str("not a yEnc article"),
            Error::Truncated => f.write_str("the yEnc article ends before =yend"),
            Error::Size { expected, got } => write!(f, "yEnc part is {got} bytes, its trailer says {expected}"),
        }
    }
}

/// `key=value` of a yEnc header line; `name` takes the rest of the line (names have spaces).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    if key == "name" {
        return line.find(" name=").map(|i| &line[i + 6..]).map(|s| s.trim_end_matches(['\r', '\n']));
    }
    line.split_whitespace().find_map(|w| w.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

fn num(line: &str, key: &str) -> Option<u64> {
    field(line, key)?.parse().ok()
}

fn hex(line: &str, key: &str) -> Option<u32> {
    u32::from_str_radix(field(line, key)?.trim(), 16).ok()
}

/// Decodes an article's body (already dot-unstuffed, lines ending in CRLF or LF).
pub fn decode(body: &[u8]) -> Result<Part, Error> {
    let mut lines = body.split(|b| *b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    let begin = loop {
        let l = lines.next().ok_or(Error::NotYenc)?;
        if l.starts_with(b"=ybegin ") {
            break String::from_utf8_lossy(l).into_owned();
        }
    };
    let file_size = num(&begin, "size").unwrap_or(0);
    let name = field(&begin, "name").unwrap_or_default().to_string();
    let mut offset = 0;
    let mut data = Vec::with_capacity(file_size.min(1 << 20) as usize);
    let mut end: Option<String> = None;
    let mut pending: Option<&[u8]> = lines.next();
    if let Some(l) = pending
        && l.starts_with(b"=ypart ")
    {
        let p = String::from_utf8_lossy(l);
        offset = num(&p, "begin").unwrap_or(1).saturating_sub(1);
        pending = lines.next();
    }
    while let Some(l) = pending {
        if l.starts_with(b"=yend") {
            end = Some(String::from_utf8_lossy(l).into_owned());
            break;
        }
        decode_line(l, &mut data);
        pending = lines.next();
    }
    let end = end.ok_or(Error::Truncated)?;
    if let Some(expected) = num(&end, "size")
        && expected != data.len() as u64
    {
        return Err(Error::Size { expected, got: data.len() as u64 });
    }
    // a multi-part post checks each part with pcrc32; a single-part one with crc32 (the whole file)
    let want = hex(&end, "pcrc32").or_else(|| if num(&begin, "part").is_none() { hex(&end, "crc32") } else { None });
    let crc_ok = want.is_none_or(|c| crc32fast::hash(&data) == c);
    Ok(Part { name, file_size, offset, data, crc_ok })
}

fn decode_line(l: &[u8], out: &mut Vec<u8>) {
    let mut escape = false;
    for &b in l {
        if escape {
            out.push(b.wrapping_sub(64).wrapping_sub(42));
            escape = false;
        } else if b == b'=' {
            escape = true;
        } else {
            out.push(b.wrapping_sub(42));
        }
    }
}

/// Encodes `data` as one yEnc article body (for tests and the fake server): `part`/`total` of a file of `file_size` bytes
/// whose part starts at `offset`.
#[cfg(test)]
pub fn encode(name: &str, file_size: u64, offset: u64, data: &[u8], part: Option<(u32, u32)>) -> Vec<u8> {
    let mut out = Vec::new();
    match part {
        Some((p, t)) => {
            out.extend(format!("=ybegin part={p} total={t} line=128 size={file_size} name={name}\r\n").bytes());
            out.extend(format!("=ypart begin={} end={}\r\n", offset + 1, offset + data.len() as u64).bytes());
        }
        None => out.extend(format!("=ybegin line=128 size={file_size} name={name}\r\n").bytes()),
    }
    let mut col = 0;
    for &b in data {
        let e = b.wrapping_add(42);
        if matches!(e, 0 | b'\n' | b'\r' | b'=') || (col == 0 && matches!(e, b'.' | b'\t' | b' ')) {
            out.push(b'=');
            out.push(e.wrapping_add(64));
            col += 2;
        } else {
            out.push(e);
            col += 1;
        }
        if col >= 128 {
            out.extend(b"\r\n");
            col = 0;
        }
    }
    if col > 0 {
        out.extend(b"\r\n");
    }
    let crc = crc32fast::hash(data);
    match part {
        Some((p, _)) => out.extend(format!("=yend size={} part={p} pcrc32={crc:08x}\r\n", data.len()).bytes()),
        None => out.extend(format!("=yend size={} crc32={crc:08x}\r\n", data.len()).bytes()),
    }
    out
}
