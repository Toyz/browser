//! H.264 through VideoToolbox (the Mac's hardware decoder): samples as MP4
//! stores them (length-prefixed NAL units) in, BGRA pictures out (in decode
//! order, as they finish: decoding is asynchronous). openh264 decodes 1080p60 at about 0.6x real time on one core;
//! this keeps up with what the hardware does, at a fraction of the CPU.

#![allow(non_upper_case_globals, non_snake_case)]

use std::ffi::c_void;
use std::sync::Mutex;

use anyhow::{Result, anyhow};

type OSStatus = i32;
type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

const CM_TIME_VALID: u32 = 1;
const INVALID_TIME: CMTime = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };
const TIMESCALE: i32 = 90_000;

#[repr(C)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation: CMTime,
    decode: CMTime,
}

type OutputCallback = extern "C" fn(refcon: *mut c_void, frame_refcon: *mut c_void, status: OSStatus, flags: u32, image: CFTypeRef, pts: CMTime, duration: CMTime);

#[repr(C)]
struct OutputCallbackRecord {
    callback: OutputCallback,
    refcon: *mut c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFRelease(cf: CFTypeRef);
    fn CFNumberCreate(alloc: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
    fn CFDictionaryCreate(alloc: CFTypeRef, keys: *const CFTypeRef, values: *const CFTypeRef, count: isize, key_callbacks: *const c_void, value_callbacks: *const c_void) -> CFTypeRef;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        alloc: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        alloc: CFTypeRef,
        memory: *mut c_void,
        length: usize,
        block_alloc: CFTypeRef,
        custom: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMBlockBufferReplaceDataBytes(source: *const c_void, block: CFTypeRef, offset: usize, length: usize) -> OSStatus;
    fn CMSampleBufferCreateReady(
        alloc: CFTypeRef,
        data: CFTypeRef,
        format: CFTypeRef,
        samples: isize,
        timings: isize,
        timing: *const CMSampleTimingInfo,
        sizes_count: isize,
        sizes: *const usize,
        out: *mut CFTypeRef,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
    fn CVPixelBufferLockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: CFTypeRef, plane: usize) -> *const u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CFTypeRef, plane: usize) -> usize;
    fn CVPixelBufferGetWidth(buffer: CFTypeRef) -> usize;
    fn CVPixelBufferGetHeight(buffer: CFTypeRef) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTDecompressionSessionCreate(
        alloc: CFTypeRef,
        format: CFTypeRef,
        decoder_spec: CFTypeRef,
        image_attrs: CFTypeRef,
        callback: *const OutputCallbackRecord,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(session: CFTypeRef, sample: CFTypeRef, flags: u32, frame_refcon: *mut c_void, info_out: *mut u32) -> OSStatus;
    fn VTDecompressionSessionWaitForAsynchronousFrames(session: CFTypeRef) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: CFTypeRef);
}

/// `kCMBlockBufferAssureMemoryNowFlag`.
const ASSURE_MEMORY_NOW: u32 = 1;
/// `kVTDecodeFrame_EnableAsynchronousDecompression`: decoding waiting on
/// each picture slowed down to tens of milliseconds a sample on Twitch.
const ASYNCHRONOUS: u32 = 1;
/// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` (NV12): what the
/// decoder makes. (Asking it for BGRA had the GPU convert every picture;
/// with the page drawing too, decoding fell to tens of ms a sample.)
const NV12: i32 = 0x3432_3076;
/// `kCFNumberSInt32Type`.
const SINT32: isize = 3;

/// A decoded picture: (time, width, height, BGRA bytes).
pub(crate) type Picture = (f64, u32, u32, Vec<u8>);

pub(crate) struct Session {
    session: CFTypeRef,
    format: CFTypeRef,
    length_size: usize,
    /// Where the callback puts pictures (boxed: its address is the callback's).
    out: Box<Mutex<Vec<Picture>>>,
}

// The session is only used from the thread that owns it.
unsafe impl Send for Session {}

extern "C" fn on_picture(refcon: *mut c_void, _frame: *mut c_void, status: OSStatus, _flags: u32, image: CFTypeRef, pts: CMTime, _duration: CMTime) {
    if status != 0 || image.is_null() || refcon.is_null() {
        return;
    }
    // SAFETY: `refcon` is the session's `out`, alive while the session is;
    // `image` is a locked-for-reading BGRA pixel buffer for this call.
    unsafe {
        let out = &*(refcon as *const Mutex<Vec<Picture>>);
        if CVPixelBufferLockBaseAddress(image, 1) != 0 {
            return;
        }
        let (w, h) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        let (luma, chroma) = (CVPixelBufferGetBaseAddressOfPlane(image, 0), CVPixelBufferGetBaseAddressOfPlane(image, 1));
        let (ls, cs) = (CVPixelBufferGetBytesPerRowOfPlane(image, 0), CVPixelBufferGetBytesPerRowOfPlane(image, 1));
        if !luma.is_null() && !chroma.is_null() && ls >= w && cs >= w.div_ceil(2) * 2 {
            let y = std::slice::from_raw_parts(luma, ls * h);
            let uv = std::slice::from_raw_parts(chroma, cs * h.div_ceil(2));
            let bgra = nv12_to_bgra(y, ls, uv, cs, w, h);
            let time = if pts.flags & CM_TIME_VALID != 0 && pts.timescale > 0 { pts.value as f64 / f64::from(pts.timescale) } else { 0.0 };
            out.lock().unwrap_or_else(|e| e.into_inner()).push((time, w as u32, h as u32, bgra));
        }
        CVPixelBufferUnlockBaseAddress(image, 1);
    }
}

/// Video-range NV12 to BGRA (BT.709 for HD pictures, BT.601 below), in
/// 16.16 fixed point.
fn nv12_to_bgra(y: &[u8], ys: usize, uv: &[u8], uvs: usize, w: usize, h: usize) -> Vec<u8> {
    let hd = h >= 720;
    // (Y-16)*1.164, V*R, U*G, V*G, U*B.
    let (yk, rv, gu, gv, bu): (i32, i32, i32, i32, i32) =
        if hd { (76309, 117489, 13975, 34925, 138438) } else { (76309, 104597, 25675, 53279, 132201) };
    let mut out = vec![0u8; w * h * 4];
    for row in 0..h {
        let yrow = &y[row * ys..row * ys + w];
        let crow = &uv[(row / 2) * uvs..];
        let orow = &mut out[row * w * 4..(row + 1) * w * 4];
        for x in 0..w {
            let c = (yrow[x] as i32 - 16).max(0) * yk;
            let u = crow[(x / 2) * 2] as i32 - 128;
            let v = crow[(x / 2) * 2 + 1] as i32 - 128;
            let r = (c + rv * v + 32768) >> 16;
            let g = (c - gu * u - gv * v + 32768) >> 16;
            let b = (c + bu * u + 32768) >> 16;
            let o = &mut orow[x * 4..x * 4 + 4];
            o[0] = b.clamp(0, 255) as u8;
            o[1] = g.clamp(0, 255) as u8;
            o[2] = r.clamp(0, 255) as u8;
            o[3] = 255;
        }
    }
    out
}

/// `avcC`'s parameter sets (SPS, then PPS) and its length prefix size.
fn parameter_sets(avcc: &[u8]) -> Option<(Vec<Vec<u8>>, usize)> {
    let length_size = usize::from(*avcc.get(4)? & 3) + 1;
    let mut at = 5;
    let mut sets = Vec::new();
    for count_mask in [0x1f, 0xff] {
        let count = usize::from(*avcc.get(at)? & count_mask);
        at += 1;
        for _ in 0..count {
            let len = usize::from(u16::from_be_bytes([*avcc.get(at)?, *avcc.get(at + 1)?]));
            at += 2;
            sets.push(avcc.get(at..at + len)?.to_vec());
            at += len;
        }
    }
    (!sets.is_empty()).then_some((sets, length_size))
}

impl Session {
    /// A hardware decoder for the stream `avcc` describes.
    pub fn new(avcc: &[u8]) -> Result<Session> {
        // Opt-in for now: on Twitch (60fps, the page busy) submitting
        // samples slowed from 0.2ms to tens, even hundreds, of ms after
        // ~15s, for reasons not yet found; openh264 kept up better at 720p.
        // YouTube played cleanly.
        if std::env::var_os("MEDIA_HARDWARE").is_none() {
            return Err(anyhow!("hardware decoding not asked for (MEDIA_HARDWARE=1)"));
        }
        let (sets, length_size) = parameter_sets(avcc).ok_or_else(|| anyhow!("no parameter sets"))?;
        let pointers: Vec<*const u8> = sets.iter().map(|s| s.as_ptr()).collect();
        let sizes: Vec<usize> = sets.iter().map(Vec::len).collect();
        let mut format: CFTypeRef = std::ptr::null();
        // SAFETY: the pointers and sizes describe `sets`, alive for the call.
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(std::ptr::null(), sets.len(), pointers.as_ptr(), sizes.as_ptr(), length_size as i32, &mut format)
        };
        if status != 0 || format.is_null() {
            return Err(anyhow!("format description: {status}"));
        }
        let out: Box<Mutex<Vec<Picture>>> = Box::default();
        let record = OutputCallbackRecord { callback: on_picture, refcon: &*out as *const Mutex<Vec<Picture>> as *mut c_void };
        let mut session: CFTypeRef = std::ptr::null();
        // SAFETY: CoreFoundation objects made and released here; `record`
        // is copied by the call; `out` outlives the session (see Drop).
        let status = unsafe {
            let value = NV12;
            let number = CFNumberCreate(std::ptr::null(), SINT32, &value as *const i32 as *const c_void);
            let keys = [kCVPixelBufferPixelFormatTypeKey];
            let values = [number];
            let attrs = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const c_void,
            );
            let status = VTDecompressionSessionCreate(std::ptr::null(), format, std::ptr::null(), attrs, &record, &mut session);
            CFRelease(attrs);
            CFRelease(number);
            status
        };
        if status != 0 || session.is_null() {
            // SAFETY: made above.
            unsafe { CFRelease(format) };
            return Err(anyhow!("decompression session: {status}"));
        }
        Ok(Session { session, format, length_size, out })
    }

    /// Decode one sample shown at `time`; the pictures that came out (in
    /// decode order).
    pub fn decode(&mut self, data: &[u8], time: f64) -> Vec<Picture> {
        if data.len() < self.length_size {
            return Vec::new();
        }
        let mut block: CFTypeRef = std::ptr::null();
        let mut sample: CFTypeRef = std::ptr::null();
        // SAFETY: the block buffer owns a copy of `data` (allocated by
        // CoreMedia), so asynchronous decoding may read it after this returns.
        unsafe {
            let mut status = CMBlockBufferCreateWithMemoryBlock(
                std::ptr::null(),
                std::ptr::null_mut(),
                data.len(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                data.len(),
                ASSURE_MEMORY_NOW,
                &mut block,
            );
            if status == 0 {
                status = CMBlockBufferReplaceDataBytes(data.as_ptr() as *const c_void, block, 0, data.len());
            }
            if status == 0 {
                let timing = CMSampleTimingInfo {
                    duration: INVALID_TIME,
                    presentation: CMTime { value: (time * f64::from(TIMESCALE)).round() as i64, timescale: TIMESCALE, flags: CM_TIME_VALID, epoch: 0 },
                    decode: INVALID_TIME,
                };
                let size = data.len();
                if CMSampleBufferCreateReady(std::ptr::null(), block, self.format, 1, 1, &timing, 1, &size, &mut sample) == 0 {
                    let mut info = 0u32;
                    let status = VTDecompressionSessionDecodeFrame(self.session, sample, ASYNCHRONOUS, std::ptr::null_mut(), &mut info);
                    if status != 0 && std::env::var_os("MEDIA_DEBUG").is_some() {
                        eprintln!("media: videotoolbox sample at {time:.3}s: {status}");
                    }
                    CFRelease(sample);
                }
                CFRelease(block);
            }
        }
        std::mem::take(&mut *self.out.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Whatever pictures are still inside.
    pub fn flush(&mut self) -> Vec<Picture> {
        // SAFETY: a live session.
        unsafe { VTDecompressionSessionWaitForAsynchronousFrames(self.session) };
        std::mem::take(&mut *self.out.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: made in `new`; invalidated before `out` is freed.
        unsafe {
            VTDecompressionSessionInvalidate(self.session);
            CFRelease(self.session);
            CFRelease(self.format);
        }
    }
}
