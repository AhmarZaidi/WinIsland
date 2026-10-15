use std::cell::RefCell;
use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC,
    SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_TOPMOST,
    IDC_ARROW, LoadCursorW, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow, ULW_ALPHA,
    UpdateLayeredWindow, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

static REGISTER_CLASS: Once = Once::new();
const CORNER_CLASS_NAME: windows::core::PCWSTR = w!("WinIsland_ScreenCorner");

thread_local! {
    static CORNERS: RefCell<ScreenCornersManager> = const { RefCell::new(ScreenCornersManager::new()) };
}

pub(crate) fn update_corners(
    enabled: bool,
    radius: f32,
    monitor_pos: (i32, i32),
    monitor_size: (u32, u32),
) {
    CORNERS.with(|corners| {
        corners
            .borrow_mut()
            .update(enabled, radius, monitor_pos, monitor_size);
    });
}

pub(crate) fn enforce_topmost() {
    CORNERS.with(|corners| {
        corners.borrow().enforce_topmost();
    });
}

#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

struct ScreenCornersManager {
    corners: Option<[HWND; 4]>,
    last_enabled: bool,
    last_radius: u32,
    last_pos: (i32, i32),
    last_size: (u32, u32),
}

impl ScreenCornersManager {
    const fn new() -> Self {
        Self {
            corners: None,
            last_enabled: false,
            last_radius: 0,
            last_pos: (0, 0),
            last_size: (0, 0),
        }
    }

    fn update(
        &mut self,
        enabled: bool,
        radius: f32,
        monitor_pos: (i32, i32),
        monitor_size: (u32, u32),
    ) {
        if !enabled || radius < 1.0 || monitor_size.0 == 0 || monitor_size.1 == 0 {
            if self.last_enabled {
                self.hide();
                self.last_enabled = false;
            }
            return;
        }

        let r = (radius.round() as u32).clamp(1, 100);
        if self.last_enabled
            && self.last_radius == r
            && self.last_pos == monitor_pos
            && self.last_size == monitor_size
        {
            return;
        }

        let hwnds = match self.corners {
            Some(hwnds) => hwnds,
            None => {
                let Some(hwnds) = create_corner_windows() else {
                    return;
                };
                self.corners = Some(hwnds);
                hwnds
            }
        };

        let (mon_x, mon_y) = monitor_pos;
        let (mon_w, mon_h) = (monitor_size.0 as i32, monitor_size.1 as i32);
        let r_i32 = r as i32;

        let positions = [
            (mon_x, mon_y),
            (mon_x + mon_w - r_i32, mon_y),
            (mon_x, mon_y + mon_h - r_i32),
            (mon_x + mon_w - r_i32, mon_y + mon_h - r_i32),
        ];

        let corner_types = [
            Corner::TopLeft,
            Corner::TopRight,
            Corner::BottomLeft,
            Corner::BottomRight,
        ];

        for i in 0..4 {
            let pixels = generate_corner_pixels(corner_types[i], r);
            apply_corner_bitmap(hwnds[i], positions[i].0, positions[i].1, r, &pixels);
        }

        self.last_enabled = true;
        self.last_radius = r;
        self.last_pos = monitor_pos;
        self.last_size = monitor_size;
    }

    fn hide(&self) {
        if let Some(hwnds) = &self.corners {
            for &hwnd in hwnds {
                // SAFETY: hwnd belongs to this thread and is hidden without destroying.
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
    }

    fn enforce_topmost(&self) {
        if self.last_enabled
            && let Some(hwnds) = &self.corners
        {
            for &hwnd in hwnds {
                // SAFETY: Updating Z-order to topmost without moving, resizing, or activating.
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
        }
    }
}

impl Drop for ScreenCornersManager {
    fn drop(&mut self) {
        if let Some(hwnds) = self.corners.take() {
            for hwnd in hwnds {
                // SAFETY: Destroying owned HWND on event-loop thread.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }
}

unsafe extern "system" fn corner_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    // SAFETY: Forwarding unhandled messages to standard default window procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn ensure_corner_class() {
    REGISTER_CLASS.call_once(|| {
        // SAFETY: Registering a standard window class for corner overlays.
        unsafe {
            let hinstance = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(corner_wnd_proc),
                hInstance: hinstance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CORNER_CLASS_NAME,
                ..Default::default()
            };
            let _ = RegisterClassExW(&wc);
        }
    });
}

fn create_corner_windows() -> Option<[HWND; 4]> {
    ensure_corner_class();
    let mut hwnds = [HWND::default(); 4];
    for slot in &mut hwnds {
        // SAFETY: Creating a layered, transparent, topmost popup window.
        let hwnd = unsafe {
            let hinstance = GetModuleHandleW(None).unwrap_or_default();
            let ex_style = WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_TOOLWINDOW
                | WS_EX_TOPMOST
                | WS_EX_NOACTIVATE;
            CreateWindowExW(
                ex_style,
                CORNER_CLASS_NAME,
                w!("WinIsland Screen Corner"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .ok()?
        };
        *slot = hwnd;
    }
    Some(hwnds)
}

fn generate_corner_pixels(corner: Corner, radius: u32) -> Vec<u8> {
    let r = radius as f32;
    let mut pixels = vec![0u8; (radius * radius * 4) as usize];

    for y in 0..radius {
        for x in 0..radius {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            let (dx, dy) = match corner {
                Corner::TopLeft => (r - px, r - py),
                Corner::TopRight => (px, r - py),
                Corner::BottomLeft => (r - px, py),
                Corner::BottomRight => (px, py),
            };

            let dist = (dx * dx + dy * dy).sqrt();
            let coverage = (dist - (r - 0.5)).clamp(0.0, 1.0);
            let alpha = (coverage * 255.0).round() as u8;

            let idx = ((y * radius + x) * 4) as usize;
            pixels[idx] = 0;
            pixels[idx + 1] = 0;
            pixels[idx + 2] = 0;
            pixels[idx + 3] = alpha;
        }
    }

    pixels
}

fn apply_corner_bitmap(hwnd: HWND, x: i32, y: i32, radius: u32, pixels: &[u8]) {
    // SAFETY: Interacting with validated GDI DCs and layered window handles.
    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return;
        }
        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        if hdc_mem.is_invalid() {
            let _ = ReleaseDC(None, hdc_screen);
            return;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: radius as i32,
                biHeight: -(radius as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(Some(hdc_mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);

        if let Ok(hbmp) = hbmp
            && !bits.is_null()
        {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
            let old_bmp = SelectObject(hdc_mem, hbmp.into());

            let pt_src = POINT { x: 0, y: 0 };
            let pt_dst = POINT { x, y };
            let sz = SIZE {
                cx: radius as i32,
                cy: radius as i32,
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };

            let _ = UpdateLayeredWindow(
                hwnd,
                Some(hdc_screen),
                Some(&pt_dst),
                Some(&sz),
                Some(hdc_mem),
                Some(&pt_src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );

            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                radius as i32,
                radius as i32,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            let _ = SelectObject(hdc_mem, old_bmp);
            let _ = DeleteObject(hbmp.into());
        }

        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(None, hdc_screen);
    }
}
