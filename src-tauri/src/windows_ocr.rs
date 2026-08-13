//! Windows-only image text recognition via the on-device `Windows.Media.Ocr` API.

use windows::{
    Graphics::Imaging::{
        BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
        ExifOrientationMode,
    },
    Media::Ocr::OcrEngine,
    Security::Cryptography::CryptographicBuffer,
    Storage::Streams::InMemoryRandomAccessStream,
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
    },
};

/// Balance a successful `RoInitialize` call on the blocking worker that owns the WinRT objects.
struct WinRtApartment {
    initialized_here: bool,
}

impl WinRtApartment {
    fn initialize() -> Result<Self, String> {
        // SAFETY: this function runs on a dedicated blocking worker and the guard keeps the
        // successful initialization balanced on the same thread.
        match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => Ok(Self {
                initialized_here: true,
            }),
            // A host may already have initialized this worker as STA. WinRT is available in that
            // apartment; only the requested apartment type cannot be changed, and this call must
            // not be balanced with `RoUninitialize`.
            Err(error) if error.code() == RPC_E_CHANGED_MODE => Ok(Self {
                initialized_here: false,
            }),
            Err(error) => Err(format!("Windows OCR 初始化失败：{error}")),
        }
    }
}

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        if self.initialized_here {
            // SAFETY: paired with the successful `RoInitialize` call above on the same thread.
            unsafe { RoUninitialize() };
        }
    }
}

fn jpeg_dimensions(image: &[u8]) -> Option<(u32, u32)> {
    if !image.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut cursor = 2usize;
    while cursor < image.len() {
        while image.get(cursor) == Some(&0xff) {
            cursor += 1;
        }
        let marker = *image.get(cursor)?;
        cursor += 1;
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let segment_len = u16::from_be_bytes([*image.get(cursor)?, *image.get(cursor + 1)?]);
        if segment_len < 2 {
            return None;
        }
        let segment_end = cursor.checked_add(segment_len as usize)?;
        if segment_end > image.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if segment_len < 7 {
                return None;
            }
            let height = u16::from_be_bytes([*image.get(cursor + 3)?, *image.get(cursor + 4)?]);
            let width = u16::from_be_bytes([*image.get(cursor + 5)?, *image.get(cursor + 6)?]);
            return Some((u32::from(width), u32::from(height)));
        }
        cursor = segment_end;
    }
    None
}

/// Read encoded dimensions before WinRT allocates a decoded pixel buffer.
pub(crate) fn image_dimensions(image: &[u8]) -> Result<(u32, u32), String> {
    let dimensions = if image.len() >= 24 && image.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some((
            u32::from_be_bytes([image[16], image[17], image[18], image[19]]),
            u32::from_be_bytes([image[20], image[21], image[22], image[23]]),
        ))
    } else {
        jpeg_dimensions(image)
    };
    match dimensions {
        Some((width, height)) if width > 0 && height > 0 => Ok((width, height)),
        _ => Err("无法安全读取图片尺寸".to_string()),
    }
}

fn scaled_dimensions(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width <= max_dimension && height <= max_dimension {
        return (width, height);
    }
    let scale = f64::from(max_dimension) / f64::from(width.max(height));
    (
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    )
}

/// Recognize visible text without uploading the image. The caller must run this on a blocking
/// worker because decoding and WinRT OCR can take noticeable time for large screenshots.
pub(crate) fn recognize_text(image: &[u8]) -> Result<String, String> {
    if image.is_empty() {
        return Err("图片内容为空".to_string());
    }

    let _apartment = WinRtApartment::initialize()?;
    let buffer = CryptographicBuffer::CreateFromByteArray(image)
        .map_err(|error| format!("Windows OCR 无法读取图片：{error}"))?;
    let stream = InMemoryRandomAccessStream::new()
        .map_err(|error| format!("Windows OCR 无法创建图片缓冲区：{error}"))?;
    stream
        .WriteAsync(&buffer)
        .and_then(|operation| operation.get())
        .map_err(|error| format!("Windows OCR 无法写入图片缓冲区：{error}"))?;
    stream
        .Seek(0)
        .map_err(|error| format!("Windows OCR 无法读取图片缓冲区：{error}"))?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|operation| operation.get())
        .map_err(|error| format!("Windows OCR 无法解码图片：{error}"))?;
    let width = decoder
        .PixelWidth()
        .map_err(|error| format!("Windows OCR 无法读取图片宽度：{error}"))?;
    let height = decoder
        .PixelHeight()
        .map_err(|error| format!("Windows OCR 无法读取图片高度：{error}"))?;
    let max_dimension = OcrEngine::MaxImageDimension()
        .map_err(|error| format!("Windows OCR 无法读取系统尺寸限制：{error}"))?;
    if max_dimension == 0 {
        return Err("Windows OCR 返回了无效的图片尺寸限制".to_string());
    }

    // Windows.Media.Ocr rejects images above its platform-dependent edge limit. Scale only the
    // decoded bitmap, preserving the original attachment and aspect ratio.
    let (scaled_width, scaled_height) = scaled_dimensions(width, height, max_dimension);
    let transform =
        BitmapTransform::new().map_err(|error| format!("Windows OCR 无法创建图片变换：{error}"))?;
    transform
        .SetScaledWidth(scaled_width)
        .and_then(|_| transform.SetScaledHeight(scaled_height))
        .map_err(|error| format!("Windows OCR 无法缩放图片：{error}"))?;
    let bitmap = decoder
        .GetSoftwareBitmapTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Premultiplied,
            &transform,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::ColorManageToSRgb,
        )
        .and_then(|operation| operation.get())
        .map_err(|error| format!("Windows OCR 无法准备图片：{error}"))?;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|error| format!("Windows OCR 没有可用的系统识别语言，请先安装语言包：{error}"))?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .and_then(|operation| operation.get())
        .map_err(|error| format!("Windows OCR 识别失败：{error}"))?;
    let text = result
        .Text()
        .map_err(|error| format!("Windows OCR 无法读取识别结果：{error}"))?
        .to_string();
    let text = text.trim();
    if text.is_empty() {
        return Err("Windows OCR 未在图片中识别到文字".to_string());
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::{image_dimensions, recognize_text, scaled_dimensions};

    #[test]
    fn recognizes_text_from_a_local_product_screenshot() {
        let image = include_bytes!("../../docs/ui/dark/50-model-configuration-dark.png");
        let text = recognize_text(image).expect("Windows OCR should recognize the local fixture");
        assert!(text.to_ascii_lowercase().contains("deepseek"), "{text}");
    }

    #[test]
    fn reads_png_dimensions_without_decoding_pixels() {
        let mut png = vec![0u8; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&10_000u32.to_be_bytes());
        assert_eq!(image_dimensions(&png).unwrap(), (20_000, 10_000));
        assert!(image_dimensions(b"not-an-image").is_err());
    }

    #[test]
    fn scales_large_images_without_distorting_aspect_ratio() {
        assert_eq!(scaled_dimensions(1_200, 800, 2_600), (1_200, 800));
        assert_eq!(scaled_dimensions(5_200, 2_600, 2_600), (2_600, 1_300));
        assert_eq!(scaled_dimensions(2_600, 5_200, 2_600), (1_300, 2_600));
    }
}
