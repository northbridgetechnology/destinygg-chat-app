use std::io::Cursor;

use image::{
    AnimationDecoder, ImageDecoder, ImageFormat, ImageReader, Limits, RgbaImage,
    codecs::{gif::GifDecoder, png::PngDecoder, webp::WebPDecoder},
};

use super::{Decoded, Duration, Frame, MAX_BYTES, egui};

pub(super) fn decode(data: &[u8]) -> Result<Decoded, String> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(MAX_BYTES as u64);
    let cursor = Cursor::new(data);
    let mut reader = ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    match reader.format() {
        Some(ImageFormat::Gif) => {
            let mut decoder = GifDecoder::new(cursor).map_err(|error| error.to_string())?;
            decoder
                .set_limits(limits)
                .map_err(|error| error.to_string())?;
            return animated(decoder);
        }
        Some(ImageFormat::WebP) => {
            let mut decoder = WebPDecoder::new(cursor).map_err(|error| error.to_string())?;
            decoder
                .set_limits(limits.clone())
                .map_err(|error| error.to_string())?;
            if decoder.has_animation() {
                return animated(decoder);
            }
        }
        Some(ImageFormat::Png) => {
            let mut decoder = PngDecoder::new(cursor).map_err(|error| error.to_string())?;
            decoder
                .set_limits(limits.clone())
                .map_err(|error| error.to_string())?;
            if decoder.is_apng().map_err(|error| error.to_string())? {
                return animated(decoder.apng().map_err(|error| error.to_string())?);
            }
        }
        _ => {}
    }
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| error.to_string())?
        .into_rgba8();
    Ok(Decoded {
        frames: vec![to_frame(image, Duration::ZERO)],
        duration: Duration::ZERO,
    })
}

fn animated<'a>(decoder: impl AnimationDecoder<'a>) -> Result<Decoded, String> {
    let mut frames = Vec::new();
    let mut allocated = 0usize;
    let mut duration = Duration::ZERO;
    for frame in decoder.into_frames().take(500) {
        let frame = frame.map_err(|error| error.to_string())?;
        allocated += frame.buffer().as_raw().len();
        if allocated > MAX_BYTES {
            break;
        }
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let delay = Duration::from_secs_f64(
            (f64::from(numerator) / f64::from(denominator) / 1_000.0).clamp(0.02, 10.0),
        );
        duration += delay;
        frames.push(to_frame(frame.into_buffer(), delay));
    }
    if frames.is_empty() {
        return Err("Image has no usable frames".into());
    }
    Ok(Decoded { frames, duration })
}

fn to_frame(image: RgbaImage, delay: Duration) -> Frame {
    let dimensions = [image.width() as usize, image.height() as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(dimensions, image.as_raw());
    Frame { image, delay }
}
