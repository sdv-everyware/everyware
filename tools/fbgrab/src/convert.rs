// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Conversion from linear 32-bit DRM framebuffer formats to tightly packed RGBA8888.

use drm_fourcc::DrmFourcc;

/// Returns `true` if `fourcc` is a supported 32-bit packed RGB/BGR format.
pub fn format_supported(fourcc: DrmFourcc) -> bool {
    matches!(
        fourcc,
        DrmFourcc::Xrgb8888
            | DrmFourcc::Argb8888
            | DrmFourcc::Xbgr8888
            | DrmFourcc::Abgr8888
            | DrmFourcc::Bgrx8888
            | DrmFourcc::Bgra8888
            | DrmFourcc::Rgbx8888
            | DrmFourcc::Rgba8888
    )
}

/// Converts a linear 32-bit DRM framebuffer region into tightly packed RGBA8888
/// (byte order `[R, G, B, A]`, `width * height * 4` bytes).
pub fn to_rgba(
    src: &[u8],
    width: u32,
    height: u32,
    pitch: u32,
    offset: u32,
    fourcc: DrmFourcc,
    dst: &mut [u8],
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("invalid dimensions {width}x{height}"));
    }
    if !format_supported(fourcc) {
        return Err(format!("unsupported pixel format {fourcc}"));
    }

    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| "row byte size overflow".to_string())?;
    let pitch_usize = pitch as usize;
    if pitch_usize < row_bytes {
        return Err(format!("pitch {pitch} smaller than row width {row_bytes}"));
    }
    let out_bytes = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(|| "output buffer size overflow".to_string())?;
    if dst.len() < out_bytes {
        return Err(format!(
            "destination buffer too small: {} < {out_bytes}",
            dst.len()
        ));
    }

    // Last accessed byte is offset + (height - 1) * pitch + row_bytes.
    let last_row = (height as usize - 1)
        .checked_mul(pitch_usize)
        .and_then(|v| v.checked_add(row_bytes))
        .and_then(|v| v.checked_add(offset as usize))
        .ok_or_else(|| "source buffer size overflow".to_string())?;
    if last_row > src.len() {
        return Err(format!(
            "source buffer too small: {} < {last_row}",
            src.len()
        ));
    }

    for y in 0..(height as usize) {
        let s_start = (offset as usize) + y * pitch_usize;
        let srow = &src[s_start..s_start + row_bytes];
        let d_start = y * row_bytes;
        let drow = &mut dst[d_start..d_start + row_bytes];

        if fourcc == DrmFourcc::Abgr8888 {
            // Little-endian A:B:G:R -> bytes [R, G, B, A], already RGBA.
            drow.copy_from_slice(srow);
            continue;
        }

        for (p, q) in srow.chunks_exact(4).zip(drow.chunks_exact_mut(4)) {
            match fourcc {
                // Little-endian x:R:G:B -> bytes [B, G, R, X].
                DrmFourcc::Xrgb8888 => q.copy_from_slice(&[p[2], p[1], p[0], 0xff]),
                // Little-endian A:R:G:B -> bytes [B, G, R, A].
                DrmFourcc::Argb8888 => q.copy_from_slice(&[p[2], p[1], p[0], p[3]]),
                // Little-endian x:B:G:R -> bytes [R, G, B, X].
                DrmFourcc::Xbgr8888 => q.copy_from_slice(&[p[0], p[1], p[2], 0xff]),
                // Little-endian B:G:R:x -> bytes [X, R, G, B].
                DrmFourcc::Bgrx8888 => q.copy_from_slice(&[p[1], p[2], p[3], 0xff]),
                // Little-endian B:G:R:A -> bytes [A, R, G, B].
                DrmFourcc::Bgra8888 => q.copy_from_slice(&[p[1], p[2], p[3], p[0]]),
                // Little-endian R:G:B:x -> bytes [X, B, G, R].
                DrmFourcc::Rgbx8888 => q.copy_from_slice(&[p[3], p[2], p[1], 0xff]),
                // Little-endian R:G:B:A -> bytes [A, B, G, R].
                DrmFourcc::Rgba8888 => q.copy_from_slice(&[p[3], p[2], p[1], p[0]]),
                _ => unreachable!("checked by format_supported"),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrgb8888_sets_opaque_alpha_and_swaps_channels() {
        // Little-endian XRGB8888: memory bytes [B, G, R, X].
        let src = [0x33, 0x22, 0x11, 0x00, 0x66, 0x55, 0x44, 0x77];
        let mut dst = [0u8; 8];
        to_rgba(&src, 2, 1, 8, 0, DrmFourcc::Xrgb8888, &mut dst).unwrap();
        assert_eq!(dst, [0x11, 0x22, 0x33, 0xff, 0x44, 0x55, 0x66, 0xff]);
    }

    #[test]
    fn argb8888_preserves_alpha() {
        let src = [0x30, 0x20, 0x10, 0x80];
        let mut dst = [0u8; 4];
        to_rgba(&src, 1, 1, 4, 0, DrmFourcc::Argb8888, &mut dst).unwrap();
        assert_eq!(dst, [0x10, 0x20, 0x30, 0x80]);
    }

    #[test]
    fn abgr8888_and_xbgr8888_with_padded_stride_and_offset() {
        // 2x2 image, pitch=12 (4 bytes padding per row), offset=4.
        let src = [
            0xde, 0xad, 0xbe, 0xef, // leading offset
            1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 99, 99,
            9, 10, 11, 12, 13, 14, 15, 16, 88, 88, 88, 88,
        ];
        let mut dst = [0u8; 16];

        to_rgba(&src, 2, 2, 12, 4, DrmFourcc::Abgr8888, &mut dst).unwrap();
        assert_eq!(
            dst,
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );

        to_rgba(&src, 2, 2, 12, 4, DrmFourcc::Xbgr8888, &mut dst).unwrap();
        assert_eq!(
            dst,
            [1, 2, 3, 0xff, 5, 6, 7, 0xff, 9, 10, 11, 0xff, 13, 14, 15, 0xff]
        );
    }

    #[test]
    fn bgrx_bgra_rgbx_rgba() {
        let src = [0x40, 0x10, 0x20, 0x30];
        let mut dst = [0u8; 4];

        to_rgba(&src, 1, 1, 4, 0, DrmFourcc::Bgrx8888, &mut dst).unwrap();
        assert_eq!(dst, [0x10, 0x20, 0x30, 0xff]);

        to_rgba(&src, 1, 1, 4, 0, DrmFourcc::Bgra8888, &mut dst).unwrap();
        assert_eq!(dst, [0x10, 0x20, 0x30, 0x40]);

        to_rgba(&src, 1, 1, 4, 0, DrmFourcc::Rgbx8888, &mut dst).unwrap();
        assert_eq!(dst, [0x30, 0x20, 0x10, 0xff]);

        to_rgba(&src, 1, 1, 4, 0, DrmFourcc::Rgba8888, &mut dst).unwrap();
        assert_eq!(dst, [0x30, 0x20, 0x10, 0x40]);
    }

    #[test]
    fn rejects_invalid_inputs() {
        let src = [0u8; 16];
        let mut dst = [0u8; 16];

        assert!(to_rgba(&src, 0, 2, 8, 0, DrmFourcc::Xrgb8888, &mut dst).is_err());
        assert!(to_rgba(&src, 2, 0, 8, 0, DrmFourcc::Xrgb8888, &mut dst).is_err());
        assert!(to_rgba(&src, 2, 2, 4, 0, DrmFourcc::Xrgb8888, &mut dst).is_err());
        assert!(to_rgba(&src[..15], 2, 2, 8, 0, DrmFourcc::Xrgb8888, &mut dst).is_err());
        assert!(to_rgba(&src, 2, 2, 8, 0, DrmFourcc::Xrgb8888, &mut dst[..15]).is_err());
        assert!(to_rgba(&src, 2, 2, 8, 0, DrmFourcc::Nv12, &mut dst).is_err());
        // Non-zero offset pushes required span past src.len().
        assert!(to_rgba(&src, 2, 2, 8, 4, DrmFourcc::Xrgb8888, &mut dst).is_err());
        // Overflow guards.
        assert!(to_rgba(&src, u32::MAX, 1, u32::MAX, 0, DrmFourcc::Xrgb8888, &mut dst).is_err());
        assert!(to_rgba(&src, 1, u32::MAX, 4, u32::MAX, DrmFourcc::Xrgb8888, &mut dst).is_err());
        assert!(to_rgba(&src, 1, 1, 4, u32::MAX, DrmFourcc::Xrgb8888, &mut dst).is_err());
    }
}
