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

//! fbgrab: dumps the active primary-plane DRM framebuffer(s) of a card
//! to raw RGBA8888 files (`<prefix>.<fb_id>.raw`).

mod convert;

use drm::buffer;
use drm::control::{framebuffer, plane, Device as ControlDevice, PlaneType};
use drm::{ClientCapability, Device};
use drm_fourcc::{DrmFourcc, DrmModifier};
use std::collections::HashSet;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::num::NonZeroU32;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::process::ExitCode;
use std::ptr;
use std::slice;

// Linux <linux/dma-buf.h> DMA_BUF_IOCTL_SYNC definitions.
#[repr(C)]
struct DmaBufSync {
    flags: u64,
}

const DMA_BUF_SYNC_READ: u64 = 1 << 0;
const DMA_BUF_SYNC_START: u64 = 0 << 2;
const DMA_BUF_SYNC_END: u64 = 1 << 2;
// _IOW('b', 0, struct dma_buf_sync)
const DMA_BUF_IOCTL_SYNC: u32 = 0x4008_6200;

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Device for Card {}
impl ControlDevice for Card {}

struct GemGuard<'a> {
    card: &'a Card,
    handle: buffer::Handle,
}

impl Drop for GemGuard<'_> {
    fn drop(&mut self) {
        let _ = self.card.close_buffer(self.handle);
    }
}

struct MmapGuard {
    ptr: *mut libc::c_void,
    len: usize,
}

impl MmapGuard {
    fn map_ro(fd: BorrowedFd<'_>, len: usize, offset: libc::off_t) -> io::Result<Self> {
        // SAFETY: mmap with NULL addr and valid fd/len/offset; checked against MAP_FAILED.
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                offset,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { ptr, len })
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr is valid for self.len bytes for the lifetime of Self.
        unsafe { slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) }
    }
}

impl Drop for MmapGuard {
    fn drop(&mut self) {
        // SAFETY: ptr/len came from a successful libc::mmap call in map_ro.
        unsafe {
            let _ = libc::munmap(self.ptr, self.len);
        }
    }
}

struct FbInfo {
    fb_id: u32,
    width: u32,
    height: u32,
    pitch: u32,
    offset: u32,
    fourcc: DrmFourcc,
    handle: buffer::Handle,
    modifier: Option<DrmModifier>,
}

fn usage(prog: &str) {
    eprintln!(
        "Usage: {prog} [-d <drm-device>] <output-prefix>\n\
         \n\
         Dumps the active primary-plane DRM framebuffer(s) to\n\
         <output-prefix>.<fb_id>.raw as tightly packed RGBA8888.\n\
         \n\
         Options:\n\
           -d <path>  DRM device node (default: /dev/dri/card0)"
    );
}

fn is_primary_plane(card: &Card, handle: plane::Handle) -> bool {
    let Ok(props) = card.get_properties(handle) else {
        return false;
    };
    for (&prop_handle, &raw_val) in props.iter() {
        let Ok(prop) = card.get_property(prop_handle) else {
            continue;
        };
        if prop.name().to_bytes() == b"type" {
            return raw_val == PlaneType::Primary as u64;
        }
    }
    false
}

fn format_raw_fourcc(raw: u32) -> String {
    let bytes = raw.to_le_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        format!("0x{raw:08x}")
    }
}

fn close_unique_handles(card: &Card, handles: [u32; 4], keep: Option<u32>) {
    let mut closed = [0u32; 4];
    let mut count = 0usize;
    for h in handles {
        if h == 0 || Some(h) == keep || closed[..count].contains(&h) {
            continue;
        }
        closed[count] = h;
        count += 1;
        if let Some(nz) = NonZeroU32::new(h) {
            let _ = card.close_buffer(buffer::Handle::from(nz));
        }
    }
}

fn query_fb(card: &Card, fb_handle: framebuffer::Handle) -> Result<FbInfo, String> {
    let fb_id: u32 = fb_handle.into();
    match drm_ffi::mode::get_framebuffer2(card.as_fd(), fb_id) {
        Ok(fb2) => {
            let primary_raw = fb2.handles[0];
            let Some(primary_nz) = NonZeroU32::new(primary_raw) else {
                close_unique_handles(card, fb2.handles, None);
                return Err(format!(
                    "fb {fb_id} has handle 0 (root / CAP_SYS_ADMIN is required)"
                ));
            };
            close_unique_handles(card, fb2.handles, Some(primary_raw));
            let handle = buffer::Handle::from(primary_nz);
            let fourcc = match DrmFourcc::try_from(fb2.pixel_format) {
                Ok(fmt) => fmt,
                Err(_) => {
                    let _ = card.close_buffer(handle);
                    return Err(format!(
                        "unrecognized pixel format '{}' (0x{:08x}) on fb {fb_id}",
                        format_raw_fourcc(fb2.pixel_format),
                        fb2.pixel_format
                    ));
                }
            };
            let modifier = if (fb2.flags & drm_ffi::DRM_MODE_FB_MODIFIERS) != 0 {
                Some(DrmModifier::from(fb2.modifier[0]))
            } else {
                None
            };
            Ok(FbInfo {
                fb_id,
                width: fb2.width,
                height: fb2.height,
                pitch: fb2.pitches[0],
                offset: fb2.offsets[0],
                fourcc,
                handle,
                modifier,
            })
        }
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ENOSYS | libc::EOPNOTSUPP | libc::EINVAL)
            ) =>
        {
            let fb = card
                .get_framebuffer(fb_handle)
                .map_err(|err| format!("get_framebuffer({fb_id}) failed: {err}"))?;
            let handle = fb.buffer().ok_or_else(|| {
                format!("fb {fb_id} has handle 0 (root / CAP_SYS_ADMIN is required)")
            })?;
            let fourcc = match (fb.bpp(), fb.depth()) {
                (32, 24) => DrmFourcc::Xrgb8888,
                (32, 32) => DrmFourcc::Argb8888,
                (bpp, depth) => {
                    let _ = card.close_buffer(handle);
                    return Err(format!(
                        "unsupported legacy fb {fb_id} format (bpp={bpp}, depth={depth})"
                    ));
                }
            };
            let (width, height) = fb.size();
            Ok(FbInfo {
                fb_id,
                width,
                height,
                pitch: fb.pitch(),
                offset: 0,
                fourcc,
                handle,
                modifier: None,
            })
        }
        Err(e) => Err(format!("DRM_IOCTL_MODE_GETFB2({fb_id}) failed: {e}")),
    }
}

fn dma_buf_sync(fd: BorrowedFd<'_>, flags: u64) {
    let mut sync = DmaBufSync { flags };
    loop {
        // SAFETY: DMA_BUF_IOCTL_SYNC takes a pointer to DmaBufSync on a valid dma-buf fd.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC as _, &mut sync) };
        if ret == 0 {
            return;
        }
        let err = io::Error::last_os_error();
        if matches!(err.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
            continue;
        }
        eprintln!("fbgrab: warning: DMA_BUF_IOCTL_SYNC(0x{flags:x}) failed: {err}");
        return;
    }
}

fn map_and_convert(card: &Card, info: &FbInfo, rgba: &mut [u8]) -> Result<(), String> {
    if info.width == 0 || info.height == 0 || info.pitch == 0 {
        return Err(format!(
            "invalid geometry {}x{} pitch={}",
            info.width, info.height, info.pitch
        ));
    }
    let row_bytes = (info.width as usize)
        .checked_mul(4)
        .ok_or_else(|| "row byte size overflow".to_string())?;
    if (info.pitch as usize) < row_bytes {
        return Err(format!(
            "pitch {} smaller than row width {row_bytes}",
            info.pitch
        ));
    }
    // Minimum required BO length matching Linux drm_gem_fb_create_with_dirty.
    let map_len = (info.height as usize - 1)
        .checked_mul(info.pitch as usize)
        .and_then(|v| v.checked_add(row_bytes))
        .and_then(|v| v.checked_add(info.offset as usize))
        .ok_or_else(|| "framebuffer mapping size overflow".to_string())?;

    // Prefer PRIME dma-buf export + DMA_BUF_IOCTL_SYNC (driver-agnostic).
    let prime_err = match card.buffer_to_prime_fd(info.handle, drm_ffi::DRM_CLOEXEC) {
        Ok(dma_fd) => match MmapGuard::map_ro(dma_fd.as_fd(), map_len, 0) {
            Ok(mapping) => {
                dma_buf_sync(dma_fd.as_fd(), DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
                let res = convert::to_rgba(
                    mapping.as_slice(),
                    info.width,
                    info.height,
                    info.pitch,
                    info.offset,
                    info.fourcc,
                    rgba,
                );
                dma_buf_sync(dma_fd.as_fd(), DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
                return res;
            }
            Err(e) => format!("prime mmap failed ({e})"),
        },
        Err(e) => format!("prime export failed ({e})"),
    };

    // Fallback: DRM_IOCTL_MODE_MAP_DUMB for dumb scanout buffers.
    let map_req = drm_ffi::mode::dumbbuffer::map(card.as_fd(), info.handle.into(), 0, 0)
        .map_err(|e| format!("{prime_err} and DRM_IOCTL_MODE_MAP_DUMB failed ({e})"))?;
    let map_offset = libc::off_t::try_from(map_req.offset)
        .map_err(|_| format!("MAP_DUMB offset {} overflows off_t", map_req.offset))?;
    let mapping = MmapGuard::map_ro(card.as_fd(), map_len, map_offset)
        .map_err(|e| format!("{prime_err} and MAP_DUMB mmap failed ({e})"))?;

    convert::to_rgba(
        mapping.as_slice(),
        info.width,
        info.height,
        info.pitch,
        info.offset,
        info.fourcc,
        rgba,
    )
}

fn dump_fb(
    card: &Card,
    plane_id: u32,
    crtc_id: u32,
    fb_handle: framebuffer::Handle,
    prefix: &str,
) -> Result<(), String> {
    let info = query_fb(card, fb_handle)?;
    let _gem = GemGuard {
        card,
        handle: info.handle,
    };

    let mod_val: u64 = info.modifier.map(u64::from).unwrap_or(0);
    if let Some(m) = info.modifier {
        if m != DrmModifier::Linear {
            eprintln!(
                "fbgrab: warning: fb {} has non-linear modifier 0x{:x} (expected 0x0); \
                 output may be tiled or compressed",
                info.fb_id, mod_val
            );
        }
    }

    if !convert::format_supported(info.fourcc) {
        return Err(format!(
            "unsupported pixel format '{}' on fb {}",
            info.fourcc, info.fb_id
        ));
    }

    let rgba_len = (info.width as usize)
        .checked_mul(info.height as usize)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| {
            format!(
                "invalid dimensions {}x{} on fb {}",
                info.width, info.height, info.fb_id
            )
        })?;
    if rgba_len == 0 {
        return Err(format!(
            "invalid dimensions {}x{} on fb {}",
            info.width, info.height, info.fb_id
        ));
    }

    let mut rgba = vec![0u8; rgba_len];
    map_and_convert(card, &info, &mut rgba)?;

    let out_path = format!("{}.{}.raw", prefix, info.fb_id);
    let mut file = File::create(&out_path)
        .map_err(|e| format!("failed to open {out_path}: {e}"))?;
    file.write_all(&rgba)
        .and_then(|()| file.flush())
        .map_err(|e| format!("failed writing {out_path}: {e}"))?;

    println!(
        "plane {plane_id} crtc {crtc_id} fb {}: {}x{} fmt={} pitch={} offset={} modifier=0x{:x} -> {out_path}",
        info.fb_id, info.width, info.height, info.fourcc, info.pitch, info.offset, mod_val
    );
    Ok(())
}

fn parse_args() -> Result<(String, String), ExitCode> {
    let args: Vec<String> = env::args().collect();
    let prog = args.first().map(String::as_str).unwrap_or("fbgrab");
    let mut device = "/dev/dri/card0".to_string();
    let mut prefix: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                usage(prog);
                return Err(ExitCode::SUCCESS);
            }
            "-d" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    eprintln!("fbgrab: -d requires an argument");
                    usage(prog);
                    return Err(ExitCode::from(2));
                };
                device = val.clone();
            }
            arg if arg.starts_with('-') => {
                eprintln!("fbgrab: unknown option '{arg}'");
                usage(prog);
                return Err(ExitCode::from(2));
            }
            arg => {
                if prefix.is_some() {
                    usage(prog);
                    return Err(ExitCode::from(2));
                }
                prefix = Some(arg.to_string());
            }
        }
        i += 1;
    }

    let Some(prefix) = prefix else {
        usage(prog);
        return Err(ExitCode::from(2));
    };
    Ok((device, prefix))
}

fn main() -> ExitCode {
    let (device, prefix) = match parse_args() {
        Ok(v) => v,
        Err(code) => return code,
    };

    let file = match OpenOptions::new().read(true).open(&device) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("fbgrab: failed to open {device}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let card = Card(file);

    let _ = card.set_client_capability(ClientCapability::UniversalPlanes, true);

    let mut dumped = 0usize;
    let mut errors = 0usize;
    let mut seen_fbs = HashSet::new();
    let mut query_err: Option<String> = None;

    match card.plane_handles() {
        Ok(planes) => {
            for plane_handle in planes {
                let Ok(plane_info) = card.get_plane(plane_handle) else {
                    continue;
                };
                if is_primary_plane(&card, plane_handle) {
                    if let Some(fb_handle) = plane_info.framebuffer() {
                        let fb_id: u32 = fb_handle.into();
                        if !seen_fbs.insert(fb_id) {
                            continue;
                        }
                        let plane_id: u32 = plane_handle.into();
                        let crtc_id: u32 = plane_info.crtc().map(u32::from).unwrap_or(0);
                        match dump_fb(&card, plane_id, crtc_id, fb_handle, &prefix) {
                            Ok(()) => dumped += 1,
                            Err(e) => {
                                eprintln!("fbgrab: {e}");
                                errors += 1;
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            query_err = Some(format!("plane_handles failed on {device}: {e}"));
        }
    }

    // Fallback to active CRTCs if no primary plane had an active FB.
    if dumped == 0 && errors == 0 {
        match card.resource_handles() {
            Ok(res) => {
                for &crtc_handle in res.crtcs() {
                    let Ok(crtc) = card.get_crtc(crtc_handle) else {
                        continue;
                    };
                    if let Some(fb_handle) = crtc.framebuffer() {
                        let fb_id: u32 = fb_handle.into();
                        if !seen_fbs.insert(fb_id) {
                            continue;
                        }
                        let crtc_id: u32 = crtc_handle.into();
                        match dump_fb(&card, 0, crtc_id, fb_handle, &prefix) {
                            Ok(()) => dumped += 1,
                            Err(e) => {
                                eprintln!("fbgrab: {e}");
                                errors += 1;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                if let Some(prev) = query_err {
                    eprintln!("fbgrab: {prev}; resource_handles failed on {device}: {e}");
                } else {
                    eprintln!("fbgrab: resource_handles failed on {device}: {e}");
                }
                return ExitCode::FAILURE;
            }
        }
    }

    if dumped == 0 {
        if errors == 0 {
            if let Some(prev) = query_err {
                eprintln!("fbgrab: {prev}; no active primary framebuffer found on {device}");
            } else {
                eprintln!("fbgrab: no active primary framebuffer found on {device}");
            }
        }
        return ExitCode::FAILURE;
    }
    if errors > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
