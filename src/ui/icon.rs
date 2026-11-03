//! The app mark (see `mark`) rendered with Direct2D for the tray and the window.

use std::cell::RefCell;

use windows::{
    Win32::{
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
            DeleteDC, DeleteObject, GetDC, HBITMAP, ReleaseDC, SelectObject,
        },
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO},
    },
    core::w,
};

use super::{canvas::Canvas, palette::Color};
pub use super::mark::{Mark, draw_mark};

thread_local! {
    static CANVAS: RefCell<Option<Canvas>> = const { RefCell::new(None) };
}

/// Releases the icon canvas. Call before the process exits: released by thread-local cleanup
/// during process shutdown, Direct2D tears down its software device after the thread pool is gone,
/// and the process fails fast.
pub fn release_canvas() {
    let taken = CANVAS.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.take()));
    drop(taken);
}

fn registry_dword(name: windows::core::PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: reads a DWORD into a local.
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
        .is_ok()
    };
    ok.then_some(value)
}

pub fn taskbar_is_dark() -> bool {
    registry_dword(w!("SystemUsesLightTheme")) != Some(1)
}

pub fn apps_prefer_dark() -> bool {
    registry_dword(w!("AppsUseLightTheme")) == Some(0)
}

/// Renders the mark to straight-alpha RGBA pixels.
pub fn render_rgba(size: u32, mark: Mark, seed: [f32; 3], dark_background: bool) -> Vec<u8> {
    let n = size as i32;
    let mut out = vec![0u8; (size * size * 4) as usize];
    CANVAS.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Canvas::new().ok();
            if let Some(c) = slot.as_mut() {
                let _ = c.attach_dc(96.0, false);
            }
        }
        let Some(canvas) = slot.as_ref() else { return };
        // SAFETY: a temporary top-down 32-bit DIB selected into a memory DC for one frame.
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: n,
                    biHeight: -n,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            ReleaseDC(None, screen);
            let Ok(bitmap) = bitmap else {
                let _ = DeleteDC(dc);
                return;
            };
            let previous = SelectObject(dc, bitmap.into());
            if canvas.bind_dc(dc, n, n) {
                canvas.begin();
                canvas.clear(Color::TRANSPARENT);
                draw_mark(canvas, 0.0, 0.0, size as f32, mark, seed, dark_background);
                canvas.end();
                let src = std::slice::from_raw_parts(bits as *const u8, out.len());
                for (px, dst) in src.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                    // Premultiplied BGRA to straight RGBA.
                    let a = px[3];
                    let un = |v: u8| if a == 0 { 0 } else { ((v as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8 };
                    dst.copy_from_slice(&[un(px[2]), un(px[1]), un(px[0]), a]);
                }
            }
            SelectObject(dc, previous);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
        }
    });
    out
}

/// An HICON for the window and taskbar, from straight-alpha RGBA pixels.
pub fn hicon(rgba: &[u8], size: u32) -> Option<HICON> {
    let n = size as i32;
    // SAFETY: builds a 32-bit color bitmap and an empty mask, then an icon from them.
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n,
                biHeight: -n,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let screen = GetDC(None);
        let mut bits = std::ptr::null_mut();
        let color: HBITMAP = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        ReleaseDC(None, screen);
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, rgba.len());
        for (src, d) in rgba.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            d.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
        }
        let mask_bytes = vec![0u8; ((n + 15) / 16 * 2 * n) as usize];
        let mask = CreateBitmap(n, n, 1, 1, Some(mask_bytes.as_ptr() as *const _));
        let icon_info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&icon_info).ok();
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_renders_opaque_pixels() {
        let rgba = render_rgba(32, Mark::Live, [255.0, 42.0, 138.0], true);
        let opaque = rgba.chunks_exact(4).filter(|p| p[3] > 200).count();
        assert!(opaque > 300, "only {opaque} opaque pixels");
        // The center column of the middle bar is the dark ink, the cap around it is light.
        let px = |x: usize, y: usize| &rgba[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        assert!(px(16, 20)[0] < 100, "{:?}", px(16, 20));
        assert!(px(4, 16)[0] > 150, "{:?}", px(4, 16));
    }
}

#[cfg(test)]
mod export {
    use super::*;

    /// Writes the app mark in the colors of the Purple preset as raw RGBA, one file per size, for
    /// `assets/`: `cargo test export_mark -- --ignored`.
    #[test]
    #[ignore]
    fn export_mark() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/icon");
        std::fs::create_dir_all(&dir).unwrap();
        let purple = crate::settings::Settings { color: "#9020ff".into(), ..Default::default() };
        let seed = crate::ui::seed_for(&purple);
        for size in [16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512] {
            std::fs::write(dir.join(format!("mark-{size}.rgba")), render_rgba(size, Mark::Live, seed, true)).unwrap();
        }
    }
}
