//! What each Blu-ray stream type becomes in Matroska: its codec id, its codec private data (from the first frames), and how a PES
//! payload is turned into frames.
//!
//! - H.264 / HEVC: Annex B to 4-byte length prefixes (access unit delimiters and filler dropped), `avcC` / `hvcC` from the first
//!   parameter sets; key frames are IDR / recovery point (H.264) and IRAP (HEVC) pictures.
//! - MPEG-2 / MPEG-1 video: the payload as it is, the sequence header (+ extension) as codec private.
//! - VC-1 (advanced profile): the payload as it is, BITMAPINFOHEADER + sequence header + entry point as codec private.
//! - AC-3, E-AC-3, DTS / DTS-HD, MPEG audio: the frames as they are; a core and its extension (two PES packets with one time stamp)
//!   are one frame.
//! - TrueHD: the AC-3 core Blu-ray interleaves is dropped, the access units are split, one frame each.
//! - LPCM: the 4-byte Blu-ray header cut, the big-endian samples turned little-endian, the padding channel dropped.
//! - PGS: the segments of one display set (up to its END segment) are one frame.

use crate::mkv::{Kind, TrackDef};
use rustdecoder::annexb::{avcc_from, nal_ranges};
use rustdecoder::bits::BitReader;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    Mpeg2,
    Mpeg1,
    Vc1,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    Lpcm,
    Mpa,
    Pgs,
}

impl Codec {
    /// The codecs leaf remuxes itself; `None`: needs ffmpeg (or is left out).
    pub fn of(stream_type: u8) -> Option<Codec> {
        Some(match stream_type {
            0x1B => Codec::H264,
            0x24 => Codec::Hevc,
            0x02 => Codec::Mpeg2,
            0x01 => Codec::Mpeg1,
            0xEA => Codec::Vc1,
            0x81 => Codec::Ac3,
            0x84 | 0x87 | 0xA1 => Codec::Eac3,
            0x82 | 0x85 | 0x86 | 0xA2 => Codec::Dts,
            0x83 => Codec::TrueHd,
            0x80 => Codec::Lpcm,
            0x03 | 0x04 => Codec::Mpa,
            0x90 => Codec::Pgs,
            _ => return None,
        })
    }

    pub fn kind(self) -> Kind {
        match self {
            Codec::H264 | Codec::Hevc | Codec::Mpeg2 | Codec::Mpeg1 | Codec::Vc1 => Kind::Video,
            Codec::Pgs => Kind::Subtitle,
            _ => Kind::Audio,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Codec::H264 => "V_MPEG4/ISO/AVC",
            Codec::Hevc => "V_MPEGH/ISO/HEVC",
            Codec::Mpeg2 => "V_MPEG2",
            Codec::Mpeg1 => "V_MPEG1",
            Codec::Vc1 => "V_MS/VFW/FOURCC",
            Codec::Ac3 => "A_AC3",
            Codec::Eac3 => "A_EAC3",
            Codec::Dts => "A_DTS",
            Codec::TrueHd => "A_TRUEHD",
            Codec::Lpcm => "A_PCM/INT/LIT",
            Codec::Mpa => "A_MPEG/L2",
            Codec::Pgs => "S_HDMV/PGS",
        }
    }
}

/// A short name of a stream type, for the track list.
pub fn type_name(stream_type: u8) -> &'static str {
    match stream_type {
        0x01 => "MPEG-1",
        0x02 => "MPEG-2",
        0x1B => "H.264",
        0x20 => "MVC (3D)",
        0x24 => "HEVC",
        0xEA => "VC-1",
        0x03 | 0x04 => "MPEG audio",
        0x0F | 0x11 => "AAC",
        0x80 => "LPCM",
        0x81 => "Dolby Digital",
        0x82 => "DTS",
        0x83 => "Dolby TrueHD",
        0x84 | 0x87 | 0xA1 => "Dolby Digital Plus",
        0x85 => "DTS-HD High Resolution",
        0x86 => "DTS-HD Master Audio",
        0xA2 => "DTS Express",
        0x90 => "PGS subtitles",
        0x91 => "Menu graphics",
        0x92 => "Text subtitles",
        _ => "Unknown",
    }
}

/// Enough of a stream's first frames to write its track header: `None` while more are needed.
pub fn track_def(codec: Codec, first: &[u8], stn_size: Option<(u32, u32)>, stn_audio: (u8, u8)) -> Option<TrackDef> {
    let mut t = TrackDef {
        kind: codec.kind(),
        codec_id: codec.id(),
        private: None,
        lang: String::new(),
        name: String::new(),
        default: false,
        size: stn_size.unwrap_or((1920, 1080)),
        audio: (crate::mpls::sample_rate(stn_audio.1), match stn_audio.0 {
            1 => 1,
            3 => 2,
            6 | 12 => 6,
            _ => 2,
        }, 0),
    };
    match codec {
        Codec::H264 => {
            t.private = Some(avcc_from(first)?);
            if let Some(s) = h264_size(first) {
                t.size = s;
            }
        }
        Codec::Hevc => {
            let (c, size) = hvcc(first)?;
            t.private = Some(c);
            t.size = size;
        }
        Codec::Mpeg2 | Codec::Mpeg1 => {
            let (c, size) = mpeg_seq(first)?;
            t.private = Some(c);
            t.size = size;
        }
        Codec::Vc1 => {
            let (c, size) = vc1_private(first)?;
            t.private = Some(c);
            t.size = size;
        }
        Codec::Ac3 | Codec::Eac3 => {
            if let Some((rate, ch)) = ac3_info(first) {
                t.audio = (rate, ch, 0);
            }
        }
        Codec::Dts => {
            if let Some((rate, ch)) = dts_info(first) {
                t.audio = (rate, ch, 0);
            }
        }
        Codec::TrueHd => {
            if let Some(rate) = truehd_rate(first) {
                t.audio.0 = rate;
            }
        }
        Codec::Lpcm => {
            let (rate, ch, bits) = lpcm_header(first)?;
            t.audio = (rate, ch, if bits == 16 { 16 } else { 24 });
        }
        Codec::Mpa | Codec::Pgs => {}
    }
    Some(t)
}

fn h264_size(data: &[u8]) -> Option<(u32, u32)> {
    let (s, e) = nal_ranges(data).into_iter().find(|&(s, e)| e > s + 4 && data[s] & 0x1F == 7)?;
    let rbsp = rustdecoder::nal::unescape(&data[s + 1..e]);
    let sps = rustdecoder::h264::sps(&mut BitReader::new(&rbsp)).ok()?;
    Some(sps.display_size())
}

/// `hvcC` with the profile fields of the SPS, and the picture size.
fn hvcc(data: &[u8]) -> Option<(Vec<u8>, (u32, u32))> {
    let mut sets: [Option<&[u8]>; 3] = [None; 3];
    for (s, e) in nal_ranges(data) {
        let u = &data[s..e];
        match (u[0] >> 1) & 0x3F {
            32 if sets[0].is_none() => sets[0] = Some(u),
            33 if sets[1].is_none() => sets[1] = Some(u),
            34 if sets[2].is_none() => sets[2] = Some(u),
            _ => {}
        }
    }
    let (vps, sps, pps) = (sets[0]?, sets[1]?, sets[2]?);
    let rbsp = rustdecoder::nal::unescape(sps);
    let mut b = BitReader::new(&rbsp);
    rustdecoder::hevc::nal_header(&mut b).ok()?;
    let s = rustdecoder::hevc::sps(&mut b).ok()?;
    let size = s.display_size();
    let ptl = rbsp.get(3..15)?;
    let mut c = vec![1];
    c.extend_from_slice(&ptl[..11]);
    c.push(ptl[11]);
    c.extend_from_slice(&[0xF0, 0, 0xFC, 0xFC | s.chroma_format_idc, 0xF8 | (s.bit_depth_luma - 8), 0xF8 | (s.bit_depth_chroma - 8), 0, 0, 0x0F]);
    c.push(3);
    for (ty, u) in [(32u8, vps), (33, sps), (34, pps)] {
        c.push(0x80 | ty);
        c.extend_from_slice(&1u16.to_be_bytes());
        c.extend_from_slice(&(u.len() as u16).to_be_bytes());
        c.extend_from_slice(u);
    }
    Some((c, size))
}

fn find_code(d: &[u8], from: usize, code: u8) -> Option<usize> {
    (from..d.len().saturating_sub(3)).find(|&i| d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 && d[i + 3] == code)
}

/// The next start code at or after `from` whose byte is not in `skip`.
fn next_code_not(d: &[u8], from: usize, skip: &[u8]) -> usize {
    let mut i = from;
    while i + 3 < d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 && !skip.contains(&d[i + 3]) {
            return i;
        }
        i += 1;
    }
    d.len()
}

fn mpeg_seq(d: &[u8]) -> Option<(Vec<u8>, (u32, u32))> {
    let s = find_code(d, 0, 0xB3)?;
    let h = d.get(s + 4..s + 7)?;
    let w = ((h[0] as u32) << 4) | (h[1] as u32 >> 4);
    let ht = ((h[1] as u32 & 0x0F) << 8) | h[2] as u32;
    let e = next_code_not(d, s + 4, &[0xB5, 0xB2]);
    Some((d[s..e].to_vec(), (w, ht)))
}

fn vc1_private(d: &[u8]) -> Option<(Vec<u8>, (u32, u32))> {
    let s = find_code(d, 0, 0x0F)?;
    let ep = find_code(d, s + 4, 0x0E)?;
    let e = next_code_not(d, ep + 4, &[]);
    let h = d.get(s + 4..s + 9)?;
    let w = (((h[2] as u32) << 4) | (h[3] as u32 >> 4)) * 2 + 2;
    let ht = ((((h[3] as u32) & 0x0F) << 8) | h[4] as u32) * 2 + 2;
    let extra = &d[s..e];
    let mut c = Vec::with_capacity(40 + extra.len());
    c.extend_from_slice(&(40 + extra.len() as u32).to_le_bytes());
    c.extend_from_slice(&(w as i32).to_le_bytes());
    c.extend_from_slice(&(ht as i32).to_le_bytes());
    c.extend_from_slice(&1u16.to_le_bytes());
    c.extend_from_slice(&24u16.to_le_bytes());
    c.extend_from_slice(b"WVC1");
    c.extend_from_slice(&(w * ht * 3).to_le_bytes());
    c.extend_from_slice(&[0; 16]);
    c.extend_from_slice(extra);
    Some((c, (w, ht)))
}

const AC3_CHANNELS: [u32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// Sample rate and channels of an AC-3 or E-AC-3 frame.
fn ac3_info(d: &[u8]) -> Option<(u32, u32)> {
    let i = (0..d.len().saturating_sub(8)).find(|&i| d[i] == 0x0B && d[i + 1] == 0x77)?;
    let f = &d[i..];
    let bsid = f[5] >> 3;
    if bsid <= 10 {
        let rate = [48_000, 44_100, 32_000, 48_000][(f[4] >> 6) as usize];
        let acmod = f[6] >> 5;
        // lfeon comes after the mix levels that this mode has
        let mut bit = 3u32;
        if acmod & 1 != 0 && acmod != 1 {
            bit += 2;
        }
        if acmod & 4 != 0 {
            bit += 2;
        }
        if acmod == 2 {
            bit += 2;
        }
        let byte = 6 + (bit / 8) as usize;
        let lfe = (f.get(byte)? >> (7 - bit % 8)) & 1;
        Some((rate, AC3_CHANNELS[acmod as usize] + lfe as u32))
    } else {
        let fscod = f[4] >> 6;
        let rate = match fscod {
            0 => 48_000,
            1 => 44_100,
            2 => 32_000,
            _ => [24_000, 22_050, 16_000, 48_000][((f[4] >> 4) & 3) as usize],
        };
        let acmod = (f[4] >> 1) & 7;
        let lfe = f[4] & 1;
        Some((rate, AC3_CHANNELS[acmod as usize] + lfe as u32))
    }
}

fn dts_info(d: &[u8]) -> Option<(u32, u32)> {
    let i = (0..d.len().saturating_sub(12)).find(|&i| d[i..i + 4] == [0x7F, 0xFE, 0x80, 0x01])?;
    let f = &d[i + 4..];
    let bits = |from: u32, n: u32| -> u32 {
        let mut v = 0u32;
        for k in from..from + n {
            v = (v << 1) | ((f[(k / 8) as usize] >> (7 - k % 8)) & 1) as u32;
        }
        v
    };
    let amode = bits(28, 6);
    let sfreq = bits(34, 4);
    let lff = bits(53, 2);
    const RATES: [u32; 16] = [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0];
    const CH: [u32; 16] = [1, 2, 2, 2, 2, 3, 3, 4, 4, 5, 6, 6, 6, 7, 8, 8];
    let rate = RATES[sfreq as usize];
    let ch = CH.get(amode as usize).copied().unwrap_or(6) + u32::from(lff != 0);
    Some((if rate == 0 { 48_000 } else { rate }, ch))
}

fn truehd_rate(d: &[u8]) -> Option<u32> {
    let i = (0..d.len().saturating_sub(10)).find(|&i| d[i..i + 4] == [0xF8, 0x72, 0x6F, 0xBA])?;
    Some(match d.get(i + 4)? >> 4 {
        0 => 48_000,
        1 => 96_000,
        2 => 192_000,
        8 => 44_100,
        9 => 88_200,
        10 => 176_400,
        _ => 48_000,
    })
}

/// (sample rate, channels, bits) of a Blu-ray LPCM PES payload's header.
fn lpcm_header(d: &[u8]) -> Option<(u32, u32, u32)> {
    let h = d.get(..4)?;
    let ch = [0, 1, 0, 2, 3, 3, 4, 4, 5, 6, 7, 8][(h[2] >> 4) as usize % 12];
    let rate = match h[2] & 0x0F {
        1 => 48_000,
        4 => 96_000,
        5 => 192_000,
        _ => return None,
    };
    let bits = match h[3] >> 6 {
        1 => 16,
        2 => 20,
        3 => 24,
        _ => return None,
    };
    (ch > 0).then_some((rate, ch, bits))
}

/// A frame of a video PES: the bytes Matroska stores and whether it is a key frame.
pub fn video_frame(codec: Codec, d: &[u8]) -> (Vec<u8>, bool) {
    match codec {
        Codec::H264 | Codec::Hevc => {
            let mut out = Vec::with_capacity(d.len() + 16);
            let mut key = false;
            for (s, e) in nal_ranges(d) {
                let u = &d[s..e];
                if codec == Codec::H264 {
                    match u[0] & 0x1F {
                        9 | 12 => continue,
                        5 => key = true,
                        // a recovery point SEI (Blu-ray's open GOP I pictures)
                        6 if u.get(1) == Some(&6) => key = true,
                        _ => {}
                    }
                } else {
                    match (u[0] >> 1) & 0x3F {
                        35 | 38 => continue,
                        16..=23 => key = true,
                        _ => {}
                    }
                }
                out.extend_from_slice(&(u.len() as u32).to_be_bytes());
                out.extend_from_slice(u);
            }
            (out, key)
        }
        Codec::Mpeg2 | Codec::Mpeg1 => (d.to_vec(), find_code(d, 0, 0xB3).is_some() || find_code(d, 0, 0xB8).is_some()),
        Codec::Vc1 => (d.to_vec(), find_code(d, 0, 0x0F).is_some() || find_code(d, 0, 0x0E).is_some()),
        _ => (d.to_vec(), true),
    }
}

/// TrueHD: the payload's own access units (the AC-3 frames of the core are left out).
pub fn truehd_units(d: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= d.len() {
        if d[i] == 0x0B && d[i + 1] == 0x77 {
            // an AC-3 frame: its size from frmsizecod
            let Some(len) = ac3_frame_len(&d[i..]) else { break };
            i += len;
            continue;
        }
        let len = ((((d[i] as usize) & 0x0F) << 8) | d[i + 1] as usize) * 2;
        if len < 4 || i + len > d.len() {
            break;
        }
        out.push(&d[i..i + len]);
        i += len;
    }
    out
}

fn ac3_frame_len(f: &[u8]) -> Option<usize> {
    const WORDS_48K: [u16; 19] = [64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1152, 1280];
    let b = *f.get(4)?;
    if f.get(5)? >> 3 > 10 {
        // E-AC-3: frmsiz in words, minus one
        return Some((((f[2] as usize & 7) << 8 | f[3] as usize) + 1) * 2);
    }
    let code = (b & 0x3F) as usize;
    let words = *WORDS_48K.get(code / 2)? as usize;
    Some(match b >> 6 {
        0 => words * 2,
        1 => (words * 480 / 441 + (code & 1)) * 2,
        2 => words * 3,
        _ => return None,
    })
}

/// LPCM: the samples of one PES in Matroska's little-endian form, the padding channel left out.
pub fn lpcm_frame(d: &[u8]) -> Option<Vec<u8>> {
    let (_, ch, bits) = lpcm_header(d)?;
    let stored = (ch as usize).div_ceil(2) * 2;
    let width = if bits == 16 { 2 } else { 3 };
    let body = &d[4..];
    let frame = stored * width;
    let mut out = Vec::with_capacity(body.len());
    for f in body.chunks_exact(frame) {
        for c in 0..ch as usize {
            let s = &f[c * width..c * width + width];
            out.extend(s.iter().rev());
        }
    }
    Some(out)
}

/// PGS: the type of each segment in a payload.
pub fn pgs_ends_display_set(d: &[u8]) -> bool {
    let mut i = 0;
    let mut end = false;
    while i + 3 <= d.len() {
        end = d[i] == 0x80;
        i += 3 + u16::from_be_bytes([d[i + 1], d[i + 2]]) as usize;
    }
    end
}

