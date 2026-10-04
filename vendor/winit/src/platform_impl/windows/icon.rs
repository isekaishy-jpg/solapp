// Modified for Solapp: retain the currently applied custom cursor until native replacement.
use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;
use std::{fmt, io, mem};

use cursor_icon::CursorIcon;
use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleBitmap, DeleteObject, GetDC, ReleaseDC, SetBitmapBits,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIcon, CreateIconIndirect, DestroyCursor, DestroyIcon, GetCursor, LoadCursorW, LoadImageW, SendMessageW, SetCursor, HCURSOR,
    HICON, ICONINFO, ICON_BIG, ICON_SMALL, IDC_ARROW, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, WM_SETICON,
};

use crate::cursor::CursorImage;
use crate::dpi::PhysicalSize;
use crate::icon::*;

use super::util;

impl Pixel {
    fn convert_to_bgra(&mut self) {
        mem::swap(&mut self.r, &mut self.b);
    }
}

impl RgbaIcon {
    fn into_windows_icon(self) -> Result<WinIcon, BadIcon> {
        let rgba = self.rgba;
        let pixel_count = rgba.len() / PIXEL_SIZE;
        let mut and_mask = Vec::with_capacity(pixel_count);
        let pixels =
            unsafe { std::slice::from_raw_parts_mut(rgba.as_ptr() as *mut Pixel, pixel_count) };
        for pixel in pixels {
            and_mask.push(pixel.a.wrapping_sub(u8::MAX)); // invert alpha channel
            pixel.convert_to_bgra();
        }
        assert_eq!(and_mask.len(), pixel_count);
        let handle = unsafe {
            CreateIcon(
                0,
                self.width as i32,
                self.height as i32,
                1,
                (PIXEL_SIZE * 8) as u8,
                and_mask.as_ptr(),
                rgba.as_ptr(),
            )
        };
        if handle != 0 {
            Ok(WinIcon::from_handle(handle))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }
}

#[derive(Debug)]
pub enum IconType {
    Small = ICON_SMALL as isize,
    Big = ICON_BIG as isize,
}

#[derive(Debug)]
struct RaiiIcon {
    handle: HICON,
}

#[derive(Clone)]
pub struct WinIcon {
    inner: Arc<RaiiIcon>,
}

unsafe impl Send for WinIcon {}

impl WinIcon {
    pub fn as_raw_handle(&self) -> HICON {
        self.inner.handle
    }

    pub fn from_path<P: AsRef<Path>>(
        path: P,
        size: Option<PhysicalSize<u32>>,
    ) -> Result<Self, BadIcon> {
        // width / height of 0 along with LR_DEFAULTSIZE tells windows to load the default icon size
        let (width, height) = size.map(Into::into).unwrap_or((0, 0));

        let wide_path = util::encode_wide(path.as_ref());

        let handle = unsafe {
            LoadImageW(
                0,
                wide_path.as_ptr(),
                IMAGE_ICON,
                width,
                height,
                LR_DEFAULTSIZE | LR_LOADFROMFILE,
            )
        };
        if handle != 0 {
            Ok(WinIcon::from_handle(handle as HICON))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }

    pub fn from_resource(
        resource_id: u16,
        size: Option<PhysicalSize<u32>>,
    ) -> Result<Self, BadIcon> {
        Self::from_resource_ptr(resource_id as PCWSTR, size)
    }

    pub fn from_resource_name(
        resource_name: &str,
        size: Option<PhysicalSize<u32>>,
    ) -> Result<Self, BadIcon> {
        let wide_name = util::encode_wide(resource_name);
        Self::from_resource_ptr(wide_name.as_ptr(), size)
    }

    fn from_resource_ptr(
        resource: PCWSTR,
        size: Option<PhysicalSize<u32>>,
    ) -> Result<Self, BadIcon> {
        // width / height of 0 along with LR_DEFAULTSIZE tells windows to load the default icon size
        let (width, height) = size.map(Into::into).unwrap_or((0, 0));
        let handle = unsafe {
            LoadImageW(
                util::get_instance_handle(),
                resource,
                IMAGE_ICON,
                width,
                height,
                LR_DEFAULTSIZE,
            )
        };
        if handle != 0 {
            Ok(WinIcon::from_handle(handle as HICON))
        } else {
            Err(BadIcon::OsError(io::Error::last_os_error()))
        }
    }

    pub fn from_rgba(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, BadIcon> {
        let rgba_icon = RgbaIcon::from_rgba(rgba, width, height)?;
        rgba_icon.into_windows_icon()
    }

    pub fn set_for_window(&self, hwnd: HWND, icon_type: IconType) {
        unsafe {
            SendMessageW(hwnd, WM_SETICON, icon_type as usize, self.as_raw_handle());
        }
    }

    fn from_handle(handle: HICON) -> Self {
        Self { inner: Arc::new(RaiiIcon { handle }) }
    }
}

impl Drop for RaiiIcon {
    fn drop(&mut self) {
        unsafe { DestroyIcon(self.handle) };
    }
}

impl fmt::Debug for WinIcon {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        (*self.inner).fmt(formatter)
    }
}

pub fn unset_for_window(hwnd: HWND, icon_type: IconType) {
    unsafe {
        SendMessageW(hwnd, WM_SETICON, icon_type as usize, 0);
    }
}

#[derive(Debug, Clone)]
pub enum SelectedCursor {
    Named(CursorIcon),
    Custom(Arc<RaiiCursor>),
}

// Keep exactly the currently applied custom resource on its owning thread.
// Selected window state can be replaced while another HWND controls the cursor.
thread_local! {
    static APPLIED_CURSOR: std::cell::RefCell<Option<Arc<RaiiCursor>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn apply_selected_cursor(cursor: SelectedCursor) {
    let (handle, custom) = match cursor {
        SelectedCursor::Named(icon) => (unsafe { LoadCursorW(0, util::to_windows_cursor(icon)) }, None),
        SelectedCursor::Custom(cursor) => (cursor.as_raw_handle(), Some(cursor)),
    };
    apply_cursor(handle, custom);
}

fn apply_cursor(handle: HCURSOR, custom: Option<Arc<RaiiCursor>>) {
    unsafe { SetCursor(handle) };
    // Never release the previously applied custom until native replacement.
    if unsafe { GetCursor() } == handle {
        APPLIED_CURSOR.with(|slot| {
            let previous = slot.replace(custom);
            drop(previous);
        });
    }
}

pub(super) fn clear_applied_cursor() {
    let current = APPLIED_CURSOR.with(|slot| slot.take());
    if let Some(cursor) = current {
        if unsafe { GetCursor() } == cursor.as_raw_handle() {
            // LoadCursor supplies a shared system resource; null also replaces
            // the owned current cursor if that system lookup unexpectedly fails.
            let fallback = unsafe { LoadCursorW(0, IDC_ARROW) };
            unsafe { SetCursor(fallback) };
            if unsafe { GetCursor() } == cursor.as_raw_handle() {
                tracing::warn!("Could not replace the applied custom cursor during loop exit; retaining its resource");
                mem::forget(cursor);
                return;
            }
        }
        drop(cursor);
    }
}

impl Default for SelectedCursor {
    fn default() -> Self {
        Self::Named(Default::default())
    }
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub enum WinCursor {
    Cursor(Arc<RaiiCursor>),
    Failed,
}

impl WinCursor {
    pub(crate) fn new(image: &CursorImage) -> Result<Self, io::Error> {
        let mut bgra = image.rgba.clone();
        bgra.chunks_exact_mut(4).for_each(|chunk| chunk.swap(0, 2));

        let w = image.width as i32;
        let h = image.height as i32;

        unsafe {
            let hdc_screen = GetDC(0);
            if hdc_screen == 0 {
                return Err(io::Error::last_os_error());
            }
            let hbm_color = CreateCompatibleBitmap(hdc_screen, w, h);
            ReleaseDC(0, hdc_screen);
            if hbm_color == 0 {
                return Err(io::Error::last_os_error());
            }
            if SetBitmapBits(hbm_color, bgra.len() as u32, bgra.as_ptr() as *const c_void) == 0 {
                DeleteObject(hbm_color);
                return Err(io::Error::last_os_error());
            };

            // Mask created according to https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-createbitmap#parameters
            let mask_bits: Vec<u8> = vec![0xff; ((((w + 15) >> 4) << 1) * h) as usize];
            let hbm_mask = CreateBitmap(w, h, 1, 1, mask_bits.as_ptr() as *const _);
            if hbm_mask == 0 {
                DeleteObject(hbm_color);
                return Err(io::Error::last_os_error());
            }

            let icon_info = ICONINFO {
                fIcon: 0,
                xHotspot: image.hotspot_x as u32,
                yHotspot: image.hotspot_y as u32,
                hbmMask: hbm_mask,
                hbmColor: hbm_color,
            };

            let handle = CreateIconIndirect(&icon_info as *const _);
            DeleteObject(hbm_color);
            DeleteObject(hbm_mask);
            if handle == 0 {
                return Err(io::Error::last_os_error());
            }

            Ok(Self::Cursor(Arc::new(RaiiCursor { handle })))
        }
    }
}

#[derive(Debug, Hash, Eq, PartialEq)]
pub struct RaiiCursor {
    handle: HCURSOR,
}

impl Drop for RaiiCursor {
    fn drop(&mut self) {
        unsafe { DestroyCursor(self.handle) };
    }
}

impl RaiiCursor {
    pub fn as_raw_handle(&self) -> HICON {
        self.handle
    }
}

#[cfg(test)]
#[path = "../../../tests/windows_cursor_retention.rs"]
mod cursor_retention_tests;
