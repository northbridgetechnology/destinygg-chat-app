use std::{ffi::c_void, ptr};

use core_foundation::{
    base::{CFRelease, CFType, TCFType},
    data::{CFData, CFDataRef},
    dictionary::{CFDictionary, CFDictionaryRef},
    number::CFNumber,
    string::CFString,
};
use core_graphics::{
    color_space::CGColorSpace,
    context::CGContext,
    geometry::{CGPoint, CGRect, CGSize},
    image::{CGImage, CGImageAlphaInfo, CGImageByteOrderInfo},
};
use foreign_types::ForeignType;

use super::{Decoded, Duration, Frame, MAX_BYTES, egui};

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateWithData(data: CFDataRef, options: CFDictionaryRef) -> *const c_void;
    fn CGImageSourceGetCount(source: *const c_void) -> usize;
    fn CGImageSourceCreateImageAtIndex(
        source: *const c_void,
        index: usize,
        options: CFDictionaryRef,
    ) -> *mut c_void;
    fn CGImageSourceCopyPropertiesAtIndex(
        source: *const c_void,
        index: usize,
        options: CFDictionaryRef,
    ) -> CFDictionaryRef;
}

struct ImageSource(*const c_void);

impl Drop for ImageSource {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

pub(super) fn decode(data: &[u8]) -> Result<Decoded, String> {
    let data = CFData::from_buffer(data);
    // ImageIO returns retained objects under the Create and Copy rules.
    let source = unsafe { CGImageSourceCreateWithData(data.as_concrete_TypeRef(), ptr::null()) };
    if source.is_null() {
        return Err("Image format could not be decoded".into());
    }
    let source = ImageSource(source);
    let count = unsafe { CGImageSourceGetCount(source.0) }.min(500);
    let mut frames = Vec::new();
    let mut allocated = 0usize;
    let mut duration = Duration::ZERO;
    for index in 0..count {
        let raw = unsafe { CGImageSourceCreateImageAtIndex(source.0, index, ptr::null()) };
        if raw.is_null() {
            break;
        }
        let image = unsafe { CGImage::from_ptr(raw.cast()) };
        let (width, height) = (image.width(), image.height());
        let size = width.saturating_mul(height).saturating_mul(4);
        if width > 8192 || height > 512 || size == 0 || allocated + size > MAX_BYTES {
            break;
        }
        let mut bitmap = CGContext::create_bitmap_context(
            None,
            width,
            height,
            8,
            width * 4,
            &CGColorSpace::create_device_rgb(),
            CGImageAlphaInfo::CGImageAlphaPremultipliedLast as u32
                | CGImageByteOrderInfo::CGImageByteOrder32Big as u32,
        );
        bitmap.draw_image(
            CGRect::new(
                &CGPoint::new(0.0, 0.0),
                &CGSize::new(width as f64, height as f64),
            ),
            &image,
        );
        let color = egui::ColorImage::from_rgba_premultiplied([width, height], bitmap.data());
        let delay = if count > 1 {
            frame_delay(&source, index)
        } else {
            Duration::ZERO
        };
        duration += delay;
        allocated += size;
        frames.push(Frame {
            image: color,
            delay,
        });
    }
    if frames.is_empty() {
        return Err("Image has no usable frames".into());
    }
    Ok(Decoded { frames, duration })
}

fn frame_delay(source: &ImageSource, index: usize) -> Duration {
    type Properties = CFDictionary<CFString, CFType>;
    let raw = unsafe { CGImageSourceCopyPropertiesAtIndex(source.0, index, ptr::null()) };
    if !raw.is_null() {
        let properties = unsafe { Properties::wrap_under_create_rule(raw) };
        for format in ["{GIF}", "{PNG}", "{WebP}", "{AVIS}", "{HEICS}"] {
            if let Some(group) = properties.find(CFString::new(format)) {
                let group = unsafe { Properties::wrap_under_get_rule(group.as_CFTypeRef().cast()) };
                for key in ["UnclampedDelayTime", "DelayTime"] {
                    if let Some(seconds) = group
                        .find(CFString::new(key))
                        .and_then(|v| v.downcast::<CFNumber>())
                        .and_then(|v| v.to_f64())
                        && seconds.is_finite()
                        && seconds > 0.0
                    {
                        return Duration::from_secs_f64(seconds.clamp(0.02, 10.0));
                    }
                }
            }
        }
    }
    Duration::from_millis(100)
}
