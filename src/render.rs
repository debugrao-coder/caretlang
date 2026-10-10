//! Cached, alpha-composited native badge rendering. No focus or input handling.
use caretlang::core::{InputState, Rect};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, POINT, RECT, SIZE},
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::*,
        },
        UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow},
    },
    core::{Result, w},
};

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    _pixels: *mut core::ffi::c_void,
    size: i32,
}

impl Surface {
    fn new(size: i32) -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size,
                    biHeight: -size,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = core::ptr::null_mut();
            let bitmap =
                match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0) {
                    Ok(bitmap) => bitmap,
                    Err(error) => {
                        let _ = DeleteDC(dc);
                        return Err(error);
                    }
                };
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            if previous.is_invalid() {
                let error = windows::core::Error::from_thread();
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
                let _ = DeleteDC(dc);
                return Err(error);
            }
            Ok(Self {
                dc,
                bitmap,
                previous,
                _pixels: pixels,
                size,
            })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

pub struct Renderer {
    // Release the render target before its bound DC.
    target: ID2D1DCRenderTarget,
    text: IDWriteTextFormat,
    surface: Option<Surface>,
    cache: Option<(InputState, bool, u32)>,
}

fn color(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

fn rounded(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left,
            top,
            right,
            bottom,
        },
        radiusX: radius,
        radiusY: radius,
    }
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let target = factory.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            })?;
            // ClearType assumes an opaque background. Grayscale preserves alpha edges.
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text = write.CreateTextFormat(
                w!("Microsoft YaHei UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                15.0,
                w!("zh-CN"),
            )?;
            text.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            text.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            text.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(Self {
                target,
                text,
                surface: None,
                cache: None,
            })
        }
    }

    fn rasterize(&mut self, state: InputState, dark: bool, dpi: u32) -> Result<()> {
        let size = ((46_u64 * dpi as u64 + 48) / 96) as i32;
        if self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.size != size)
        {
            let surface = Surface::new(size)?;
            unsafe {
                self.target.BindDC(
                    surface.dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: size,
                        bottom: size,
                    },
                )?;
            }
            self.surface = Some(surface);
        }
        self.cache = None;
        let chinese = state == InputState::Chinese;
        let (fill, edge, ink) = match (dark, chinese) {
            (true, true) => (
                color(29, 55, 53, 0.98),
                color(107, 166, 157, 0.48),
                color(162, 224, 210, 1.0),
            ),
            (false, true) => (
                color(234, 249, 244, 0.98),
                color(92, 151, 137, 0.40),
                color(35, 103, 90, 1.0),
            ),
            (true, false) => (
                color(40, 45, 55, 0.98),
                color(149, 161, 182, 0.38),
                color(228, 232, 240, 1.0),
            ),
            (false, false) => (
                color(248, 250, 253, 0.98),
                color(121, 136, 158, 0.30),
                color(65, 78, 99, 1.0),
            ),
        };
        unsafe {
            let brush = self.target.CreateSolidColorBrush(&fill, None)?;
            self.target.SetDpi(dpi as f32, dpi as f32);
            self.target.BeginDraw();
            self.target.Clear(Some(&color(0, 0, 0, 0.0)));
            if state != InputState::Unknown {
                // Nested translucent outlines give a quiet, softly feathered shadow.
                for spread in (1..=6).rev() {
                    let s = spread as f32;
                    brush.SetColor(&color(6, 18, 29, if dark { 0.025 } else { 0.014 }));
                    self.target.FillRoundedRectangle(
                        &rounded(8.0 - s, 9.5 - s, 38.0 + s, 39.5 + s, 9.0 + s),
                        &brush,
                    );
                }
                let badge = rounded(8.5, 8.5, 37.5, 37.5, 8.5);
                brush.SetColor(&fill);
                self.target.FillRoundedRectangle(&badge, &brush);
                brush.SetColor(&edge);
                self.target.DrawRoundedRectangle(&badge, &brush, 1.0, None);
                brush.SetColor(&ink);
                let glyph = if chinese { [0x4e2d_u16] } else { [0x82f1_u16] };
                self.target.DrawText(
                    &glyph,
                    &self.text,
                    &D2D_RECT_F {
                        left: 8.0,
                        top: 7.0,
                        right: 38.0,
                        bottom: 38.0,
                    },
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            self.target.EndDraw(None, None)?;
        }
        self.cache = Some((state, dark, dpi));
        Ok(())
    }

    /// Export the actual Direct2D pixels for inspection; no separate preview renderer.
    /// The existing transparent 8 DIP margin is composited on a neutral background.
    pub fn export_preview(
        &mut self,
        path: &std::path::Path,
        state: InputState,
        dark: bool,
        dpi: u32,
    ) -> Result<()> {
        let dpi = dpi.clamp(48, 960);
        self.rasterize(state, dark, dpi)?;
        let surface = self.surface.as_ref().expect("rasterize creates a surface");
        let pixels = unsafe {
            core::slice::from_raw_parts(
                surface._pixels.cast::<u8>(),
                (surface.size * surface.size * 4) as usize,
            )
        };
        let pixel_bytes = pixels.len() as u32;
        let mut bmp = Vec::with_capacity(54 + pixels.len());
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(54 + pixel_bytes).to_le_bytes());
        bmp.extend_from_slice(&0_u32.to_le_bytes());
        bmp.extend_from_slice(&54_u32.to_le_bytes());
        bmp.extend_from_slice(&40_u32.to_le_bytes());
        bmp.extend_from_slice(&surface.size.to_le_bytes());
        // A negative height keeps the DIB's top-down row order intact.
        bmp.extend_from_slice(&(-surface.size).to_le_bytes());
        bmp.extend_from_slice(&1_u16.to_le_bytes());
        bmp.extend_from_slice(&32_u16.to_le_bytes());
        bmp.extend_from_slice(&0_u32.to_le_bytes());
        bmp.extend_from_slice(&pixel_bytes.to_le_bytes());
        let pixels_per_meter = ((u64::from(dpi) * 10_000 + 127) / 254) as i32;
        bmp.extend_from_slice(&pixels_per_meter.to_le_bytes());
        bmp.extend_from_slice(&pixels_per_meter.to_le_bytes());
        bmp.extend_from_slice(&0_u32.to_le_bytes());
        bmp.extend_from_slice(&0_u32.to_le_bytes());
        let background: [u8; 3] = if dark { [28, 25, 23] } else { [245, 243, 240] };
        for pixel in pixels.as_chunks::<4>().0 {
            let inverse_alpha = 255 - u32::from(pixel[3]);
            for channel in 0..3 {
                // Direct2D supplies premultiplied BGRA. Do not multiply alpha twice.
                let value = u32::from(pixel[channel])
                    + (u32::from(background[channel]) * inverse_alpha + 127) / 255;
                bmp.push(value.min(255) as u8);
            }
            bmp.push(255);
        }
        std::fs::write(path, bmp).map_err(|error| {
            windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                format!(
                    "Cannot export native preview to {}: {error}",
                    path.display()
                ),
            )
        })
    }

    pub fn render(
        &mut self,
        hwnd: HWND,
        state: InputState,
        dark: bool,
        dpi: u32,
        position: Rect,
        opacity: u8,
    ) -> Result<()> {
        let dpi = dpi.clamp(48, 960);
        if self.cache != Some((state, dark, dpi)) {
            self.rasterize(state, dark, dpi)?;
        }
        let surface = self.surface.as_ref().expect("rasterize creates a surface");
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: opacity,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&POINT {
                    x: position.left,
                    y: position.top,
                }),
                Some(&SIZE {
                    cx: surface.size,
                    cy: surface.size,
                }),
                Some(surface.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_preview_has_valid_bmp_dimensions_and_payload() {
        let filename = format!("caretlang-render-preview-{}.bmp", std::process::id());
        let path = std::env::temp_dir().join(filename);
        let mut renderer = Renderer::new().expect("Direct2D initialization");
        renderer
            .export_preview(&path, InputState::Chinese, true, 192)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(&bytes[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bytes[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(bytes[18..22].try_into().unwrap()), 92);
        assert_eq!(i32::from_le_bytes(bytes[22..26].try_into().unwrap()), -92);
        assert_eq!(u16::from_le_bytes(bytes[28..30].try_into().unwrap()), 32);
        assert_eq!(bytes.len(), 54 + 92 * 92 * 4);
        assert_eq!(
            u32::from_le_bytes(bytes[2..6].try_into().unwrap()) as usize,
            bytes.len()
        );
        assert!(
            bytes[54..]
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
    }

    #[test]
    fn native_raster_has_transparent_margin_and_premultiplied_color() {
        let mut renderer = Renderer::new().expect("Direct2D initialization");
        for dpi in [96, 144, 192] {
            for dark in [false, true] {
                for state in [InputState::Chinese, InputState::English] {
                    renderer.rasterize(state, dark, dpi).expect("rasterization");
                    let surface = renderer.surface.as_ref().unwrap();
                    let pixels = unsafe {
                        core::slice::from_raw_parts(
                            surface._pixels as *const u8,
                            (surface.size * surface.size * 4) as usize,
                        )
                    };
                    assert_eq!(&pixels[..4], &[0, 0, 0, 0]);
                    assert!(pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 240));
                    assert!(
                        pixels
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .all(|pixel| pixel[..3].iter().all(|channel| *channel <= pixel[3]))
                    );
                }
            }
        }
        renderer.rasterize(InputState::Unknown, false, 96).unwrap();
        let surface = renderer.surface.as_ref().unwrap();
        let pixels = unsafe {
            core::slice::from_raw_parts(
                surface._pixels as *const u8,
                (surface.size * surface.size * 4) as usize,
            )
        };
        assert!(pixels.iter().all(|value| *value == 0));
    }
}
