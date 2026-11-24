use eframe::egui::Color32;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Graphics::Dwm::{
        DWMWA_CAPTION_COLOR, DWMWA_COLOR_DEFAULT, DWMWA_TEXT_COLOR, DwmSetWindowAttribute,
    },
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SystemParametersInfoW,
        },
    },
};

#[derive(Default)]
pub struct WindowAppearance {
    checked_at: Option<Instant>,
    high_contrast: bool,
    reduced_motion: bool,
    applied: Option<(isize, u32, u32)>,
}

impl WindowAppearance {
    pub fn update(&mut self, frame: &eframe::Frame, background: Color32, text: Color32) -> bool {
        if self
            .checked_at
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
        {
            let mut animations = 1i32;
            let mut contrast = HIGHCONTRASTW::default();
            contrast.cbSize = size_of::<HIGHCONTRASTW>() as u32;
            // Each SPI query writes to the documented output type in this stack frame.
            unsafe {
                if SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    (&raw mut animations).cast(),
                    0,
                ) != 0
                {
                    self.reduced_motion = animations == 0;
                }
                if SystemParametersInfoW(
                    SPI_GETHIGHCONTRAST,
                    contrast.cbSize,
                    (&raw mut contrast).cast(),
                    0,
                ) != 0
                {
                    self.high_contrast = contrast.dwFlags & HCF_HIGHCONTRASTON != 0;
                }
            }
            self.checked_at = Some(Instant::now());
        }
        if let Ok(handle) = frame.window_handle()
            && let RawWindowHandle::Win32(window) = handle.as_raw()
        {
            let colorref = |c: Color32| u32::from_le_bytes([c.r(), c.g(), c.b(), 0]);
            let colors = if self.high_contrast {
                (DWMWA_COLOR_DEFAULT, DWMWA_COLOR_DEFAULT)
            } else {
                (colorref(background), colorref(text))
            };
            let key = (window.hwnd.get(), colors.0, colors.1);
            if self.applied != Some(key) {
                // The frame owns this HWND. Older Windows versions keep their native colors
                // if they do not implement these two DWM attributes.
                for (attribute, color) in [
                    (DWMWA_CAPTION_COLOR, colors.0),
                    (DWMWA_TEXT_COLOR, colors.1),
                ] {
                    unsafe {
                        DwmSetWindowAttribute(
                            key.0 as _,
                            attribute as u32,
                            (&raw const color).cast(),
                            size_of::<u32>() as u32,
                        );
                    }
                }
                self.applied = Some(key);
            }
        }
        self.reduced_motion
    }
}
