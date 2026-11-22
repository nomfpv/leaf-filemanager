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
