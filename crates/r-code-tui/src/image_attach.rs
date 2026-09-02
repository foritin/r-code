//! 图片附件（G6：粘贴图片 + `@file` 图片提及）。
//!
//! 通道复用宿主既有附件管线：`agent_send_with_mode_and_attachments`（魔数校验、
//! 主模型不支持 vision 时的 OCR/vision 转换、排队持久化全在宿主侧）。本模块
//! 只负责三件事：读字节（剪贴板/文件）、生成 transcript 半块 ANSI 预览
//! （pi terminal-image 形态——字符网格原生，无 kitty/sixel 终端依赖）、
//! 按 `@token` 收集待发图片文件。
//!
//! 体积上限与宿主一致（单图 8 MiB）；超限在 TUI 侧提前报错，避免整段 base64
//! 白做。

use std::path::{Path, PathBuf};

/// 宿主 `MAX_IMAGE_ATTACHMENT_BYTES` 同款上限（单图）。
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// 预览块最大列宽 / 行高（半块渲染：1 行 = 2 像素行）。
const PREVIEW_MAX_COLS: u32 = 48;
const PREVIEW_MAX_ROWS: u32 = 16;

/// 一张待发图片（粘贴或 @file）。`data` 是原始文件字节（png/jpeg/gif/webp），
/// 剪贴板 DIB 在读取时即重编码为 PNG。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingImage {
    pub name: String,
    pub media_type: &'static str,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// 半块 ANSI 预览行（空 = 占位行由渲染层呈现）。
    pub preview: Vec<String>,
}

/// 魔数嗅探（与宿主 `image_magic_matches` 同一张白名单）。
pub fn sniff_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// 图片扩展名白名单（@file 提及识别用；与魔数白名单一致）。
pub fn is_image_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp"
    )
}

/// 从输入文本收集 `@file` 图片提及（文件存在于 `cwd`、扩展名白名单）。
/// 返回去重后的路径列表；`@token` 内允许 `/`（补全列表虽只扫一层，手输路径
/// 不设限）。
pub fn collect_image_mentions(text: &str, cwd: &Path) -> Vec<PathBuf> {
    // 分隔符 = 空白 + 中英文标点（中文输入常把标点黏在提及两侧：
    // "@a.png，@b.png"）。'.' 与 '/' 不是分隔符（扩展名与子目录路径的一部分）。
    const SEPARATORS: &[char] = &[
        '，', '。', '；', '！', '？', '：', '、', '“', '”', '‘', '’', '（', '）', '《', '》', ',',
        '!', ';', ':', '?', '(', ')', '"', '\'', '[', ']', '{', '}',
    ];
    let mut paths = Vec::new();
    for token in text.split(|ch: char| ch.is_whitespace() || SEPARATORS.contains(&ch)) {
        let Some(name) = token.strip_prefix('@') else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let extension = Path::new(name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !is_image_extension(extension) {
            continue;
        }
        let path = cwd.join(name);
        if path.is_file() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// 占位行（重建历史时无字节、预览渲染失败、或用户禁用预览）。
pub fn placeholder_line(name: &str, width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        format!("[图片 {name}]")
    } else {
        format!("[图片 {name} {width}x{height}]")
    }
}

/// 读文件为待发图片（魔数校验 + 尺寸上限 + 预览）。
pub fn load_image_file(path: &Path) -> Result<PendingImage, String> {
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    let metadata = std::fs::metadata(path).map_err(|error| format!("读取 {name}：{error}"))?;
    if metadata.len() as usize > MAX_IMAGE_BYTES {
        return Err(format!(
            "{name} 超过单图 8 MiB 上限（{} 字节）",
            metadata.len()
        ));
    }
    let data = std::fs::read(path).map_err(|error| format!("读取 {name}：{error}"))?;
    build_pending(name, data)
}

/// 字节 → PendingImage（解码尺寸 + 生成预览；构造失败即报错，不静默降级）。
pub fn build_pending(name: String, data: Vec<u8>) -> Result<PendingImage, String> {
    let Some(media_type) = sniff_media_type(&data) else {
        return Err(format!("{name} 不是受支持的图片（png/jpeg/gif/webp）"));
    };
    if data.len() > MAX_IMAGE_BYTES {
        return Err(format!("{name} 超过单图 8 MiB 上限（{} 字节）", data.len()));
    }
    let decoded =
        image::load_from_memory(&data).map_err(|error| format!("{name} 解码失败：{error}"))?;
    let (width, height) = (decoded.width(), decoded.height());
    let preview = preview_lines(&decoded.to_rgba8());
    Ok(PendingImage {
        name,
        media_type,
        data,
        width,
        height,
        preview,
    })
}

/// RGBA → 半块 ANSI 预览行（pi terminal-image 形态）。
///
/// 每终端行 = 上下两像素：`▀` 前景 = 上像素、背景 = 下像素。等比缩放进
/// 48×16 行的框；透明像素落在黑色背景上合成（终端无 alpha）。
pub fn preview_lines(rgba: &image::RgbaImage) -> Vec<String> {
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Vec::new();
    }
    // 目标框：像素网格 = cols × (rows*2)，保纵横比取最小缩放（永不放大——
    // 小图放大只会糊，2x2 图标不该铺满 48 列）。
    let box_w = PREVIEW_MAX_COLS;
    let box_h = PREVIEW_MAX_ROWS * 2;
    let scale = (box_w as f64 / w as f64)
        .min(box_h as f64 / h as f64)
        .min(1.0);
    let target_w = ((w as f64 * scale).round() as u32).max(1);
    let target_h = ((h as f64 * scale).round() as u32).max(1);
    let scaled = image::imageops::resize(
        rgba,
        target_w,
        target_h,
        image::imageops::FilterType::Triangle,
    );
    let composited = composite_on_black(&scaled);
    let mut lines = Vec::new();
    let mut y = 0;
    while y < target_h {
        let mut line = String::new();
        for x in 0..target_w {
            let top = composited.get_pixel(x, y);
            let bottom = if y + 1 < target_h {
                composited.get_pixel(x, y + 1)
            } else {
                top
            };
            line.push_str(&format!(
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀\x1b[0m",
                top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]
            ));
        }
        lines.push(line);
        y += 2;
    }
    lines
}

/// 透明像素合成到黑底（半块渲染无 alpha 通道）。
fn composite_on_black(rgba: &image::RgbaImage) -> image::RgbaImage {
    let mut out = rgba.clone();
    for pixel in out.pixels_mut() {
        let alpha = pixel[3] as u32;
        if alpha >= 255 {
            continue;
        }
        for channel in pixel.0.iter_mut().take(3) {
            *channel = ((*channel as u32 * alpha) / 255) as u8;
        }
        pixel[3] = 255;
    }
    out
}

// ---------------------------------------------------------------------------
// 剪贴板读取（平台三态；终端转义序列拿不到图片字节，必须走系统剪贴板）
// ---------------------------------------------------------------------------

/// 读系统剪贴板图片。Ok(None) = 剪贴板无图片（合法状态，不报错）。
#[cfg(target_os = "windows")]
pub fn read_clipboard_image() -> Result<Option<PendingImage>, String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatA,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;

    /// 读取一个格式句柄的完整字节（GlobalLock 拷贝后立即解锁）。
    unsafe fn read_format_bytes(format: u32) -> Option<Vec<u8>> {
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let size = GlobalSize(handle);
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() || size == 0 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
    }

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("无法打开系统剪贴板（可能被其他程序占用）".to_string());
        }
        let outcome = (|| {
            // 优先注册 PNG 格式（浏览器/截图工具普遍提供，无损免转码）。
            let png_format = RegisterClipboardFormatA(c"PNG".as_ptr().cast());
            if png_format != 0 && IsClipboardFormatAvailable(png_format) != 0 {
                if let Some(bytes) = read_format_bytes(png_format) {
                    if sniff_media_type(&bytes) == Some("image/png") {
                        let name = clipboard_name("png");
                        return build_pending(name, bytes).map(Some);
                    }
                }
            }
            // 回落 DIB（BITMAPINFOHEADER 系列）→ RGBA → PNG。
            for format in [CF_DIBV5, CF_DIB] {
                if IsClipboardFormatAvailable(format) == 0 {
                    continue;
                }
                let Some(bytes) = read_format_bytes(format) else {
                    continue;
                };
                let (rgba, width, height) = dib_to_rgba(&bytes)?;
                let mut png = Vec::new();
                let encoder = image::codecs::png::PngEncoder::new_with_quality(
                    &mut png,
                    image::codecs::png::CompressionType::Fast,
                    image::codecs::png::FilterType::Adaptive,
                );
                image::DynamicImage::ImageRgba8(rgba)
                    .write_with_encoder(encoder)
                    .map_err(|error| format!("剪贴板图片编码失败：{error}"))?;
                let preview = preview_lines_bytes(&png, width, height);
                return Ok(Some(PendingImage {
                    name: clipboard_name("png"),
                    media_type: "image/png",
                    data: png,
                    width,
                    height,
                    preview,
                }));
            }
            Ok(None)
        })();
        CloseClipboard();
        outcome
    }
}

/// PNG 字节（已编码）的预览：解码后按 RGBA 渲染（DIB 路径复用）。
fn preview_lines_bytes(png: &[u8], _width: u32, _height: u32) -> Vec<String> {
    match image::load_from_memory(png) {
        Ok(decoded) => preview_lines(&decoded.to_rgba8()),
        Err(_) => Vec::new(),
    }
}

fn clipboard_name(extension: &str) -> String {
    format!(
        "clipboard-{}.{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        extension
    )
}

/// CF_DIB/CF_DIBV5 → RGBA（BI_RGB 24/32bpp；bottom-up 翻转；32bpp 的全零
/// alpha 通道按不透明处理——大量 DIB 生产者不写 alpha）。
/// 纯函数可单测（不依赖 Windows）。
fn dib_to_rgba(dib: &[u8]) -> Result<(image::RgbaImage, u32, u32), String> {
    if dib.len() < 40 {
        return Err("剪贴板图片数据不完整".to_string());
    }
    let read_u32 = |offset: usize| -> u32 {
        u32::from_le_bytes([
            dib[offset],
            dib[offset + 1],
            dib[offset + 2],
            dib[offset + 3],
        ])
    };
    let header_size = read_u32(0) as usize;
    if header_size < 40 || header_size > dib.len() {
        return Err("剪贴板图片头不完整".to_string());
    }
    let width = read_u32(4) as i32;
    let raw_height = read_u32(8) as i32;
    let bit_count = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = read_u32(16);
    if width <= 0 || raw_height == 0 {
        return Err("剪贴板图片尺寸无效".to_string());
    }
    if compression != 0 {
        return Err("剪贴板图片使用了不支持的压缩格式（仅支持未压缩 DIB）".to_string());
    }
    let bottom_up = raw_height > 0;
    let height = raw_height.unsigned_abs();
    let bytes_per_pixel = match bit_count {
        24 => 3usize,
        32 => 4usize,
        _ => return Err("剪贴板图片位深不支持（仅 24/32bpp）".to_string()),
    };
    let stride = (width as usize * bytes_per_pixel).div_ceil(4) * 4;
    let pixels_offset = header_size;
    if pixels_offset + stride * height as usize > dib.len() {
        return Err("剪贴板图片像素数据不完整".to_string());
    }
    let mut out = image::RgbaImage::new(width as u32, height);
    for row in 0..height as usize {
        // bottom-up：DIB 第 0 行是图像底部。
        let source_row = if bottom_up {
            height as usize - 1 - row
        } else {
            row
        };
        let base = pixels_offset + source_row * stride;
        for column in 0..width as usize {
            let offset = base + column * bytes_per_pixel;
            let (blue, green, red, alpha) = match bytes_per_pixel {
                3 => (dib[offset], dib[offset + 1], dib[offset + 2], 255u8),
                _ => (
                    dib[offset],
                    dib[offset + 1],
                    dib[offset + 2],
                    dib[offset + 3],
                ),
            };
            // 全零 alpha = 生产者未写 alpha，按不透明。
            let alpha = if bytes_per_pixel == 4 && alpha == 0 {
                255
            } else {
                alpha
            };
            out.put_pixel(
                column as u32,
                row as u32,
                image::Rgba([red, green, blue, alpha]),
            );
        }
    }
    Ok((out, width as u32, height))
}

#[cfg(target_os = "macos")]
pub fn read_clipboard_image() -> Result<Option<PendingImage>, String> {
    // osascript 输出 «data PNGf;hex...»——零依赖拿 PNG 字节（pngpaste 需 brew）。
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg("the clipboard as «class PNGf»")
        .output()
        .map_err(|error| format!("调用 osascript 失败：{error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let Some((_, rest)) = text.split_once("PNGf") else {
        return Ok(None);
    };
    let hex: String = rest
        .chars()
        .skip_while(|ch| !ch.is_ascii_hexdigit())
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect();
    if hex.len() < 16 || hex.len() % 2 != 0 {
        return Ok(None);
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "剪贴板图片十六进制解码失败".to_string())?;
    if sniff_media_type(&bytes) != Some("image/png") {
        return Ok(None);
    }
    build_pending(clipboard_name("png"), bytes).map(Some)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn read_clipboard_image() -> Result<Option<PendingImage>, String> {
    // Wayland → wl-paste；X11 → xclip。均为常见预装；缺失时给可操作指引。
    let (program, args): (&str, Vec<&str>) = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        ("wl-paste", vec!["-t", "image/png"])
    } else if std::env::var_os("DISPLAY").is_some() {
        (
            "xclip",
            vec!["-selection", "clipboard", "-t", "image/png", "-o"],
        )
    } else {
        return Ok(None);
    };
    let output = match std::process::Command::new(program).args(&args).output() {
        Ok(output) => output,
        Err(_) => {
            return Err(format!(
                "剪贴板图片需要 {program}（未安装）；也可用 @文件名.png 直接提及图片文件"
            ))
        }
    };
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    if sniff_media_type(&output.stdout) != Some("image/png") {
        return Ok(None);
    }
    build_pending(clipboard_name("png"), output.stdout).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 魔数嗅探覆盖四种白名单格式 + 拒绝非图片。
    #[test]
    fn sniff_media_type_covers_whitelist() {
        assert_eq!(
            sniff_media_type(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]),
            Some("image/png")
        );
        assert_eq!(
            sniff_media_type(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_media_type(b"GIF89a...."), Some("image/gif"));
        let webp = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(sniff_media_type(webp), Some("image/webp"));
        assert_eq!(sniff_media_type(b"hello world"), None);
    }

    /// 半块预览：2x2 图 → 1 行；含 ▀ 与 truecolor SGR；宽图列数封顶。
    #[test]
    fn preview_renders_half_blocks_with_caps() {
        // 2x2：上行红、下行蓝 → 1 行（top=红 fg，bottom=蓝 bg）。
        let mut image = image::RgbaImage::new(2, 2);
        for x in 0..2 {
            image.put_pixel(x, 0, image::Rgba([255, 0, 0, 255]));
            image.put_pixel(x, 1, image::Rgba([0, 0, 255, 255]));
        }
        let lines = preview_lines(&image);
        assert_eq!(lines.len(), 1, "2 像素行 = 1 终端行");
        assert!(lines[0].contains('▀'), "{}", lines[0]);
        assert!(lines[0].contains("38;2;255;0;0"), "{}", lines[0]);
        assert!(lines[0].contains("48;2;0;0;255"), "{}", lines[0]);

        // 96x2 宽图 → 缩到 48 列；不超框。
        let wide = image::RgbaImage::from_pixel(96, 2, image::Rgba([10, 20, 30, 255]));
        let lines = preview_lines(&wide);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].matches('▀').count(), 48, "列数封顶 48");

        // 2x200 高图 → 行数封顶 16。
        let tall = image::RgbaImage::from_pixel(2, 200, image::Rgba([1, 2, 3, 255]));
        assert_eq!(preview_lines(&tall).len(), 16, "行数封顶 16");

        // 透明像素合成黑底。
        let mut transparent = image::RgbaImage::new(1, 2);
        transparent.put_pixel(0, 0, image::Rgba([255, 128, 64, 128]));
        transparent.put_pixel(0, 1, image::Rgba([0, 0, 0, 0]));
        let lines = preview_lines(&transparent);
        assert!(
            lines[0].contains("38;2;128;64;32"),
            "半透明按黑底合成：{}",
            lines[0]
        );
    }

    /// @file 图片提及收集：白名单扩展 + 存在性 + 去重；非图片/缺失不收。
    #[test]
    fn collect_image_mentions_filters() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("shot.png"), b"x").expect("png");
        std::fs::write(dir.path().join("photo.jpg"), b"x").expect("jpg");
        std::fs::write(dir.path().join("note.md"), b"x").expect("md");
        let text =
            "看这张 @shot.png 和 @shot.png 与 @photo.jpg；@note.md 不是图片，@missing.png 不存在";
        let paths = collect_image_mentions(text, dir.path());
        assert_eq!(paths.len(), 2, "去重 + 白名单 + 存在性：{paths:?}");
        assert!(paths.contains(&dir.path().join("shot.png")));
        assert!(paths.contains(&dir.path().join("photo.jpg")));
    }

    /// 占位行形态。
    #[test]
    fn placeholder_line_shapes() {
        assert_eq!(placeholder_line("a.png", 100, 50), "[图片 a.png 100x50]");
        assert_eq!(placeholder_line("a.png", 0, 0), "[图片 a.png]");
    }

    /// DIB 解析：2x2 32bpp bottom-up（DIB 第 0 行 = 图像底部）→ 顶部行是后写
    /// 的红像素；24bpp 行补齐（stride 4 字节对齐）。
    #[test]
    fn dib_to_rgba_parses_32bpp_bottom_up() {
        // 头 40 字节：biSize=40, 2x2, 1 plane, 32bpp, BI_RGB。
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2u32.to_le_bytes());
        dib[8..12].copy_from_slice(&2u32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        // 像素（bottom-up）：DIB 行 0 = 图像底行（蓝），行 1 = 顶行（红）。
        let blue = [255u8, 0, 0, 255]; // BGRA 蓝
        let red = [0u8, 0, 255, 255]; // BGRA 红
        dib.extend_from_slice(&blue);
        dib.extend_from_slice(&blue);
        dib.extend_from_slice(&red);
        dib.extend_from_slice(&red);
        let (rgba, width, height) = dib_to_rgba(&dib).expect("dib");
        assert_eq!((width, height), (2, 2));
        assert_eq!(
            *rgba.get_pixel(0, 0),
            image::Rgba([255, 0, 0, 255]),
            "顶行红"
        );
        assert_eq!(
            *rgba.get_pixel(0, 1),
            image::Rgba([0, 0, 255, 255]),
            "底行蓝"
        );
    }

    /// DIB 24bpp：stride 4 字节对齐（2px × 3B = 6 → pad 到 8）+ 全零 alpha 语义
    /// 仅限 32bpp（24bpp 恒不透明）。
    #[test]
    fn dib_to_rgba_parses_24bpp_stride() {
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2u32.to_le_bytes());
        dib[8..12].copy_from_slice(&2u32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        // 每行 6 字节像素 + 2 字节 padding。
        dib.extend_from_slice(&[0u8, 0, 255, 0, 0, 255, 0, 0]); // 底行：2 个红
        dib.extend_from_slice(&[255u8, 0, 0, 255, 0, 0, 0, 0]); // 顶行：2 个蓝
        let (rgba, _, _) = dib_to_rgba(&dib).expect("dib");
        assert_eq!(
            *rgba.get_pixel(1, 0),
            image::Rgba([0, 0, 255, 255]),
            "顶行蓝"
        );
        assert_eq!(
            *rgba.get_pixel(0, 1),
            image::Rgba([255, 0, 0, 255]),
            "底行红"
        );
    }

    /// DIB 非法输入：压缩格式 / 位深 / 尺寸全部明确报错。
    #[test]
    fn dib_to_rgba_rejects_unsupported() {
        let mut dib = vec![0u8; 40];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1u32.to_le_bytes());
        dib[8..12].copy_from_slice(&1u32.to_le_bytes());
        // 16bpp 不支持。
        dib[14..16].copy_from_slice(&16u16.to_le_bytes());
        assert!(dib_to_rgba(&dib).is_err());
        // BI_BITFIELDS 压缩不支持。
        let mut compressed = dib.clone();
        compressed[14..16].copy_from_slice(&32u16.to_le_bytes());
        compressed[16..20].copy_from_slice(&3u32.to_le_bytes());
        assert!(dib_to_rgba(&compressed).is_err());
        // 数据不完整。
        assert!(dib_to_rgba(&[0u8; 8]).is_err());
    }

    /// 文件加载端到端（png 编码 → 解码 → 尺寸/媒体类型/预览）。
    #[test]
    fn load_image_file_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tiny.png");
        let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([90, 120, 150, 255]));
        image.save(&path).expect("save png");
        let pending = load_image_file(&path).expect("load");
        assert_eq!(pending.name, "tiny.png");
        assert_eq!(pending.media_type, "image/png");
        assert_eq!((pending.width, pending.height), (4, 4));
        assert!(!pending.preview.is_empty(), "预览非空");
        assert_eq!(pending.preview.len(), 2, "4 像素行 = 2 终端行");
        // 非图片文件明确报错（不静默）。
        let other = dir.path().join("plain.txt");
        std::fs::write(&other, b"not an image").expect("txt");
        let error = load_image_file(&other).expect_err("must reject");
        assert!(error.contains("不是受支持的图片"), "{error}");
    }
}
