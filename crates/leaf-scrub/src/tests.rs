//! Every file here is made by the test with made-up values; nothing comes from a real photo or document.

use super::*;
use std::io::Write;

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("leaf-scrub-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn opts() -> Options {
    Options { keep_color_profile: true, keep_cover_art: false, reset_times: false, remove_signatures: false, keep_subtitles: false }
}

fn no_cancel() -> bool {
    false
}

fn rat(n: u32, d: u32) -> Vec<u8> {
    let mut v = n.to_be_bytes().to_vec();
    v.extend_from_slice(&d.to_be_bytes());
    v
}

/// A big-endian Exif block with make, orientation 6 and a GPS position (the Eiffel Tower).
fn exif_tiff() -> Vec<u8> {
    exif_tiff_with(b"Example Camera\0", [(48, 1), (51, 1), (30, 1)])
}

fn exif_tiff_with(make: &[u8], lat: [(u32, u32); 3]) -> Vec<u8> {
    // layout: header 8, IFD0 (3 entries) at 8: 2 + 36 + 4 = 42 -> make at 50, GPS IFD after it
    let ifd0 = 8usize;
    let make_at = ifd0 + 2 + 3 * 12 + 4;
    let gps_at = make_at + make.len();
    let mut v = Vec::new();
    v.extend_from_slice(b"MM\0*");
    v.extend_from_slice(&(ifd0 as u32).to_be_bytes());
    v.extend_from_slice(&3u16.to_be_bytes());
    // Make
    v.extend_from_slice(&0x010Fu16.to_be_bytes());
    v.extend_from_slice(&2u16.to_be_bytes());
    v.extend_from_slice(&(make.len() as u32).to_be_bytes());
    v.extend_from_slice(&(make_at as u32).to_be_bytes());
    // Orientation = 6
    v.extend_from_slice(&0x0112u16.to_be_bytes());
    v.extend_from_slice(&3u16.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&[0, 6, 0, 0]);
    // GPSInfo pointer
    v.extend_from_slice(&0x8825u16.to_be_bytes());
    v.extend_from_slice(&4u16.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&(gps_at as u32).to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(v.len(), make_at);
    v.extend_from_slice(make);
    // GPS IFD: LatitudeRef, Latitude
    let lat_at = gps_at + 2 + 2 * 12 + 4;
    v.extend_from_slice(&2u16.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&2u16.to_be_bytes());
    v.extend_from_slice(&2u32.to_be_bytes());
    v.extend_from_slice(b"N\0\0\0");
    v.extend_from_slice(&2u16.to_be_bytes());
    v.extend_from_slice(&5u16.to_be_bytes());
    v.extend_from_slice(&3u32.to_be_bytes());
    v.extend_from_slice(&(lat_at as u32).to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    for (n, d) in lat {
        v.extend_from_slice(&rat(n, d));
    }
    v
}

fn test_image() -> ::image::RgbImage {
    ::image::RgbImage::from_fn(24, 16, |x, y| ::image::Rgb([(x * 10) as u8, (y * 15) as u8, 90]))
}

fn make_jpeg(path: &Path) {
    fs::write(path, jpeg_bytes(&exif_tiff(), b"made by user")).unwrap();
}

fn jpeg_bytes(tiff: &[u8], com: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    test_image().write_to(&mut std::io::Cursor::new(&mut bytes), ::image::ImageFormat::Jpeg).unwrap();
    // insert Exif, XMP and a comment right after the start marker
    let mut extra = Vec::new();
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(tiff);
    extra.extend_from_slice(&[0xFF, 0xE1]);
    extra.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    extra.extend_from_slice(&exif);
    let xmp = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta><dc:creator>user</dc:creator></x:xmpmeta>";
    extra.extend_from_slice(&[0xFF, 0xE1]);
    extra.extend_from_slice(&((xmp.len() + 2) as u16).to_be_bytes());
    extra.extend_from_slice(xmp);
    extra.extend_from_slice(&[0xFF, 0xFE]);
    extra.extend_from_slice(&((com.len() + 2) as u16).to_be_bytes());
    extra.extend_from_slice(com);
    bytes.splice(2..2, extra);
    bytes
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn pixels(path: &Path) -> Vec<u8> {
    ::image::open(path).unwrap().to_rgb8().into_raw()
}

#[test]
fn scrub_jpeg_gps_gone_and_still_decodes() {
    let d = tmp_dir("jpeg");
    let src = d.join("a.jpg");
    make_jpeg(&src);
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label.contains("GPS")), "the test file must carry GPS: {:?}", before.findings);
    let dst = clean_copy_name(&src);
    let out = clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert!(out.removed.iter().any(|f| f.label.contains("GPS")));
    let bytes = fs::read(&dst).unwrap();
    assert!(!contains(&bytes, b"Example Camera") && !contains(&bytes, b"xmpmeta") && !contains(&bytes, b"made by user"));
    // it still decodes, to the same pixels (segments were removed, the picture was not re-encoded)
    assert_eq!(pixels(&src), pixels(&dst));
    // the orientation is kept so the picture shows the same way up
    let ex = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(fs::File::open(&dst).unwrap())).unwrap();
    assert_eq!(ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY).unwrap().value.get_uint(0), Some(6));
    assert!(ex.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_none());
    assert!(inspect(&dst, &opts()).unwrap().to_remove().next().is_none());
    assert!(src.exists());
    let _ = fs::remove_dir_all(&d);
}

fn png_chunk(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut c = (body.len() as u32).to_be_bytes().to_vec();
    c.extend_from_slice(ty);
    c.extend_from_slice(body);
    let crc = crc32fast::hash(&c[4..]);
    c.extend_from_slice(&crc.to_be_bytes());
    c
}

#[test]
fn scrub_png_text_and_exif_gone_and_still_decodes() {
    let d = tmp_dir("png");
    let src = d.join("a.png");
    let mut bytes = Vec::new();
    test_image().write_to(&mut std::io::Cursor::new(&mut bytes), ::image::ImageFormat::Png).unwrap();
    let mut extra = png_chunk(b"tEXt", b"Author\0user");
    extra.extend(png_chunk(b"tEXt", b"Software\0Example Editor"));
    extra.extend(png_chunk(b"eXIf", &exif_tiff()));
    extra.extend(png_chunk(b"tIME", &[0x07, 0xE9, 1, 2, 3, 4, 5]));
    bytes.splice(33..33, extra); // after the 25-byte IHDR chunk and the 8-byte signature
    fs::write(&src, &bytes).unwrap();
    assert!(inspect(&src, &opts()).unwrap().findings.iter().any(|f| f.label == "Text: Author" && f.value == "user"));
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(!contains(&out, b"Example Editor") && !contains(&out, b"Author") && !contains(&out, &[0x07, 0xE9, 1, 2, 3]));
    assert_eq!(pixels(&src), pixels(&dst));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_webp_exif_and_xmp_gone_and_still_decodes() {
    let d = tmp_dir("webp");
    let src = d.join("a.webp");
    let mut plain = Vec::new();
    test_image().write_to(&mut std::io::Cursor::new(&mut plain), ::image::ImageFormat::WebP).unwrap();
    // a simple lossless file: RIFF size, WEBP, VP8L chunk. Wrap it into the extended layout with EXIF and XMP chunks.
    assert_eq!(&plain[8..12], b"WEBP");
    let image_chunks = plain[12..].to_vec();
    let mut vp8x = vec![0x08 | 0x04, 0, 0, 0];
    vp8x.extend_from_slice(&(24u32 - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(16u32 - 1).to_le_bytes()[..3]);
    let chunk = |t: &[u8; 4], b: &[u8]| {
        let mut c = t.to_vec();
        c.extend_from_slice(&(b.len() as u32).to_le_bytes());
        c.extend_from_slice(b);
        if b.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    let mut body = b"WEBP".to_vec();
    body.extend(chunk(b"VP8X", &vp8x));
    body.extend(image_chunks);
    body.extend(chunk(b"EXIF", &exif_tiff()));
    body.extend(chunk(b"XMP ", b"<x:xmpmeta><dc:creator>user</dc:creator></x:xmpmeta>"));
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend(body);
    fs::write(&src, &file).unwrap();
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label.contains("GPS")));
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(!contains(&out, b"Example Camera") && !contains(&out, b"xmpmeta"));
    assert_eq!(pixels(&src), pixels(&dst));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_pdf_info_and_xmp_gone_and_still_opens() {
    use lopdf::{Dictionary, Document, Object, Stream, dictionary};
    let d = tmp_dir("pdf");
    let src = d.join("a.pdf");
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(Dictionary::new(), b"BT ET".to_vec()));
    let page = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()] });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
    let xmp = doc.add_object(Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" }, b"<x:xmpmeta>user</x:xmpmeta>".to_vec()));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id, "Metadata" => xmp });
    doc.trailer.set("Root", catalog);
    let info = doc.add_object(dictionary! { "Author" => Object::string_literal("user"), "Producer" => Object::string_literal("Example Writer") });
    doc.trailer.set("Info", info);
    doc.trailer.set("ID", vec![Object::string_literal("abc"), Object::string_literal("abc")]);
    doc.save(&src).unwrap();
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label == "Author" && f.value == "user"));
    let dst = clean_copy_name(&src);
    let out = clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert!(out.removed.iter().any(|f| f.label == "Author"));
    let bytes = fs::read(&dst).unwrap();
    assert!(!contains(&bytes, b"Example Writer") && !contains(&bytes, b"xmpmeta"));
    let again = Document::load(&dst).unwrap();
    assert_eq!(again.get_pages().len(), 1);
    assert!(!again.trailer.has(b"Info"));
    let _ = fs::remove_dir_all(&d);
}

fn make_zip(path: &Path, entries: &[(&str, &str)]) {
    let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (n, c) in entries {
        z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(c.as_bytes()).unwrap();
    }
    z.finish().unwrap();
}

#[test]
fn scrub_docx_author_gone_and_still_opens() {
    let d = tmp_dir("docx");
    let src = d.join("a.docx");
    make_zip(
        &src,
        &[
            ("[Content_Types].xml", "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>"),
            ("_rels/.rels", "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"r1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"r2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail\" Target=\"docProps/thumbnail.jpeg\"/></Relationships>"),
            ("word/document.xml", "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:ins w:author=\"user\"><w:r><w:t>hi</w:t></w:r></w:ins></w:p></w:body></w:document>"),
            ("docProps/core.xml", "<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:creator>user</dc:creator><cp:lastModifiedBy>user</cp:lastModifiedBy></cp:coreProperties>"),
            ("docProps/app.xml", "<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Company>Example Corp</Company></Properties>"),
            ("docProps/thumbnail.jpeg", "not really a picture"),
        ],
    );
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label == "Author" && f.value == "user"));
    assert!(before.notes.iter().any(|n| n.contains("tracked changes")), "tracked changes are reported, not silently kept: {:?}", before.notes);
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let mut z = zip::ZipArchive::new(fs::File::open(&dst).unwrap()).unwrap();
    let mut core = String::new();
    std::io::Read::read_to_string(&mut z.by_name("docProps/core.xml").unwrap(), &mut core).unwrap();
    assert!(!core.contains("user"));
    assert!(z.by_name("docProps/thumbnail.jpeg").is_err());
    let mut rels = String::new();
    std::io::Read::read_to_string(&mut z.by_name("_rels/.rels").unwrap(), &mut rels).unwrap();
    assert!(!rels.contains("thumbnail") && rels.contains("officeDocument"));
    let mut doc = String::new();
    std::io::Read::read_to_string(&mut z.by_name("word/document.xml").unwrap(), &mut doc).unwrap();
    assert!(doc.contains("<w:t>hi</w:t>"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_odt_meta_gone_and_still_opens() {
    let d = tmp_dir("odt");
    let src = d.join("a.odt");
    make_zip(
        &src,
        &[
            ("mimetype", "application/vnd.oasis.opendocument.text"),
            ("content.xml", "<office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\"/>"),
            ("meta.xml", "<office:document-meta xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" office:version=\"1.3\"><office:meta><meta:initial-creator>user</meta:initial-creator><dc:creator>user</dc:creator></office:meta></office:document-meta>"),
            ("Thumbnails/thumbnail.png", "png"),
            ("META-INF/manifest.xml", "<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\"><manifest:file-entry manifest:full-path=\"/\" manifest:media-type=\"application/vnd.oasis.opendocument.text\"/><manifest:file-entry manifest:full-path=\"Thumbnails/thumbnail.png\" manifest:media-type=\"image/png\"/></manifest:manifest>"),
        ],
    );
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let mut z = zip::ZipArchive::new(fs::File::open(&dst).unwrap()).unwrap();
    assert_eq!(z.by_index(0).unwrap().name().unwrap(), "mimetype");
    let mut meta = String::new();
    std::io::Read::read_to_string(&mut z.by_name("meta.xml").unwrap(), &mut meta).unwrap();
    assert!(!meta.contains("user") && meta.contains("1.3"));
    assert!(z.by_name("Thumbnails/thumbnail.png").is_err());
    let mut man = String::new();
    std::io::Read::read_to_string(&mut z.by_name("META-INF/manifest.xml").unwrap(), &mut man).unwrap();
    assert!(!man.contains("Thumbnails"));
    let _ = fs::remove_dir_all(&d);
}

fn bx(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

#[test]
fn scrub_mp4_tags_location_and_times_gone_same_size() {
    let d = tmp_dir("mp4");
    let src = d.join("a.mp4");
    let mut mvhd = vec![0u8, 0, 0, 0];
    mvhd.extend_from_slice(&3_700_000_000u32.to_be_bytes()); // creation
    mvhd.extend_from_slice(&3_700_000_100u32.to_be_bytes()); // modification
    mvhd.extend_from_slice(&[0u8; 80]);
    let loc = bx(b"\xa9xyz", b"+48.8584+002.2945/");
    let ilst = bx(b"ilst", &loc);
    let meta = bx(b"meta", &[&[0u8; 4][..], &bx(b"hdlr", &[0u8; 24]), &ilst].concat());
    let udta = bx(b"udta", &meta);
    let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), udta].concat());
    let file = [bx(b"ftyp", b"isom\0\0\0\0isom"), moov, bx(b"mdat", b"0123456789")].concat();
    fs::write(&src, &file).unwrap();
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label.contains("Movie creation")));
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let out = fs::read(&dst).unwrap();
    assert_eq!(out.len(), file.len());
    assert!(contains(&out, b"free") && contains(&out, b"0123456789"));
    assert!(inspect(&dst, &opts()).unwrap().to_remove().next().is_none());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_mp3_tags_gone_and_audio_unchanged() {
    use lofty::config::WriteOptions;
    use lofty::tag::{Accessor, Tag, TagExt, TagType};
    let d = tmp_dir("mp3");
    let src = d.join("a.mp3");
    // 40 MPEG-1 Layer III frames, 128 kbit/s, 44.1 kHz: 417 bytes each
    let mut audio = Vec::new();
    for _ in 0..40 {
        let mut f = vec![0xFF, 0xFB, 0x90, 0x00];
        f.resize(417, 0);
        audio.extend(f);
    }
    fs::write(&src, &audio).unwrap();
    let mut t = Tag::new(TagType::Id3v2);
    t.set_artist("user".into());
    t.set_title("Example Song".into());
    t.save_to_path(&src, WriteOptions::default()).unwrap();
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.value == "user"));
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let out = fs::read(&dst).unwrap();
    assert!(!contains(&out, b"Example Song"));
    assert!(contains(&out, &audio[..64]));
    assert!(inspect(&dst, &opts()).unwrap().to_remove().next().is_none());
    let _ = fs::remove_dir_all(&d);
}

#[cfg(windows)]
#[test]
fn scrub_zone_identifier_stream_listed_and_not_in_the_copy() {
    let d = tmp_dir("ads");
    let src = d.join("a.txt");
    fs::write(&src, "hello").unwrap();
    let stream = format!("{}:Zone.Identifier", src.display());
    if fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/a.txt\r\n").is_err() {
        // a file system without streams (not NTFS): nothing to test here
        let _ = fs::remove_dir_all(&d);
        return;
    }
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label.contains("Zone.Identifier") && f.value.contains("example.com")));
    let dst = clean_copy_name(&src);
    let out = clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert!(!out.contents_read && out.notes.iter().any(|n| n == PLAIN_NOTE), "a kind leaf does not read is not called cleaned");
    assert!(fs::read_to_string(format!("{}:Zone.Identifier", dst.display())).is_err(), "the copy must not carry the stream");
    assert_eq!(fs::read_to_string(&dst).unwrap(), "hello");
    // removing in place (the replace mode's last step)
    let gone = remove_streams(&src);
    assert!(gone.iter().any(|n| n.contains("Zone.Identifier")));
    assert!(fs::read_to_string(&stream).is_err());
    let _ = fs::remove_dir_all(&d);
}

/// Kinds that carry metadata but have no remover (old Office / protected Office, Photoshop, RTF, EPS, AIFF) are refused and nothing
/// is written; they are never passed through as "cleaned".
#[test]
fn scrub_known_formats_without_a_remover_are_refused() {
    let d = tmp_dir("nowriter");
    let samples: [(&str, Vec<u8>); 5] = [
        ("a.doc", [&[0xD0u8, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1][..], &[0u8; 504], b"Planted Author"].concat()),
        ("a.psd", [&b"8BPS\0\x01"[..], &[0u8; 20], b"Planted Author"].concat()),
        ("a.rtf", br"{\rtf1{\info{\author Planted Author}}hello}".to_vec()),
        ("a.eps", b"%!PS-Adobe-3.0 EPSF-3.0\n%%Creator: Planted Author\n".to_vec()),
        ("a.aiff", [&b"FORM\0\0\0\x20AIFF"[..], b"AUTH\0\0\0\x0ePlanted Author"].concat()),
    ];
    for (name, bytes) in samples {
        let src = d.join(name);
        fs::write(&src, &bytes).unwrap();
        let i = inspect(&src, &opts()).unwrap();
        assert!(!i.can_clean, "{name} must be refused");
        let dst = clean_copy_name(&src);
        assert!(matches!(clean_to(&src, &dst, &opts(), &no_cancel), Err(ScrubError::Unsupported(_))), "{name}");
        assert!(!dst.exists());
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_refuses_cut_off_and_damaged_files_and_writes_nothing() {
    let d = tmp_dir("refuse");
    // JPEG cut off in the middle of the picture data
    let j = d.join("a.jpg");
    make_jpeg(&j);
    let b = fs::read(&j).unwrap();
    let cut = d.join("cut.jpg");
    fs::write(&cut, &b[..b.len() - 20]).unwrap();
    // PNG with a damaged checksum
    let p = d.join("a.png");
    let mut pb = Vec::new();
    test_image().write_to(&mut std::io::Cursor::new(&mut pb), ::image::ImageFormat::Png).unwrap();
    let n = pb.len();
    pb[n / 2] ^= 0xFF;
    fs::write(&p, &pb).unwrap();
    // PDF without its end
    let pdf = d.join("a.pdf");
    fs::write(&pdf, b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n").unwrap();
    // zip cut off
    let docx = d.join("a.docx");
    make_zip(&docx, &[("[Content_Types].xml", "<Types/>"), ("word/document.xml", "<w:document/>")]);
    let zb = fs::read(&docx).unwrap();
    fs::write(&docx, &zb[..zb.len() - 30]).unwrap();
    // TIFF: recognised, no safe writer
    let tif = d.join("a.tif");
    fs::write(&tif, b"II*\0\x08\0\0\0\0\0\0\0\0\0").unwrap();
    for f in [&cut, &p, &pdf, &docx, &tif] {
        let before = fs::read(f).unwrap();
        let dst = clean_copy_name(f);
        let r = clean_to(f, &dst, &opts(), &no_cancel);
        assert!(r.is_err(), "{} must be refused", f.display());
        assert!(!dst.exists(), "nothing written for {}", f.display());
        assert_eq!(fs::read(f).unwrap(), before, "{} must stay untouched", f.display());
    }
    // no temporary file is left behind
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("leafscrub")));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_replace_mode_leaves_the_original_recoverable() {
    let d = tmp_dir("replace");
    let src = d.join("a.jpg");
    make_jpeg(&src);
    let original = fs::read(&src).unwrap();
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // the Trash of the test: the file is moved into a folder
    let trash = |p: &Path| fs::rename(p, bin.join(p.file_name().unwrap())).map_err(|e| e.to_string());
    let out = replace(&src, &src, &opts(), &no_cancel, &trash).unwrap();
    assert_eq!(out.output, src);
    assert_eq!(fs::read(bin.join("a.jpg")).unwrap(), original, "the original is whole in the Trash");
    let now = fs::read(&src).unwrap();
    assert!(!contains(&now, b"Example Camera"));
    assert_eq!(pixels(&bin.join("a.jpg")), pixels(&src));
    // when the Trash step fails, nothing changes and nothing is left beside
    let src2 = d.join("b.jpg");
    make_jpeg(&src2);
    let before = fs::read(&src2).unwrap();
    let broken = |_: &Path| Err::<(), String>("no bin".into());
    assert!(replace(&src2, &src2, &opts(), &no_cancel, &broken).is_err());
    assert_eq!(fs::read(&src2).unwrap(), before);
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("leaf-scrub")));
    // a "Trash" that does nothing: the file is never replaced
    let noop = |_: &Path| Ok::<(), String>(());
    assert!(replace(&src2, &src2, &opts(), &no_cancel, &noop).is_err());
    assert_eq!(fs::read(&src2).unwrap(), before);
    let _ = fs::remove_dir_all(&d);
}

/// Replace mode where the "Trash" refuses: whatever the refusal did to the original, exactly one of original / cleaned file is at
/// the path afterwards, never none, and nothing is left beside it.
#[test]
fn scrub_replace_mode_bin_refuses_leaves_exactly_one_file_never_none() {
    let d = tmp_dir("replace-nobin");
    let files = |d: &Path| fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
    // 1. the bin refuses and the original stays: nothing changed, only the original
    let src = d.join("a.jpg");
    make_jpeg(&src);
    let before = fs::read(&src).unwrap();
    let refuse = |_: &Path| Err::<(), String>("this drive has no Recycle Bin".into());
    assert!(replace(&src, &src, &opts(), &no_cancel, &refuse).is_err());
    assert_eq!(fs::read(&src).unwrap(), before);
    assert_eq!(files(&d), vec!["a.jpg".to_string()]);
    // 2. the bin "refuses" but the system deleted the original for good (the user said yes to "delete permanently?"): the cleaned
    //    file takes its place, the result says there is no undo
    let gone = |p: &Path| {
        fs::remove_file(p).unwrap();
        Err::<(), String>("not in the Recycle Bin".into())
    };
    let out = replace(&src, &src, &opts(), &no_cancel, &gone).unwrap();
    assert!(!out.undoable && out.notes.iter().any(|n| n.starts_with("Replaced, no undo")));
    assert_eq!(files(&d), vec!["a.jpg".to_string()]);
    let now = fs::read(&src).unwrap();
    assert!(!contains(&now, b"Example Camera") && !now.is_empty());
    // 3. the same with a neutral name as the target: still exactly one file
    let src2 = d.join("b.jpg");
    fs::remove_file(&src).unwrap();
    make_jpeg(&src2);
    let target = d.join("file-1.jpg");
    let out = replace(&src2, &target, &opts(), &no_cancel, &gone).unwrap();
    assert_eq!(out.output, target);
    assert_eq!(files(&d), vec!["file-1.jpg".to_string()]);
    let _ = fs::remove_dir_all(&d);
}

// ------------------------------------------------------------------------------------------------------------------
// The independent second lister: the raw bytes of a cleaned file are scanned for the planted markers. This finds data that
// the structured reader cannot see: slack space, renamed-but-not-emptied boxes, old revisions, unreferenced objects.

const SERIAL: &[u8] = b"LEAFTESTSERIAL123";
const AUTHOR: &[u8] = b"Planted Author";

fn planted_lat() -> [(u32, u32); 3] {
    [(48, 1), (51, 1), (4_885_837, 100_000)]
}

/// The made-up coordinate, as the 8 bytes a rational number takes.
fn coord_bytes() -> Vec<u8> {
    rat(4_885_837, 100_000)
}

fn assert_no_markers(file: &Path, what: &str) {
    let bytes = fs::read(file).unwrap();
    for (name, m) in [("serial", SERIAL.to_vec()), ("author", AUTHOR.to_vec()), ("coordinate", coord_bytes())] {
        assert!(!contains(&bytes, &m), "{what}: the planted {name} is still somewhere in the cleaned file");
    }
}

fn assert_markers_present(file: &Path, what: &str) {
    let bytes = fs::read(file).unwrap();
    assert!(contains(&bytes, SERIAL) && contains(&bytes, AUTHOR), "{what}: the test file must carry the markers");
}

fn assert_clean_listing(file: &Path) {
    let after = inspect(file, &opts()).unwrap();
    assert!(after.to_remove().next().is_none(), "the lister still finds {:?}", after.to_remove().collect::<Vec<_>>());
}

fn full_box(ty: &[u8; 4], ver: u8, body: &[u8]) -> Vec<u8> {
    bx(ty, &[&[ver, 0, 0, 0][..], body].concat())
}

fn infe(id: u16, ty: &[u8; 4], name: &[u8], ct: Option<&[u8]>) -> Vec<u8> {
    let mut b = id.to_be_bytes().to_vec();
    b.extend_from_slice(&[0, 0]);
    b.extend_from_slice(ty);
    b.extend_from_slice(name);
    b.push(0);
    if let Some(c) = ct {
        b.extend_from_slice(c);
        b.push(0);
    }
    full_box(b"infe", 2, &b)
}

/// A HEIC-shaped file: the picture is a made-up blob of bytes (the cleaner never decodes it).
fn make_heic(path: &Path, brand: &[u8; 4]) -> Vec<u8> {
    let image = (0..64u8).map(|i| i.wrapping_mul(7).wrapping_add(3)).collect::<Vec<u8>>();
    let mut exif = vec![0, 0, 0, 0];
    exif.extend(exif_tiff_with(b"LEAFTESTSERIAL123\0", planted_lat()));
    let xmp = b"<x:xmpmeta><dc:creator>Planted Author</dc:creator></x:xmpmeta>".to_vec();
    let items: [(u16, &Vec<u8>); 3] = [(1, &image), (2, &exif), (3, &xmp)];
    let iinf = full_box(
        b"iinf",
        0,
        &[&3u16.to_be_bytes()[..], &infe(1, b"hvc1", b"", None), &infe(2, b"Exif", b"Exif", None), &infe(3, b"mime", b"XMP", Some(b"application/rdf+xml"))].concat(),
    );
    let build = |offsets: [u32; 3]| -> Vec<u8> {
        let mut iloc = vec![0x44, 0x00];
        iloc.extend_from_slice(&3u16.to_be_bytes());
        for (k, (id, data)) in items.iter().enumerate() {
            iloc.extend_from_slice(&id.to_be_bytes());
            iloc.extend_from_slice(&[0, 0, 0, 0]); // construction method 0, data reference 0
            iloc.extend_from_slice(&1u16.to_be_bytes());
            iloc.extend_from_slice(&offsets[k].to_be_bytes());
            iloc.extend_from_slice(&(data.len() as u32).to_be_bytes());
        }
        let hdlr = full_box(b"hdlr", 0, &[&[0u8; 4][..], b"pict", &[0u8; 12], b"\0"].concat());
        let pitm = full_box(b"pitm", 0, &1u16.to_be_bytes());
        let ipco = bx(b"ipco", &[bx(b"ispe", &[0u8; 12]), bx(b"irot", &[1])].concat());
        let iprp = bx(b"iprp", &ipco);
        let iref = full_box(b"iref", 0, &bx(b"cdsc", &[&2u16.to_be_bytes()[..], &1u16.to_be_bytes(), &1u16.to_be_bytes()].concat()));
        let meta = full_box(b"meta", 0, &[hdlr, pitm, iinf.clone(), full_box(b"iloc", 1, &iloc), iprp, iref].concat());
        let udta = bx(b"udta", &bx(b"\xa9xyz", b"owner Planted Author"));
        let free = bx(b"free", b"stale LEAFTESTSERIAL123 bytes");
        let mdat_body = [image.clone(), exif.clone(), xmp.clone()].concat();
        [bx(b"ftyp", &[&brand[..], &[0u8; 4], &brand[..]].concat()), meta, udta, free, bx(b"mdat", &mdat_body)].concat()
    };
    let probe = build([0, 0, 0]);
    let mdat_at = probe.len() - (image.len() + exif.len() + xmp.len()) as usize;
    let file = build([mdat_at as u32, (mdat_at + image.len()) as u32, (mdat_at + image.len() + exif.len()) as u32]);
    fs::write(path, &file).unwrap();
    image
}

#[test]
fn scrub_heic_and_avif_leave_no_marker_anywhere_and_keep_picture_and_rotation() {
    for (brand, ext) in [(b"heic", "heic"), (b"avif", "avif")] {
        let d = tmp_dir(&format!("heif-{ext}"));
        let src = d.join(format!("a.{ext}"));
        let image = make_heic(&src, brand);
        assert_markers_present(&src, ext);
        let before = inspect(&src, &opts()).unwrap();
        assert!(before.findings.iter().any(|f| f.label.contains("GPS")), "{:?}", before.findings);
        assert!(before.findings.iter().any(|f| f.label == "XMP"));
        let dst = clean_copy_name(&src);
        clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
        let (a, b) = (fs::read(&src).unwrap(), fs::read(&dst).unwrap());
        assert_eq!(a.len(), b.len(), "nothing changes size, no offset moves");
        assert!(contains(&b, &image), "the coded picture is byte for byte the same");
        assert!(contains(&b, b"irot"), "rotation stays");
        assert_no_markers(&dst, ext);
        assert_clean_listing(&dst);
        let _ = fs::remove_dir_all(&d);
    }
}

/// Files built to exhaust memory are refused quickly: a HEIC item table with zero-width fields or more items than it holds, a PDF
/// nested thousands deep, an Office part that unpacks to a thousand times its size.
#[test]
fn scrub_crafted_files_that_would_exhaust_memory_are_refused() {
    let d = tmp_dir("bombs");
    let heic = d.join("a.heic");
    make_heic(&heic, b"heic");
    let good = fs::read(&heic).unwrap();
    let at = good.windows(4).position(|w| w == b"iloc").unwrap() + 8; // after the box type and version / flags
    for (name, patch) in [("zero widths", vec![0u8, 0u8]), ("too many items", vec![0x44, 0x00, 0xFF, 0xFF])] {
        let mut b = good.clone();
        b[at..at + patch.len()].copy_from_slice(&patch);
        fs::write(&heic, &b).unwrap();
        assert!(matches!(inspect(&heic, &opts()), Err(ScrubError::Malformed(_))), "{name}");
    }
    let pdf = d.join("deep.pdf");
    let deep = format!("%PDF-1.4\n1 0 obj\n<< /Pages 2 0 R /A {}{} >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n", "[".repeat(5000), "]".repeat(5000));
    fs::write(&pdf, deep).unwrap();
    assert!(matches!(inspect(&pdf, &opts()), Err(ScrubError::Unsupported(_))));
    let docx = d.join("bomb.docx");
    let zeros = vec![b' '; 8 << 20];
    let mut z = zip::ZipWriter::new(fs::File::create(&docx).unwrap());
    for (n, c) in [("[Content_Types].xml", &b"<Types/>"[..]), ("word/document.xml", &zeros[..])] {
        z.start_file(n, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(c).unwrap();
    }
    z.finish().unwrap();
    assert!(matches!(inspect(&docx, &opts()), Err(ScrubError::Unsupported(_))));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_heic_refuses_when_metadata_shares_bytes_with_the_picture() {
    let d = tmp_dir("heif-shared");
    let src = d.join("a.heic");
    make_heic(&src, b"heic");
    let mut b = fs::read(&src).unwrap();
    // move the Exif item's data onto the picture's first bytes: item 2's iloc offset becomes item 1's
    // item 1 offset sits at: iloc body + 2 + 2 + (id 2 + method/dref 4 + ext_count 2) ; copy it into item 2's offset
    let base = b.windows(4).position(|w| w == b"iloc").unwrap() + 4 + 4 + 2 + 2;
    let item1_off = base + 2 + 4 + 2;
    let item2_off = base + 14 + 2 + 4 + 2;
    let v: [u8; 4] = b[item1_off..item1_off + 4].try_into().unwrap();
    b[item2_off..item2_off + 4].copy_from_slice(&v);
    fs::write(&src, &b).unwrap();
    let dst = clean_copy_name(&src);
    assert!(clean_to(&src, &dst, &opts(), &no_cancel).is_err());
    assert!(!dst.exists());
    let _ = fs::remove_dir_all(&d);
}

fn put_ifd(buf: &mut Vec<u8>, entries: &[(u16, u16, u32, Vec<u8>)]) -> usize {
    let at = buf.len();
    let table = 2 + 12 * entries.len() + 4;
    let mut data_at = at + table;
    let mut t = (entries.len() as u16).to_be_bytes().to_vec();
    let mut data = Vec::new();
    for (tag, ty, count, v) in entries {
        t.extend_from_slice(&tag.to_be_bytes());
        t.extend_from_slice(&ty.to_be_bytes());
        t.extend_from_slice(&count.to_be_bytes());
        if v.len() <= 4 {
            let mut x = v.clone();
            x.resize(4, 0);
            t.extend_from_slice(&x);
        } else {
            t.extend_from_slice(&(data_at as u32).to_be_bytes());
            data.extend_from_slice(v);
            if v.len() % 2 == 1 {
                data.push(0);
            }
            data_at = at + table + data.len();
        }
    }
    t.extend_from_slice(&[0; 4]);
    buf.extend(t);
    buf.extend(data);
    at
}

fn short(v: u16) -> Vec<u8> {
    let mut b = v.to_be_bytes().to_vec();
    b.extend_from_slice(&[0, 0]);
    b
}

fn long(v: u32) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}

fn text(s: &[u8]) -> (u32, Vec<u8>) {
    let mut v = s.to_vec();
    v.push(0);
    (v.len() as u32, v)
}

/// A big-endian TIFF with one 4x4 grey strip and every kind of tag the remover claims to handle.
fn make_tiff(path: &Path) -> Vec<u8> {
    let strip: Vec<u8> = (0..16u8).map(|i| i * 13 + 1).collect();
    let mut buf = vec![0u8; 8];
    buf[..4].copy_from_slice(b"MM\0*");
    buf.extend_from_slice(&strip);
    let (nl, serial) = text(SERIAL);
    let mut mk = SERIAL.to_vec();
    mk.extend_from_slice(b" maker note");
    let exif_ifd = put_ifd(&mut buf, &[(0x927C, 7, mk.len() as u32, mk), (0xA431, 2, nl, serial.clone())]);
    let mut lat = Vec::new();
    for (n, d) in planted_lat() {
        lat.extend(rat(n, d));
    }
    let gps_ifd = put_ifd(&mut buf, &[(1, 2, 2, b"N\0".to_vec()), (2, 5, 3, lat)]);
    let (al, author) = text(AUTHOR);
    let (dl, desc) = text(b"a photo of Planted Author");
    let xmp = b"<x:xmpmeta>Planted Author</x:xmpmeta>".to_vec();
    let iptc = b"\x1c\x02\x50Planted Author".to_vec();
    let ifd0 = put_ifd(
        &mut buf,
        &[
            (256, 3, 1, short(4)),
            (257, 3, 1, short(4)),
            (258, 3, 1, short(8)),
            (259, 3, 1, short(1)),
            (262, 3, 1, short(1)),
            (270, 2, dl, desc),
            (271, 2, nl, serial.clone()),
            (273, 4, 1, long(8)),
            (274, 3, 1, short(6)),
            (277, 3, 1, short(1)),
            (278, 3, 1, short(4)),
            (279, 4, 1, long(16)),
            (306, 2, 20, b"2026:11:22 14:22:33\0".to_vec()),
            (315, 2, al, author),
            (700, 1, xmp.len() as u32, xmp),
            (33432, 2, nl, serial),
            (33723, 7, iptc.len() as u32, iptc),
            (34665, 4, 1, long(exif_ifd as u32)),
            (34853, 4, 1, long(gps_ifd as u32)),
        ],
    );
    buf[4..8].copy_from_slice(&(ifd0 as u32).to_be_bytes());
    fs::write(path, &buf).unwrap();
    strip
}

#[test]
fn scrub_tiff_leaves_no_marker_anywhere_and_keeps_strips_and_orientation() {
    let d = tmp_dir("tiff");
    let src = d.join("a.tif");
    let strip = make_tiff(&src);
    assert_markers_present(&src, "tiff");
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label.contains("GPS")), "{:?}", before.findings);
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let (a, b) = (fs::read(&src).unwrap(), fs::read(&dst).unwrap());
    assert_eq!(a.len(), b.len());
    assert_eq!(&b[8..24], &strip[..], "the strip is byte for byte the same");
    assert_no_markers(&dst, "tiff");
    assert_clean_listing(&dst);
    let ex = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(fs::File::open(&dst).unwrap())).unwrap();
    assert_eq!(ex.get_field(exif::Tag::Orientation, exif::In::PRIMARY).unwrap().value.get_uint(0), Some(6));
    assert!(ex.get_field(exif::Tag::Make, exif::In::PRIMARY).is_none() && ex.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY).is_none());
    assert_eq!(ex.get_field(exif::Tag::ImageWidth, exif::In::PRIMARY).unwrap().value.get_uint(0), Some(4));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_camera_raw_is_refused_with_its_own_wording_and_nothing_written() {
    let d = tmp_dir("raw");
    let src = d.join("a.dng");
    make_tiff(&src);
    let dst = clean_copy_name(&src);
    let e = clean_to(&src, &dst, &opts(), &no_cancel).unwrap_err();
    assert!(e.to_string().contains("convert to another format first"), "{e}");
    assert!(!dst.exists());
    // a TIFF that carries the DNG version tag is refused even under a .tif name
    let dng = d.join("b.tif");
    make_tiff(&dng);
    let mut b = fs::read(&dng).unwrap();
    // turn the Software-like tag 306 (DateTime) into DNGVersion (50706)
    let at = b.windows(4).position(|w| w == [0x01, 0x32, 0x00, 0x02]).unwrap();
    b[at..at + 2].copy_from_slice(&50706u16.to_be_bytes());
    fs::write(&dng, &b).unwrap();
    assert!(clean_to(&dng, &clean_copy_name(&dng), &opts(), &no_cancel).is_err());
    let _ = fs::remove_dir_all(&d);
}

fn gif_blocks(pieces: &[&[u8]]) -> Vec<u8> {
    pieces.concat()
}

fn sub(data: &[u8]) -> Vec<u8> {
    let mut v = vec![data.len() as u8];
    v.extend_from_slice(data);
    v.push(0);
    v
}

#[test]
fn scrub_gif_leaves_no_marker_anywhere_and_keeps_frames_and_loop() {
    let d = tmp_dir("gif");
    let src = d.join("a.gif");
    let frame = [&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0][..], &[2], &sub(&[0x4C, 0x01])].concat();
    let netscape = [&[0x21, 0xFF, 11][..], b"NETSCAPE2.0", &sub(&[1, 0, 0])].concat();
    let comment = [&[0x21, 0xFE][..], &sub(b"made by Planted Author")].concat();
    let xmp = [&[0x21, 0xFF, 11][..], b"XMP DataXMP", &sub(b"<x:xmpmeta>LEAFTESTSERIAL123</x:xmpmeta>")].concat();
    let other = [&[0x21, 0xFF, 11][..], b"MYEDITOR1.0", &sub(b"Planted Author")].concat();
    let gce = [0x21, 0xF9, 4, 0, 10, 0, 0, 0];
    let file = gif_blocks(&[
        b"GIF89a", &[1, 0, 1, 0, 0x80, 0, 0], &[0, 0, 0, 255, 255, 255], &netscape, &comment, &xmp, &other, &gce.to_vec(), &frame, &[0x3B],
        b"trailing LEAFTESTSERIAL123 Planted Author",
    ]);
    fs::write(&src, &file).unwrap();
    assert_markers_present(&src, "gif");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let b = fs::read(&dst).unwrap();
    assert_no_markers(&dst, "gif");
    assert_clean_listing(&dst);
    assert!(contains(&b, &frame) && contains(&b, &netscape) && contains(&b, &gce));
    assert_eq!(*b.last().unwrap(), 0x3B);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_svg_leaves_no_marker_anywhere_and_keeps_the_drawing() {
    let d = tmp_dir("svg");
    let src = d.join("a.svg");
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- Created by LEAFTESTSERIAL123 -->
<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" width="10" height="10" inkscape:version="1.3 Planted Author" sodipodi:docname="LEAFTESTSERIAL123.svg">
  <sodipodi:namedview id="nv" inkscape:current-layer="Planted Author"/>
  <metadata id="m"><rdf:RDF><rdf:Description><dc:creator>Planted Author</dc:creator></rdf:Description></rdf:RDF></metadata>
  <rect id="r" width="5" height="5" fill="red" inkscape:label="LEAFTESTSERIAL123"/>
</svg>"##;
    fs::write(&src, xml).unwrap();
    assert_markers_present(&src, "svg");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    let out = fs::read_to_string(&dst).unwrap();
    assert_no_markers(&dst, "svg");
    assert!(!out.contains("inkscape") && !out.contains("sodipodi") && !out.contains("metadata"));
    assert!(out.contains("<rect") && out.contains("fill=\"red\"") && out.contains("width=\"10\""));
    roxmltree::Document::parse(&out).unwrap();
    assert_clean_listing(&dst);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_mp4_leaves_no_marker_in_renamed_boxes() {
    let d = tmp_dir("mp4-raw");
    let src = d.join("a.mp4");
    let loc = bx(b"\xa9xyz", b"+48.8584+002.2945/ Planted Author LEAFTESTSERIAL123");
    let meta = bx(b"meta", &[&[0u8; 4][..], &bx(b"hdlr", &[0u8; 24]), &bx(b"ilst", &loc)].concat());
    let moov = bx(b"moov", &[bx(b"mvhd", &[0u8; 100]), bx(b"udta", &meta)].concat());
    let file = [bx(b"ftyp", b"isom\0\0\0\0isom"), moov, bx(b"free", b"stale Planted Author"), bx(b"mdat", b"0123456789")].concat();
    fs::write(&src, &file).unwrap();
    assert_markers_present(&src, "mp4");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_eq!(fs::read(&dst).unwrap().len(), file.len());
    assert_no_markers(&dst, "mp4");
    let _ = fs::remove_dir_all(&d);
}

/// One MP4 track whose samples are `sizes` long and start at `chunk` in the file (one chunk).
fn mp4_trak(handler: &[u8; 4], entry: &[u8; 4], chunk: u32, sizes: &[u32]) -> Vec<u8> {
    let tkhd = bx(b"tkhd", &[&[0u8, 0, 0, 3][..], &[0u8; 80]].concat());
    let hdlr = full_box(b"hdlr", 0, &[&[0u8; 4][..], handler, &[0u8; 12], b"Planted Author\0"].concat());
    let dinf = bx(b"dinf", &full_box(b"dref", 0, &[&1u32.to_be_bytes()[..], &bx(b"url ", &[0, 0, 0, 1])].concat()));
    let stsd = full_box(b"stsd", 0, &[&1u32.to_be_bytes()[..], &bx(entry, &[0u8; 8])].concat());
    let stco = full_box(b"stco", 0, &[1u32.to_be_bytes(), chunk.to_be_bytes()].concat());
    let stsc = full_box(b"stsc", 0, &[1u32.to_be_bytes(), 1u32.to_be_bytes(), (sizes.len() as u32).to_be_bytes(), 1u32.to_be_bytes()].concat());
    let mut sz = [0u32.to_be_bytes(), (sizes.len() as u32).to_be_bytes()].concat();
    for s in sizes {
        sz.extend_from_slice(&s.to_be_bytes());
    }
    let stsz = full_box(b"stsz", 0, &sz);
    let stbl = bx(b"stbl", &[stsd, stco, stsc, stsz].concat());
    let minf = bx(b"minf", &[dinf, stbl].concat());
    let mdia = bx(b"mdia", &[full_box(b"mdhd", 0, &[0u8; 20]), hdlr, minf].concat());
    bx(b"trak", &[tkhd, mdia].concat())
}

/// A GoPro-style telemetry track (`gpmd`) and a timecode track are emptied with zeros and disabled; the video samples stay byte
/// for byte; the tracks are named; a telemetry track whose samples share bytes with the video refuses the file.
#[test]
fn scrub_mp4_telemetry_and_timecode_tracks_are_emptied_and_named_video_untouched() {
    let d = tmp_dir("mp4-tracks");
    let ftyp = bx(b"ftyp", b"isom\0\0\0\0isom");
    let video = b"VIDEODATA0123456".to_vec();
    let gps = [SERIAL, &coord_bytes(), b"GPS5"].concat();
    let tc = b"\x00\x0e\x11\x22".to_vec();
    let base = ftyp.len() as u32 + 8;
    let mdat = bx(b"mdat", &[&video[..], &gps, &tc].concat());
    let build = |gps_at: u32| {
        let moov = bx(
            b"moov",
            &[
                bx(b"mvhd", &[0u8; 100]),
                mp4_trak(b"vide", b"avc1", base, &[8, 8]),
                mp4_trak(b"meta", b"gpmd", gps_at, &[gps.len() as u32]),
                mp4_trak(b"tmcd", b"tmcd", base + 16 + gps.len() as u32, &[4]),
            ]
            .concat(),
        );
        [ftyp.clone(), mdat.clone(), moov].concat()
    };
    let file = build(base + 16);
    let src = d.join("a.mp4");
    fs::write(&src, &file).unwrap();
    assert!(contains(&file, SERIAL));
    let before = inspect(&src, &opts()).unwrap();
    assert!(before.findings.iter().any(|f| f.label.contains("GoPro telemetry")), "{:?}", before.findings);
    assert!(before.findings.iter().any(|f| f.label.contains("timecode")));
    let dst = clean_copy_name(&src);
    let out = clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert!(out.removed.iter().any(|f| f.label.contains("GoPro telemetry")));
    let cleaned = fs::read(&dst).unwrap();
    assert_eq!(cleaned.len(), file.len());
    assert_no_markers(&dst, "mp4 tracks");
    assert!(!contains(&cleaned, b"GPS5") && !contains(&cleaned, &tc));
    let v = base as usize;
    assert_eq!(&cleaned[v..v + 16], &video[..], "the video samples are byte for byte the same");
    assert_clean_listing(&dst);
    // a telemetry track that points into the video's bytes: refused, nothing written
    let bad = d.join("b.mp4");
    fs::write(&bad, build(base + 4)).unwrap();
    let dst2 = clean_copy_name(&bad);
    assert!(matches!(clean_to(&bad, &dst2, &opts(), &no_cancel), Err(ScrubError::Unsupported(_))));
    assert!(!dst2.exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_bmp_says_nothing_found_and_archives_are_refused() {
    let d = tmp_dir("bmp");
    let bmp = d.join("a.bmp");
    fs::write(&bmp, [b"BM".to_vec(), vec![0u8; 60]].concat()).unwrap();
    let i = inspect(&bmp, &opts()).unwrap();
    assert!(i.findings.is_empty() && i.notes.iter().any(|n| n.starts_with("Nothing found")));
    let z = d.join("a.zip");
    make_zip(&z, &[("a.txt", "x")]);
    let e = clean_to(&z, &clean_copy_name(&z), &opts(), &no_cancel).unwrap_err();
    assert!(e.to_string().contains("keeps the metadata of the files inside"), "{e}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_pdf_leaves_no_marker_anywhere_not_in_old_revisions_or_unreferenced_objects() {
    use lopdf::{Dictionary, Document, Object, Stream, dictionary};
    let d = tmp_dir("pdf-raw");
    let src = d.join("a.pdf");
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(Dictionary::new(), b"BT ET".to_vec()));
    let thumb = doc.add_object(Stream::new(dictionary! { "Subtype" => "Image" }, b"LEAFTESTSERIAL123".to_vec()));
    let annot = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()],
        "T" => Object::string_literal("Planted Author"), "M" => Object::string_literal("D:20261122142233"), "Contents" => Object::string_literal("note"),
    });
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Thumb" => thumb, "Annots" => vec![annot.into()],
        "PieceInfo" => dictionary! { "App" => dictionary! { "Private" => Object::string_literal("LEAFTESTSERIAL123") } },
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
    });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
    let xmp = doc.add_object(Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" }, b"<x:xmpmeta>Planted Author LEAFTESTSERIAL123</x:xmpmeta>".to_vec()));
    let attached = doc.add_object(Stream::new(
        dictionary! { "Type" => "EmbeddedFile", "Params" => dictionary! { "ModDate" => Object::string_literal("LEAFTESTSERIAL123") } },
        b"file".to_vec(),
    ));
    let spec = doc.add_object(dictionary! { "Type" => "Filespec", "F" => Object::string_literal("a.txt"), "EF" => dictionary! { "F" => attached } });
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id, "Metadata" => xmp,
        "Names" => dictionary! { "EmbeddedFiles" => dictionary! { "Names" => vec![Object::string_literal("a.txt"), spec.into()] } },
    });
    doc.trailer.set("Root", catalog);
    let info = doc.add_object(dictionary! { "Author" => Object::string_literal("Planted Author"), "Producer" => Object::string_literal("LEAFTESTSERIAL123") });
    doc.trailer.set("Info", info);
    doc.add_object(dictionary! { "Orphan" => Object::string_literal("Planted Author LEAFTESTSERIAL123") });
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    // an incremental update after the first revision, as an editor writes it: the first revision's Info stays in the file
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let prev: usize = text[text.rfind("startxref").unwrap() + 10..].lines().next().unwrap().trim().parse().unwrap();
    let new_id = doc.max_id + 1;
    bytes.push(b'\n');
    let off = bytes.len();
    bytes.extend_from_slice(format!("{new_id} 0 obj\n<< /Author (Planted Author) /Producer (LEAFTESTSERIAL123) >>\nendobj\n").as_bytes());
    let xoff = bytes.len();
    bytes.extend_from_slice(
        format!(
            "xref\n0 1\n0000000000 65535 f \n{new_id} 1\n{off:010} 00000 n \ntrailer\n<< /Size {} /Root {} 0 R /Info {new_id} 0 R /Prev {prev} >>\nstartxref\n{xoff}\n%%EOF\n",
            new_id + 1,
            catalog.0
        )
        .as_bytes(),
    );
    fs::write(&src, &bytes).unwrap();
    assert_markers_present(&src, "pdf");
    let before = inspect(&src, &opts()).unwrap();
    for want in ["Comment author names", "Comment dates", "Dates of attached files", "Earlier saved versions inside the file"] {
        assert!(before.findings.iter().any(|f| f.label == want), "{want} missing from {:?}", before.findings);
    }
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers(&dst, "pdf");
    assert_clean_listing(&dst);
    let again = Document::load(&dst).unwrap();
    assert_eq!(again.get_pages().len(), 1);
    let out = fs::read(&dst).unwrap();
    assert!(contains(&out, b"Contents") && contains(&out, b"(note)"), "the comment's text stays");
    let _ = fs::remove_dir_all(&d);
}

/// A signed PDF is refused by default; with the tick the signature dictionary (signer, time, place, reason, certificate), the
/// field's value and drawn appearance, the permissions and the security store go.
#[test]
fn scrub_signed_pdf_is_refused_unless_signatures_are_removed() {
    use lopdf::{Dictionary, Document, Object, Stream, dictionary};
    let d = tmp_dir("pdf-signed");
    let src = d.join("s.pdf");
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content = doc.add_object(Stream::new(Dictionary::new(), b"BT ET".to_vec()));
    let sig = doc.add_object(dictionary! {
        "Type" => "Sig", "Filter" => "Adobe.PPKLite", "SubFilter" => "adbe.pkcs7.detached",
        "Name" => Object::string_literal("Planted Author"), "M" => Object::string_literal("D:20261122142233"),
        "Location" => Object::string_literal("LEAFTESTSERIAL123"), "Reason" => Object::string_literal("approved"),
        "ByteRange" => vec![0.into(), 10.into(), 20.into(), 30.into()], "Contents" => Object::String(b"LEAFTESTSERIAL123 cert".to_vec(), lopdf::StringFormat::Hexadecimal),
    });
    let ap = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form" }, b"BT (Signed by Planted Author) Tj ET".to_vec()));
    let widget = doc.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget", "FT" => "Sig", "T" => Object::string_literal("Signature1"), "V" => sig,
        "Rect" => vec![0.into(), 0.into(), 10.into(), 10.into()], "AP" => dictionary! { "N" => ap },
    });
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content, "Annots" => vec![widget.into()],
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
    });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
    let dss_cert = doc.add_object(Stream::new(Dictionary::new(), b"certificate of Planted Author".to_vec()));
    let catalog = doc.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages_id,
        "AcroForm" => dictionary! { "Fields" => vec![widget.into()], "SigFlags" => 3 },
        "Perms" => dictionary! { "DocMDP" => sig },
        "DSS" => dictionary! { "Certs" => vec![dss_cert.into()] },
    });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    fs::write(&src, &bytes).unwrap();
    let i = inspect(&src, &opts()).unwrap();
    assert!(!i.can_clean && i.refusal.as_deref().unwrap().contains("signed"));
    assert!(i.findings.iter().any(|f| f.label.starts_with("Digital signature") && f.value.contains("Planted Author")), "{:?}", i.findings);
    let dst = clean_copy_name(&src);
    assert!(matches!(clean_to(&src, &dst, &opts(), &no_cancel), Err(ScrubError::Unsupported(_))));
    assert!(!dst.exists());
    let o = Options { remove_signatures: true, ..opts() };
    clean_to(&src, &dst, &o, &no_cancel).unwrap();
    assert_no_markers(&dst, "signed pdf");
    let again = Document::load(&dst).unwrap();
    assert_eq!(again.get_pages().len(), 1);
    let c = again.catalog().unwrap();
    assert!(!c.has(b"Perms") && !c.has(b"DSS"));
    assert!(inspect(&dst, &opts()).unwrap().can_clean, "no signature left: the cleaned file is not refused as signed");
    let _ = fs::remove_dir_all(&d);
}

/// Raw scan of the zip AND of every entry after it is unpacked (the entries are compressed, so the raw scan alone proves little).
fn assert_no_markers_in_zip(file: &Path, what: &str) {
    assert_no_markers(file, what);
    let mut z = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut e, &mut b).unwrap();
        for (name, m) in [("serial", SERIAL.to_vec()), ("author", AUTHOR.to_vec()), ("coordinate", coord_bytes())] {
            assert!(!contains(&b, &m), "{what}: the planted {name} is still inside the entry {}", e.name().unwrap());
        }
    }
}

fn zip_has_markers(file: &Path) -> bool {
    let mut z = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    (0..z.len()).any(|i| {
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut z.by_index(i).unwrap(), &mut b).unwrap();
        contains(&b, SERIAL) && contains(&b, AUTHOR) || contains(&b, AUTHOR)
    })
}

fn make_zip_bytes(path: &Path, entries: &[(&str, &[u8])]) {
    let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (n, c) in entries {
        z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(c).unwrap();
    }
    z.finish().unwrap();
}

fn zip_text(file: &Path, name: &str) -> String {
    let mut z = zip::ZipArchive::new(fs::File::open(file).unwrap()).unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut z.by_name(name).unwrap(), &mut s).unwrap();
    s
}

const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

#[test]
fn scrub_docx_xlsx_pptx_leave_no_marker_anywhere_in_any_entry() {
    let d = tmp_dir("office-raw");
    let planted_jpeg = jpeg_bytes(&exif_tiff_with(b"LEAFTESTSERIAL123\0", planted_lat()), b"Planted Author");
    let ct = "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>";
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"r1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"r2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail\" Target=\"docProps/thumbnail.jpeg\"/></Relationships>";
    let doc_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"p1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/printerSettings\" Target=\"printerSettings/printerSettings1.bin\"/><Relationship Id=\"i1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"media/image1.jpeg\"/></Relationships>";
    let settings_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/attachedTemplate\" Target=\"file:///C:/Users/LEAFTESTSERIAL123/t.dotm\" TargetMode=\"External\"/></Relationships>";
    let document = format!("<w:document {W}><w:body><w:p w:rsidR=\"00A1B2C3\" w:rsidRDefault=\"00A1B2C3\"><w:ins w:id=\"1\" w:author=\"Planted Author\" w:date=\"2026-11-22T14:22:00Z\"><w:r><w:t>hi</w:t></w:r></w:ins><w:del w:id=\"2\" w:author=\"Planted Author\" w:date=\"2026-11-22T14:23:00Z\"><w:r><w:delText>old</w:delText></w:r></w:del></w:p></w:body></w:document>");
    let comments = format!("<w:comments {W}><w:comment w:id=\"0\" w:author=\"Planted Author\" w:date=\"2026-11-22T14:22:00Z\" w:initials=\"PA\"><w:p><w:r><w:t>remember this</w:t></w:r></w:p></w:comment></w:comments>");
    let settings = format!("<w:settings {W} xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:rsids><w:rsidRoot w:val=\"00112233\"/><w:rsid w:val=\"00445566\"/></w:rsids><w:docVars><w:docVar w:name=\"who\" w:val=\"Planted Author\"/></w:docVars><w:attachedTemplate r:id=\"rId1\"/></w:settings>");
    let core = "<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:creator>Planted Author</dc:creator><cp:lastModifiedBy>Planted Author</cp:lastModifiedBy></cp:coreProperties>";
    let app = "<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\"><Company>LEAFTESTSERIAL123</Company><HyperlinkBase>C:\\Users\\Planted Author\\</HyperlinkBase></Properties>";
    let docx = d.join("a.docx");
    make_zip_bytes(
        &docx,
        &[
            ("[Content_Types].xml", ct.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            ("word/document.xml", document.as_bytes()),
            ("word/comments.xml", comments.as_bytes()),
            ("word/settings.xml", settings.as_bytes()),
            ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
            ("word/_rels/settings.xml.rels", settings_rels.as_bytes()),
            ("word/printerSettings/printerSettings1.bin", b"printer of Planted Author LEAFTESTSERIAL123"),
            ("word/media/image1.jpeg", &planted_jpeg),
            ("customXml/item1.xml", b"<root><who>LEAFTESTSERIAL123 Planted Author</who></root>"),
            ("docProps/core.xml", core.as_bytes()),
            ("docProps/app.xml", app.as_bytes()),
            ("docProps/thumbnail.jpeg", &planted_jpeg),
        ],
    );
    assert!(zip_has_markers(&docx));
    let before = inspect(&docx, &opts()).unwrap();
    for want in [CAT_AUTHOR_LABEL, "Dates (comments, tracked changes)", "Editing session numbers (rsid)", "Document variables", "Attached template path", "Printer settings", "Custom XML data"] {
        assert!(before.findings.iter().any(|f| f.label == want), "{want} missing from {:?}", before.findings);
    }
    assert!(before.findings.iter().any(|f| f.label.starts_with("Picture metadata")));
    let dst = clean_copy_name(&docx);
    clean_to(&docx, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers_in_zip(&dst, "docx");
    assert_clean_listing(&dst);
    assert!(zip_text(&dst, "word/document.xml").contains("<w:t>hi</w:t>") && zip_text(&dst, "word/comments.xml").contains("remember this"));
    assert!(zip_text(&dst, "word/document.xml").contains("w:author=\"Author\""));
    assert!(!zip_text(&dst, "word/_rels/document.xml.rels").contains("printerSettings"));
    assert!(!zip_text(&dst, "word/_rels/settings.xml.rels").contains("attachedTemplate"));
    roxmltree::Document::parse(&zip_text(&dst, "word/settings.xml")).unwrap();
    // the picture is still a JPEG that decodes, to the same pixels
    let mut z = zip::ZipArchive::new(fs::File::open(&dst).unwrap()).unwrap();
    let mut pic = Vec::new();
    std::io::Read::read_to_end(&mut z.by_name("word/media/image1.jpeg").unwrap(), &mut pic).unwrap();
    assert_eq!(::image::load_from_memory(&pic).unwrap().to_rgb8().into_raw(), ::image::load_from_memory(&planted_jpeg).unwrap().to_rgb8().into_raw());

    // spreadsheet: comment authors, persons
    let xlsx = d.join("b.xlsx");
    make_zip_bytes(
        &xlsx,
        &[
            ("[Content_Types].xml", ct.as_bytes()),
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/comments1.xml", b"<comments><authors><author>Planted Author</author></authors><commentList><comment ref=\"A1\" authorId=\"0\"><text><r><t>note</t></r></text></comment></commentList></comments>"),
            ("xl/persons/person.xml", b"<personList><person displayName=\"Planted Author\" id=\"{1}\" userId=\"LEAFTESTSERIAL123\" providerId=\"AD\"/></personList>"),
            ("xl/threadedComments/threadedComment1.xml", b"<ThreadedComments><threadedComment ref=\"A1\" dT=\"2026-11-22T14:22:33.00\" personId=\"{1}\" id=\"{2}\"><text>hello</text></threadedComment></ThreadedComments>"),
        ],
    );
    let dst = clean_copy_name(&xlsx);
    clean_to(&xlsx, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers_in_zip(&dst, "xlsx");
    assert!(zip_text(&dst, "xl/comments1.xml").contains("<t>note</t>"));
    assert_clean_listing(&dst);

    // presentation: comment authors
    let pptx = d.join("c.pptx");
    make_zip_bytes(
        &pptx,
        &[
            ("[Content_Types].xml", ct.as_bytes()),
            ("ppt/presentation.xml", b"<p:presentation xmlns:p=\"urn:p\"/>"),
            ("ppt/commentAuthors.xml", b"<p:cmAuthorLst xmlns:p=\"urn:p\"><p:cmAuthor id=\"0\" name=\"Planted Author\" initials=\"PA\" lastIdx=\"1\" clrIdx=\"0\"/></p:cmAuthorLst>"),
            ("ppt/comments/comment1.xml", b"<p:cmLst xmlns:p=\"urn:p\"><p:cm authorId=\"0\" dt=\"2026-11-22T14:22:33.000\" idx=\"1\"><p:text>look</p:text></p:cm></p:cmLst>"),
        ],
    );
    let dst = clean_copy_name(&pptx);
    clean_to(&pptx, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers_in_zip(&dst, "pptx");
    assert!(zip_text(&dst, "ppt/comments/comment1.xml").contains("<p:text>look</p:text>"));
    assert_clean_listing(&dst);
    let _ = fs::remove_dir_all(&d);
}

/// Excel: the folder path with the user name, the reservation name and the save ids of `xl/workbook.xml` go; a sheet's
/// `pageSetup r:id` loses the attribute when its printer settings go; every reference of the cleaned file resolves.
#[test]
fn scrub_xlsx_workbook_path_and_printer_reference_and_package_stays_consistent() {
    let d = tmp_dir("xlsx-wb");
    let ct = "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"bin\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings\"/><Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/><Override PartName=\"/docProps/thumbnail.jpeg\" ContentType=\"image/jpeg\"/></Types>";
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail\" Target=\"docProps/thumbnail.jpeg\"/></Relationships>";
    let wb_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
    let wb = "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:x15ac=\"http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac\" xmlns:xr=\"http://schemas.microsoft.com/office/spreadsheetml/2014/revision\" xmlns:xr10=\"http://schemas.microsoft.com/office/spreadsheetml/2016/revision10\" mc:Ignorable=\"x15ac xr xr10\"><fileVersion appName=\"xl\"/><fileSharing userName=\"Planted Author\"/><workbookPr/><mc:AlternateContent><mc:Choice Requires=\"x15\"><x15ac:absPath url=\"C:\\Users\\Planted Author\\Documents\\\"/></mc:Choice></mc:AlternateContent><xr:revisionPtr revIDLastSave=\"0\" documentId=\"8_{LEAFTESTSERIAL123}\" xr10:uidLastSave=\"{LEAFTESTSERIAL123}\"/><sheets><sheet name=\"S\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
    let sheet = "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>kept</t></is></c></row></sheetData><pageSetup orientation=\"portrait\" r:id=\"rId1\"/></worksheet>";
    let sheet_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/printerSettings\" Target=\"../printerSettings/printerSettings1.bin\"/></Relationships>";
    let src = d.join("a.xlsx");
    make_zip_bytes(
        &src,
        &[
            ("[Content_Types].xml", ct.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            ("xl/workbook.xml", wb.as_bytes()),
            ("xl/_rels/workbook.xml.rels", wb_rels.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
            ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels.as_bytes()),
            ("xl/printerSettings/printerSettings1.bin", b"printer of Planted Author"),
            ("docProps/thumbnail.jpeg", b"\xFF\xD8\xFF\xD9"),
        ],
    );
    let before = inspect(&src, &opts()).unwrap();
    for want in ["Folder path where the workbook was saved", "Name of the user who reserved the workbook", "Save and co-authoring ids (revisionPtr, uid)"] {
        assert!(before.findings.iter().any(|f| f.label == want), "{want} missing from {:?}", before.findings);
    }
    assert!(before.findings.iter().any(|f| f.value.contains("Planted Author\\Documents")), "the path itself is shown");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers_in_zip(&dst, "xlsx workbook");
    assert_clean_listing(&dst);
    let wb2 = zip_text(&dst, "xl/workbook.xml");
    assert!(!wb2.contains("absPath") && !wb2.contains("AlternateContent") && !wb2.contains("revisionPtr") && wb2.contains("<sheet name=\"S\""));
    roxmltree::Document::parse(&wb2).unwrap();
    let sh = zip_text(&dst, "xl/worksheets/sheet1.xml");
    assert!(sh.contains("<pageSetup orientation=\"portrait\"/>") && sh.contains("kept"), "{sh}");
    assert!(!zip_text(&dst, "xl/worksheets/_rels/sheet1.xml.rels").contains("rId1"));
    let types = zip_text(&dst, "[Content_Types].xml");
    assert!(!types.contains("thumbnail") && !types.contains("Extension=\"bin\"") && types.contains("sheet1.xml"), "{types}");
    assert!(office::broken_references(&dst).unwrap().is_empty(), "{:?}", office::broken_references(&dst));
    let _ = fs::remove_dir_all(&d);
}

/// A signed Office document is refused by default (removing metadata would break the signature); with the tick the signature
/// parts and their relationship go, and the signer's name with them.
#[test]
fn scrub_signed_docx_is_refused_unless_signatures_are_removed() {
    let d = tmp_dir("docx-signed");
    let ct = "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"sigs\" ContentType=\"application/vnd.openxmlformats-package.digital-signature-origin\"/><Override PartName=\"/_xmlsignatures/sig1.xml\" ContentType=\"application/vnd.openxmlformats-package.digital-signature-xmlsignature+xml\"/></Types>";
    let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"r1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"r9\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin\" Target=\"_xmlsignatures/origin.sigs\"/></Relationships>";
    let origin_rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"s1\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/signature\" Target=\"sig1.xml\"/></Relationships>";
    let sig = "<Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\"><KeyInfo><X509Data><X509SubjectName>CN=Planted Author, SERIALNUMBER=LEAFTESTSERIAL123</X509SubjectName></X509Data></KeyInfo></Signature>";
    let src = d.join("s.docx");
    make_zip_bytes(
        &src,
        &[
            ("[Content_Types].xml", ct.as_bytes()),
            ("_rels/.rels", rels.as_bytes()),
            ("word/document.xml", format!("<w:document {W}><w:body><w:p><w:r><w:t>signed text</w:t></w:r></w:p></w:body></w:document>").as_bytes()),
            ("_xmlsignatures/origin.sigs", b""),
            ("_xmlsignatures/_rels/origin.sigs.rels", origin_rels.as_bytes()),
            ("_xmlsignatures/sig1.xml", sig.as_bytes()),
        ],
    );
    let i = inspect(&src, &opts()).unwrap();
    assert!(!i.can_clean && i.refusal.as_deref().unwrap().contains("signed"));
    assert!(i.findings.iter().any(|f| f.label.starts_with("Digital signature") && f.value.contains("Planted Author")));
    let dst = clean_copy_name(&src);
    assert!(matches!(clean_to(&src, &dst, &opts(), &no_cancel), Err(ScrubError::Unsupported(_))));
    assert!(!dst.exists());
    let o = Options { remove_signatures: true, ..opts() };
    let out = clean_to(&src, &dst, &o, &no_cancel).unwrap();
    assert!(out.removed.iter().any(|f| f.label.starts_with("Digital signature")));
    assert_no_markers_in_zip(&dst, "signed docx");
    let mut z = zip::ZipArchive::new(fs::File::open(&dst).unwrap()).unwrap();
    assert!((0..z.len()).all(|i| !z.by_index(i).unwrap().name().unwrap().starts_with("_xmlsignatures")));
    assert!(!zip_text(&dst, "_rels/.rels").contains("digital-signature") && !zip_text(&dst, "[Content_Types].xml").contains("sig"));
    assert!(zip_text(&dst, "word/document.xml").contains("signed text"));
    assert!(office::broken_references(&dst).unwrap().is_empty());
    let _ = fs::remove_dir_all(&d);
}

const CAT_AUTHOR_LABEL: &str = "Author names (comments, tracked changes)";

#[test]
fn scrub_odt_leaves_no_marker_anywhere_in_any_entry() {
    let d = tmp_dir("odt-raw");
    let planted_jpeg = jpeg_bytes(&exif_tiff_with(b"LEAFTESTSERIAL123\0", planted_lat()), b"Planted Author");
    let ns = "xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:meta=\"urn:oasis:names:tc:opendocument:xmlns:meta:1.0\" xmlns:config=\"urn:oasis:names:tc:opendocument:xmlns:config:1.0\"";
    let content = format!("<office:document-content {ns}><office:body><office:text><text:tracked-changes><text:changed-region><text:insertion><office:change-info><dc:creator>Planted Author</dc:creator><dc:date>2026-11-22T14:22:33</dc:date></office:change-info></text:insertion></text:changed-region></text:tracked-changes><text:p>hello<office:annotation><dc:creator>Planted Author</dc:creator><dc:date>2026-11-22T14:22:33</dc:date><meta:creator-initials>PA</meta:creator-initials><text:p>a note</text:p></office:annotation></text:p></office:text></office:body></office:document-content>");
    let settings = format!("<office:document-settings {ns}><office:settings><config:config-item-set config:name=\"x\"><config:config-item config:name=\"PrinterName\" config:type=\"string\">LEAFTESTSERIAL123 printer</config:config-item><config:config-item config:name=\"PrinterSetup\" config:type=\"base64Binary\">UGxhbnRlZCBBdXRob3I=</config:config-item></config:config-item-set></office:settings></office:document-settings>");
    let meta = format!("<office:document-meta {ns} office:version=\"1.3\"><office:meta><meta:initial-creator>Planted Author</meta:initial-creator><dc:creator>Planted Author</dc:creator></office:meta></office:document-meta>");
    let manifest = "<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\"><manifest:file-entry manifest:full-path=\"/\" manifest:media-type=\"application/vnd.oasis.opendocument.text\"/><manifest:file-entry manifest:full-path=\"Thumbnails/thumbnail.png\" manifest:media-type=\"image/png\"/></manifest:manifest>";
    let src = d.join("a.odt");
    make_zip_bytes(
        &src,
        &[
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
            ("content.xml", content.as_bytes()),
            ("settings.xml", settings.as_bytes()),
            ("meta.xml", meta.as_bytes()),
            ("Pictures/a.jpg", &planted_jpeg),
            ("Thumbnails/thumbnail.png", b"Planted Author thumbnail"),
            ("META-INF/manifest.xml", manifest.as_bytes()),
        ],
    );
    assert!(zip_has_markers(&src));
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers_in_zip(&dst, "odt");
    assert_clean_listing(&dst);
    let c = zip_text(&dst, "content.xml");
    assert!(c.contains("a note") && c.contains("hello"), "the text of the note stays");
    roxmltree::Document::parse(&c).unwrap();
    roxmltree::Document::parse(&zip_text(&dst, "settings.xml")).unwrap();
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_jpeg_png_webp_leave_no_marker_anywhere_including_after_the_end_of_the_picture() {
    let d = tmp_dir("img-raw");
    // JPEG: Exif with serial and coordinate, XMP, IPTC, a comment, an unknown application segment and bytes after the end
    let tiff = exif_tiff_with(b"LEAFTESTSERIAL123\0", planted_lat());
    let mut j = jpeg_bytes(&tiff, b"Planted Author");
    let seg = |m: u8, body: &[u8]| {
        let mut v = vec![0xFF, m];
        v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(body);
        v
    };
    j.splice(2..2, [seg(0xED, b"Photoshop 3.0\0Planted Author"), seg(0xEC, b"Ducky LEAFTESTSERIAL123")].concat());
    j.extend_from_slice(b"trailer LEAFTESTSERIAL123 Planted Author");
    let src = d.join("a.jpg");
    fs::write(&src, &j).unwrap();
    assert_markers_present(&src, "jpeg");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers(&dst, "jpeg");
    assert_clean_listing(&dst);
    assert_eq!(pixels(&src), pixels(&dst));

    // PNG: text chunks of every kind, Exif, an unknown ancillary chunk
    let mut p = Vec::new();
    test_image().write_to(&mut std::io::Cursor::new(&mut p), ::image::ImageFormat::Png).unwrap();
    let mut extra = png_chunk(b"tEXt", b"Author\0Planted Author");
    extra.extend(png_chunk(b"iTXt", b"Comment\0\0\0\0\0LEAFTESTSERIAL123"));
    extra.extend(png_chunk(b"eXIf", &tiff));
    extra.extend(png_chunk(b"prVt", b"Planted Author"));
    p.splice(33..33, extra);
    p.extend_from_slice(b"after IEND LEAFTESTSERIAL123 Planted Author");
    let src = d.join("a.png");
    fs::write(&src, &p).unwrap();
    assert_markers_present(&src, "png");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers(&dst, "png");
    assert_clean_listing(&dst);
    assert_eq!(pixels(&src), pixels(&dst));

    // WebP: Exif, XMP, an unknown chunk
    let mut plain = Vec::new();
    test_image().write_to(&mut std::io::Cursor::new(&mut plain), ::image::ImageFormat::WebP).unwrap();
    let chunk = |t: &[u8; 4], b: &[u8]| {
        let mut c = t.to_vec();
        c.extend_from_slice(&(b.len() as u32).to_le_bytes());
        c.extend_from_slice(b);
        if b.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    let mut vp8x = vec![0x08 | 0x04, 0, 0, 0];
    vp8x.extend_from_slice(&(24u32 - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(16u32 - 1).to_le_bytes()[..3]);
    let mut body = b"WEBP".to_vec();
    body.extend(chunk(b"VP8X", &vp8x));
    body.extend_from_slice(&plain[12..]);
    body.extend(chunk(b"EXIF", &tiff));
    body.extend(chunk(b"XMP ", b"<x:xmpmeta>Planted Author</x:xmpmeta>"));
    body.extend(chunk(b"LEAF", b"LEAFTESTSERIAL123"));
    let mut w = b"RIFF".to_vec();
    w.extend_from_slice(&(body.len() as u32).to_le_bytes());
    w.extend(body);
    let src = d.join("a.webp");
    fs::write(&src, &w).unwrap();
    assert_markers_present(&src, "webp");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers(&dst, "webp");
    assert_clean_listing(&dst);
    assert_eq!(pixels(&src), pixels(&dst));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn scrub_mp3_leaves_no_marker_anywhere() {
    use lofty::config::WriteOptions;
    use lofty::tag::{Accessor, Tag, TagExt, TagType};
    let d = tmp_dir("mp3-raw");
    let src = d.join("a.mp3");
    let mut audio = Vec::new();
    for _ in 0..40 {
        let mut f = vec![0xFF, 0xFB, 0x90, 0x00];
        f.resize(417, 0);
        audio.extend(f);
    }
    fs::write(&src, &audio).unwrap();
    let mut t = Tag::new(TagType::Id3v2);
    t.set_artist("Planted Author".into());
    t.set_comment("LEAFTESTSERIAL123".into());
    t.save_to_path(&src, WriteOptions::default()).unwrap();
    assert_markers_present(&src, "mp3");
    let dst = clean_copy_name(&src);
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert_no_markers(&dst, "mp3");
    let _ = fs::remove_dir_all(&d);
}
