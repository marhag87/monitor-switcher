//! A flyout like the one Windows shows when you change the volume: a small
//! panel at the bottom of the screen that appears for a moment and fades.
//!
//! It exists because the alternative, a notification, is the wrong tool for
//! "you just pressed a hotkey, here is what it did". Do Not Disturb holds
//! notifications back — and Windows turns that on by itself in a full-screen
//! game, which is exactly when a toggle like activate-on-hover gets pressed —
//! and they pile up in the notification centre afterwards. Windows' own flyouts
//! have no API, so this is our own window drawn to look like one.
//!
//! The window must never take focus. Activating anything over a game can drop
//! it out of full screen or minimise it, so it is created no-activate, shown
//! no-activate, and transparent to the mouse — which also keeps it out of
//! activate-on-hover's way, the setting it is most often reporting on. Being
//! topmost, it shows over borderless-windowed games; a game in true exclusive
//! full screen owns the display, and nothing drawn by another window appears
//! over it, Windows' own flyouts included.
//!
//! Everything here runs on the tray's UI thread, which owns the window and the
//! timer that animates it.

mod paint;

pub use paint::DEFAULT_FROM_BOTTOM_PERCENT;

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GdiFlush,
    GetMonitorInfoW, GetTextExtentPoint32W, MonitorFromPoint, SelectObject, SetBkMode,
    SetTextColor, TextOutW, AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS,
    FW_NORMAL, HBITMAP, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, OUT_DEFAULT_PRECIS,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, KillTimer, RegisterClassW, SetTimer,
    SetWindowPos, ShowWindow, UpdateLayeredWindow, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, ULW_ALPHA, WM_MOUSEACTIVATE,
    WM_NCHITTEST, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::notify::Notice;

const TIMER: usize = 1;
/// About 60 frames a second, which is what makes a fade look like one.
const FRAME_MS: u32 = 16;

struct Flyout {
    hwnd: HWND,
    /// When the current showing started, or `None` while hidden.
    shown: Option<Instant>,
}

thread_local! {
    static FLYOUT: RefCell<Option<Flyout>> = const { RefCell::new(None) };
}

/// Show a notice, centred on the monitor the mouse is on, with its bottom edge
/// `from_bottom_percent` of the way up the screen. A notice arriving while the
/// last is still up replaces it in place and starts its timer over, so pressing
/// a toggle twice shows where it ended up rather than a queue.
pub fn show(notice: &Notice, from_bottom_percent: f32) -> Result<()> {
    // Per-monitor aware for everything below, the window included: it is
    // created on first use, inside this. Unaware, Windows would draw the panel
    // at 96 DPI and stretch it — blurry on any scaled display.
    let _dpi = ThreadDpi::per_monitor();

    let hwnd = window()?;

    let mut cursor = POINT::default();
    // SAFETY: plain out-parameter calls.
    let monitor = unsafe {
        GetCursorPos(&mut cursor).context("finding the mouse")?;
        MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: cbSize matches the struct passed.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        bail!("could not read the monitor's work area");
    }
    let (mut dpi, mut unused) = (96u32, 96u32);
    // SAFETY: out-parameters only. On failure the 96 stands, which is right
    // for an unscaled display and merely small on a scaled one.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused) };
    let scale = dpi as f32 / 96.0;

    let light = system_uses_light_theme();
    let theme = paint::Theme::new(light, accent(light));
    let label = &notice.label;
    let state = if notice.on { "On" } else { "Off" };

    // SAFETY: the DC and the objects drawn with it are released by the guards
    // below. The font is declared first so that it outlives the DC: deleting a
    // DC with an object still selected is fine, deleting a selected object is
    // not, and an early return could otherwise leave it selected.
    unsafe {
        let font = Object(
            CreateFontW(
                -paint::font_px(scale),
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                // Greyscale anti-aliasing, not ClearType: the text is used as
                // an alpha mask, and ClearType's colour fringes would come out
                // as coloured edges.
                ANTIALIASED_QUALITY,
                0,
                w!("Segoe UI"),
            )
            .into(),
        );
        let dc = Dc::new()?;
        let old_font = SelectObject(dc.0, font.0);

        let label_size = measure(dc.0, label);
        let state_w = measure(dc.0, "On").0.max(measure(dc.0, "Off").0);
        let state_size = measure(dc.0, state);
        let layout = paint::layout(scale, label_size, state_w, label_size.1);

        let mut canvas = paint::draw(&layout, &theme, scale, notice.on);
        let (lx, ly) = layout.label;
        let mask = text_mask(dc.0, label, label_size)?;
        canvas.blend_mask(
            lx,
            ly,
            &mask,
            label_size.0 as usize,
            label_size.1 as usize,
            theme.text,
        );
        let mask = text_mask(dc.0, state, state_size)?;
        canvas.blend_mask(
            layout.state_right - state_size.0,
            layout.state_top,
            &mask,
            state_size.0 as usize,
            state_size.1 as usize,
            theme.text,
        );
        SelectObject(dc.0, old_font);

        let (bitmap, bits) = dib(dc.0, layout.width, layout.height)?;
        let bitmap = Object(bitmap.into());
        let pixels = canvas.to_bgra();
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits, pixels.len());
        let old_bitmap = SelectObject(dc.0, bitmap.0);

        let work = info.rcWork;
        let at = POINT {
            x: (work.left + work.right) / 2 - layout.width / 2,
            y: paint::panel_top(
                work.top,
                work.bottom,
                paint::lift_px(scale),
                from_bottom_percent,
                (layout.panel.y1 - layout.panel.y0) as i32,
            ) - layout.panel.y0 as i32,
        };
        let size = SIZE {
            cx: layout.width,
            cy: layout.height,
        };

        // Already up: swap the contents at full strength rather than fading
        // in again from nothing, which would read as a flicker.
        let now = Instant::now();
        let start = FLYOUT.with(|f| {
            let visible = f.borrow().as_ref().is_some_and(|f| f.shown.is_some());
            if visible {
                now - std::time::Duration::from_millis(paint::FADE_IN_MS)
            } else {
                now
            }
        });
        let opacity = paint::opacity(now.duration_since(start).as_millis() as u64).unwrap_or(0.0);

        let result = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&at),
            Some(&size),
            Some(dc.0),
            Some(&POINT::default()),
            COLORREF(0),
            Some(&blend(opacity)),
            ULW_ALPHA,
        );
        SelectObject(dc.0, old_bitmap);
        result.context("drawing the flyout")?;

        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
        .context("showing the flyout")?;

        FLYOUT.with(|f| {
            if let Some(f) = f.borrow_mut().as_mut() {
                f.shown = Some(start);
            }
        });
        SetTimer(Some(hwnd), TIMER, FRAME_MS, None);
    }
    Ok(())
}

/// The flyout's window, created on first use.
fn window() -> Result<HWND> {
    if let Some(hwnd) = FLYOUT.with(|f| f.borrow().as_ref().map(|f| f.hwnd)) {
        return Ok(hwnd);
    }

    let class = w!("monitor-switcher-flyout");
    // SAFETY: registering a class whose strings are literals, and creating one
    // window of it; the window procedure touches only thread-local state.
    let hwnd = unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            bail!("could not register the flyout's window class");
        }
        CreateWindowExW(
            // Layered for per-pixel alpha; transparent so the mouse passes
            // straight through; no-activate so it never takes focus; tool
            // window so it never appears in the taskbar or Alt+Tab.
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
        .context("creating the flyout window")?
    };

    FLYOUT.with(|f| *f.borrow_mut() = Some(Flyout { hwnd, shown: None }));
    Ok(hwnd)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == TIMER => {
            tick(hwnd);
            LRESULT(0)
        }
        // Both belt and braces over the extended styles: never activate, and
        // never be the thing under the mouse.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// One animation frame: set the opacity for now, or hide once faded out.
unsafe fn tick(hwnd: HWND) {
    let shown = FLYOUT.with(|f| f.try_borrow().ok().and_then(|f| f.as_ref()?.shown));
    let opacity = shown.and_then(|at| paint::opacity(at.elapsed().as_millis() as u64));
    match opacity {
        // No source DC: the contents stay as they are and only the blend
        // changes, which is all a fade is.
        Some(a) => {
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                None,
                None,
                None,
                None,
                COLORREF(0),
                Some(&blend(a)),
                ULW_ALPHA,
            );
        }
        None => {
            let _ = KillTimer(Some(hwnd), TIMER);
            let _ = ShowWindow(hwnd, SW_HIDE);
            FLYOUT.with(|f| {
                if let Ok(mut f) = f.try_borrow_mut() {
                    if let Some(f) = f.as_mut() {
                        f.shown = None;
                    }
                }
            });
        }
    }
}

fn blend(opacity: f32) -> BLENDFUNCTION {
    BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
        AlphaFormat: AC_SRC_ALPHA as u8,
    }
}

/// Width and height of a line of text in the selected font.
unsafe fn measure(dc: HDC, text: &str) -> (i32, i32) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(dc, &wide, &mut size);
    (size.cx, size.cy)
}

/// Render text white-on-black in the DC's font and return its coverage, one
/// byte per pixel.
unsafe fn text_mask(dc: HDC, text: &str, (w, h): (i32, i32)) -> Result<Vec<u8>> {
    if w <= 0 || h <= 0 {
        return Ok(Vec::new());
    }
    let (bitmap, bits) = dib(dc, w, h)?;
    let bitmap = Object(bitmap.into());
    let old = SelectObject(dc, bitmap.0);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, COLORREF(0x00FF_FFFF));
    let wide: Vec<u16> = text.encode_utf16().collect();
    let _ = TextOutW(dc, 0, 0, &wide);
    // GDI may batch drawing; the bits are read directly next.
    let _ = GdiFlush();

    let len = (w * h) as usize;
    let pixels = std::slice::from_raw_parts(bits, len * 4);
    // Greyscale anti-aliasing writes the same value to all three channels;
    // green is as good as any.
    let mask = (0..len).map(|i| pixels[i * 4 + 1]).collect();
    SelectObject(dc, old);
    Ok(mask)
}

/// A 32-bit top-down DIB section, zeroed, and a pointer to its pixels.
unsafe fn dib(dc: HDC, w: i32, h: i32) -> Result<(HBITMAP, *mut u8)> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            // Negative: rows run top to bottom, matching the canvas.
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        .context("allocating the flyout's bitmap")?;
    if bits.is_null() {
        let _ = DeleteObject(bitmap.into());
        bail!("the flyout's bitmap came back without pixels");
    }
    Ok((bitmap, bits as *mut u8))
}

/// Whether the taskbar and Windows' own flyouts are in light mode. That is the
/// "system" half of the colour mode, as opposed to the one apps follow, and the
/// one a flyout should match.
fn system_uses_light_theme() -> bool {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the buffer and its size describe `value`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    // Absent means the Windows 11 default, which is dark.
    status.is_ok() && value != 0
}

/// The system accent colour, in the shade Windows uses for a toggle in this
/// mode. `None` falls back to Windows' default blue.
fn accent(light: bool) -> Option<[u8; 3]> {
    let mut palette = [0u8; 64];
    let mut size = palette.len() as u32;
    // SAFETY: the buffer and its size describe `palette`.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent"),
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(palette.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    paint::accent_from_palette(&palette[..size as usize], light)
}

/// A memory DC, deleted on drop.
struct Dc(HDC);

impl Dc {
    unsafe fn new() -> Result<Self> {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            bail!("could not create a drawing context");
        }
        Ok(Dc(dc))
    }
}

impl Drop for Dc {
    fn drop(&mut self) {
        // SAFETY: created by CreateCompatibleDC and deleted once, here. Every
        // object selected into it has been selected back out by then.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// A GDI object — font or bitmap — deleted on drop.
struct Object(HGDIOBJ);

impl Drop for Object {
    fn drop(&mut self) {
        // SAFETY: owned here and deleted once, after being selected back out
        // of any DC it was drawn with.
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
}

/// Run the current thread per-monitor DPI aware until dropped.
struct ThreadDpi(DPI_AWARENESS_CONTEXT);

impl ThreadDpi {
    fn per_monitor() -> Self {
        // SAFETY: changes only this thread's awareness, restored on drop.
        ThreadDpi(unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        })
    }
}

impl Drop for ThreadDpi {
    fn drop(&mut self) {
        // A null previous context means the call failed and nothing changed.
        if !self.0 .0.is_null() {
            // SAFETY: restoring the context this thread had before.
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}
