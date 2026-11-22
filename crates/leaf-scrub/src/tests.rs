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
    Options { keep_color_profile: true, keep_cover_art: false, reset_times: false }
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
    let make = b"Example Camera\0";
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
    v.extend_from_slice(&rat(48, 1));
    v.extend_from_slice(&rat(51, 1));
    v.extend_from_slice(&rat(30, 1));
    v
}

fn test_image() -> ::image::RgbImage {
    ::image::RgbImage::from_fn(24, 16, |x, y| ::image::Rgb([(x * 10) as u8, (y * 15) as u8, 90]))
}

fn make_jpeg(path: &Path) {
    let mut bytes = Vec::new();
    test_image().write_to(&mut std::io::Cursor::new(&mut bytes), ::image::ImageFormat::Jpeg).unwrap();
    // insert Exif, XMP and a comment right after the start marker
    let tiff = exif_tiff();
    let mut extra = Vec::new();
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(&tiff);
    extra.extend_from_slice(&[0xFF, 0xE1]);
    extra.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    extra.extend_from_slice(&exif);
    let xmp = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta><dc:creator>user</dc:creator></x:xmpmeta>";
    extra.extend_from_slice(&[0xFF, 0xE1]);
    extra.extend_from_slice(&((xmp.len() + 2) as u16).to_be_bytes());
    extra.extend_from_slice(xmp);
    let com = b"made by user";
    extra.extend_from_slice(&[0xFF, 0xFE]);
    extra.extend_from_slice(&((com.len() + 2) as u16).to_be_bytes());
    extra.extend_from_slice(com);
    bytes.splice(2..2, extra);
    fs::write(path, bytes).unwrap();
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
    clean_to(&src, &dst, &opts(), &no_cancel).unwrap();
    assert!(fs::read_to_string(format!("{}:Zone.Identifier", dst.display())).is_err(), "the copy must not carry the stream");
    assert_eq!(fs::read_to_string(&dst).unwrap(), "hello");
    // removing in place (the replace mode's last step)
    let gone = remove_streams(&src);
    assert!(gone.iter().any(|n| n.contains("Zone.Identifier")));
    assert!(fs::read_to_string(&stream).is_err());
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
