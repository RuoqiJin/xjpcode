//! 对应上游 `src/utils/mime.ts`：从文件头判图片类型（不看扩展名）。

use std::path::Path;

use tokio::io::AsyncReadExt;

const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// pi: `detectSupportedImageMimeType`
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<String> {
    if buffer.starts_with(&[0xff, 0xd8, 0xff]) {
        return if buffer.get(3) == Some(&0xf7) {
            None
        } else {
            Some("image/jpeg".to_string())
        };
    }
    if buffer.starts_with(&PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png".to_string())
        } else {
            None
        };
    }
    if starts_with_ascii(buffer, 0, "GIF87a") || starts_with_ascii(buffer, 0, "GIF89a") {
        return Some("image/gif".to_string());
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp".to_string());
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp".to_string());
    }
    None
}

/// pi: `detectSupportedImageMimeTypeFromFile`
pub async fn detect_supported_image_mime_type_from_file(path: &Path) -> Option<String> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut buffer = vec![0u8; IMAGE_TYPE_SNIFF_BYTES];
    let bytes_read = file.read(&mut buffer).await.ok()?;
    buffer.truncate(bytes_read);
    detect_supported_image_mime_type(&buffer)
}

fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    buffer
        .get(offset..offset + text.len())
        .is_some_and(|window| window == text.as_bytes())
}

fn read_u32_be(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes = buffer.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u32_le(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes = buffer.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u16_le(buffer: &[u8], offset: usize) -> Option<u16> {
    let bytes = buffer.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_u32_be(buffer, PNG_SIGNATURE.len()) == Some(13)
        && starts_with_ascii(buffer, 12, "IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let Some(chunk_length) = read_u32_be(buffer, offset) else {
            return false;
        };
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }
        let next_offset = offset as u64 + 8 + chunk_length as u64 + 4;
        if next_offset <= offset as u64 || next_offset > buffer.len() as u64 {
            return false;
        }
        offset = next_offset as usize;
    }
    false
}

fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let Some(declared_file_size) = read_u32_le(buffer, 2) else {
        return false;
    };
    let Some(pixel_data_offset) = read_u32_le(buffer, 10) else {
        return false;
    };
    let Some(dib_header_size) = read_u32_le(buffer, 14) else {
        return false;
    };
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if (pixel_data_offset as u64) < 14 + dib_header_size as u64 {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }

    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
    } else {
        return false;
    };

    color_planes == Some(1) && matches!(bits_per_pixel, Some(1 | 4 | 8 | 16 | 24 | 32))
}
