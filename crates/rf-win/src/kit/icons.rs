//! An icon as BGRA: one stored in a file (an `.exe`, `.dll` or `.ico` with an index) and an `HICON` of the system.

use std::path::Path;

use crate::thumbs::Bgra;

/// An icon stored in a file, at the size asked for (`PrivateExtractIconsW` picks the frame closest to it).
pub fn extract_resource(file: &Path, index: i32, px: u32) -> Option<Bgra> {
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON, PrivateExtractIconsW};
    let w = crate::wide::wide(file);
    let mut buf = [0u16; 260];
    let n = w.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&w[..n]);
    let mut icons = [HICON::default()];
    let got = unsafe { PrivateExtractIconsW(&buf, index, px as i32, px as i32, Some(&mut icons), None, 0) };
    if got == 0 || got == u32::MAX || icons[0].is_invalid() {
        return None;
    }
    let r = hicon_to_bgra(icons[0]);
    unsafe {
        let _ = DestroyIcon(icons[0]);
    }
    r
}

/// An `HICON` as straight-alpha BGRA at its own size. An icon without an alpha channel gets its transparency from its mask.
pub fn hicon_to_bgra(icon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<Bgra> {
    use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HGDIOBJ, ReleaseDC};
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(icon, &mut ii).ok()?;
        let free = |ii: &ICONINFO| {
            if !ii.hbmColor.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
            }
            if !ii.hbmMask.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
            }
        };
        if ii.hbmColor.is_invalid() {
            // a monochrome icon (colour and mask stacked in one bitmap): not worth drawing
            free(&ii);
            return None;
        }
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(ii.hbmColor.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
            free(&ii);
            return None;
        }
        let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
        let read = |bmp: windows::Win32::Graphics::Gdi::HBITMAP| -> Option<Vec<u8>> {
            let mut data = vec![0u8; (w * h * 4) as usize];
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = GetDC(None);
            let lines = GetDIBits(dc, bmp, 0, h, Some(data.as_mut_ptr() as *mut _), &mut bmi, DIB_RGB_COLORS);
            ReleaseDC(None, dc);
            (lines as u32 == h).then_some(data)
        };
        let color = read(ii.hbmColor);
        let mask = if ii.hbmMask.is_invalid() { None } else { read(ii.hbmMask) };
        free(&ii);
        let mut data = color?;
        if data.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
            // no alpha channel: opaque where the mask is black
            for (i, p) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let opaque = mask.as_ref().is_none_or(|m| m[i * 4] == 0);
                p[3] = if opaque { 255 } else { 0 };
            }
        }
        Some(Bgra { width: w, height: h, data })
    }
}
