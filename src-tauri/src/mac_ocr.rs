//! macOS-only image text recognition via Apple's on-device Vision framework.

use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSProcessInfo, NSString};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};

const RECOGNIZE_TEXT_REVISION_2: usize = 2;

fn supports_automatic_language_detection(major_version: isize) -> bool {
    major_version >= 13
}

fn configure_languages(request: &VNRecognizeTextRequest) -> Result<(), String> {
    let process_info = NSProcessInfo::processInfo();
    let version = process_info.operatingSystemVersion();
    if supports_automatic_language_detection(version.majorVersion) {
        // `automaticallyDetectsLanguage` was added in macOS 13. Calling this selector on an older
        // deployment target can terminate the process with an unrecognized-selector exception.
        request.setAutomaticallyDetectsLanguage(true);
        return Ok(());
    }

    // macOS 11/12 use revision 2. It supports Chinese in Accurate mode, but does not auto-detect
    // the script, so select the supported subset explicitly instead of silently falling back to
    // English-only recognition.
    // SAFETY: revision 2 and this class method are available from macOS 11, our deployment target.
    unsafe { request.setRevision(RECOGNIZE_TEXT_REVISION_2) };
    #[allow(deprecated)]
    let supported = unsafe {
        VNRecognizeTextRequest::supportedRecognitionLanguagesForTextRecognitionLevel_revision_error(
            VNRequestTextRecognitionLevel::Accurate,
            RECOGNIZE_TEXT_REVISION_2,
        )
    }
    .map_err(|error| format!("macOS Vision 无法读取 OCR 语言列表：{error}"))?;
    let supported = supported
        .to_vec()
        .into_iter()
        .map(|language| language.to_string())
        .collect::<Vec<_>>();
    let preferred = ["zh-Hans", "zh-Hant", "en-US"]
        .into_iter()
        .filter(|language| supported.iter().any(|value| value == language))
        .map(NSString::from_str)
        .collect::<Vec<_>>();
    if preferred.is_empty() {
        return Err("macOS Vision 没有可用的中英文 OCR 语言".to_string());
    }
    request.setRecognitionLanguages(&NSArray::from_retained_slice(&preferred));
    Ok(())
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

fn webp_dimensions(image: &[u8]) -> Option<(u32, u32)> {
    if image.len() < 20 || !image.starts_with(b"RIFF") || image.get(8..12)? != b"WEBP" {
        return None;
    }
    match image.get(12..16)? {
        b"VP8X" if image.len() >= 30 => {
            let width = 1 + u32::from_le_bytes([image[24], image[25], image[26], 0]);
            let height = 1 + u32::from_le_bytes([image[27], image[28], image[29], 0]);
            Some((width, height))
        }
        b"VP8L" if image.len() >= 25 && image[20] == 0x2f => {
            let width = 1 + u32::from(image[21]) + ((u32::from(image[22]) & 0x3f) << 8);
            let height = 1
                + (u32::from(image[22]) >> 6)
                + (u32::from(image[23]) << 2)
                + ((u32::from(image[24]) & 0x0f) << 10);
            Some((width, height))
        }
        b"VP8 " if image.len() >= 30 && image.get(23..26) == Some(&[0x9d, 0x01, 0x2a][..]) => {
            let width = u16::from_le_bytes([image[26], image[27]]) & 0x3fff;
            let height = u16::from_le_bytes([image[28], image[29]]) & 0x3fff;
            Some((u32::from(width), u32::from(height)))
        }
        _ => None,
    }
}

/// Read encoded dimensions without asking ImageIO to allocate the decoded pixel buffer.
pub(crate) fn image_dimensions(image: &[u8]) -> Result<(u32, u32), String> {
    let dimensions = if image.len() >= 24 && image.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some((
            u32::from_be_bytes([image[16], image[17], image[18], image[19]]),
            u32::from_be_bytes([image[20], image[21], image[22], image[23]]),
        ))
    } else if image.len() >= 10 && (image.starts_with(b"GIF87a") || image.starts_with(b"GIF89a")) {
        Some((
            u32::from(u16::from_le_bytes([image[6], image[7]])),
            u32::from(u16::from_le_bytes([image[8], image[9]])),
        ))
    } else {
        jpeg_dimensions(image).or_else(|| webp_dimensions(image))
    };
    match dimensions {
        Some((width, height)) if width > 0 && height > 0 => Ok((width, height)),
        _ => Err("无法安全读取图片尺寸".to_string()),
    }
}

/// Recognize visible text without uploading the image. The caller runs this on a blocking worker
/// because Vision performs synchronously and can take noticeable time for large screenshots.
pub(crate) fn recognize_text(image: &[u8]) -> Result<String, String> {
    if image.is_empty() {
        return Err("图片内容为空".to_string());
    }

    autoreleasepool(|_| {
        let data = NSData::with_bytes(image);
        let options = NSDictionary::<VNImageOption, AnyObject>::new();
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &data,
            &options,
        );
        // SAFETY: `init` is the designated no-handler initializer exposed by Vision.
        let request = unsafe { VNRecognizeTextRequest::init(VNRecognizeTextRequest::alloc()) };
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        configure_languages(&request)?;

        let requests = NSArray::<VNRequest>::from_slice(&[&request]);
        handler
            .performRequests_error(&requests)
            .map_err(|error| format!("macOS Vision 识别失败：{error}"))?;

        let observations = request
            .results()
            .ok_or_else(|| "macOS Vision 没有返回识别结果".to_string())?;
        let lines = observations
            .to_vec()
            .into_iter()
            .filter_map(|observation| observation.topCandidates(1).to_vec().into_iter().next())
            .map(|candidate| candidate.string().to_string())
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return Err("macOS Vision 未在图片中识别到文字".to_string());
        }
        Ok(lines.join("\n"))
    })
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    use super::{image_dimensions, recognize_text, supports_automatic_language_detection};

    #[test]
    fn recognizes_a_local_png_without_network_access() {
        const PNG: &str = "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";
        let image = STANDARD.decode(PNG).expect("fixture base64");
        assert_eq!(image_dimensions(&image).unwrap(), (360, 100));
        let text = recognize_text(&image).expect("Vision should recognize the fixture");
        assert!(text.to_ascii_uppercase().contains("OCR"), "{text}");
        assert!(text.contains("123"), "{text}");
    }

    #[test]
    fn automatic_language_detection_starts_at_macos_13() {
        assert!(!supports_automatic_language_detection(11));
        assert!(!supports_automatic_language_detection(12));
        assert!(supports_automatic_language_detection(13));
    }

    #[test]
    fn reads_dimensions_without_decoding_pixels() {
        let mut png = vec![0u8; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&10_000u32.to_be_bytes());
        assert_eq!(image_dimensions(&png).unwrap(), (20_000, 10_000));
        assert!(image_dimensions(b"not-an-image").is_err());
    }
}
