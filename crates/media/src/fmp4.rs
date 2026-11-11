//! Fragmented MP4 as Media Source Extensions append it: an initialization
//! segment (`ftyp` + `moov`: the tracks and their codecs), then media
//! segments (`moof` + `mdat`: samples with their times). Just the boxes
//! that takes; everything else is skipped.

use std::collections::HashMap;
use std::sync::Arc;

/// A track's codec, from its sample description.
#[derive(Debug, Clone, PartialEq)]
pub enum Codec {
    /// H.264, with its `avcC`.
    Avc { avcc: Vec<u8>, width: u16, height: u16 },
    /// AAC, with its AudioSpecificConfig; or MP3 (`asc` empty).
    Aac { asc: Vec<u8>, rate: u32, channels: u16 },
    Mp3 { rate: u32, channels: u16 },
    /// Something this can't decode (its four-character code).
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: u32,
    /// Ticks per second.
    pub timescale: u32,
    pub codec: Codec,
    defaults: Defaults,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Defaults {
    duration: u32,
    size: u32,
    flags: u32,
}

/// What an initialization segment says.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Init {
    pub tracks: Vec<Track>,
}

/// One coded frame.
#[derive(Clone)]
pub struct Sample {
    pub track: u32,
    /// Seconds.
    pub dts: f64,
    pub pts: f64,
    pub duration: f64,
    pub key: bool,
    /// Its bytes: a range of the segment's `mdat` (shared, not copied).
    pub data: Arc<[u8]>,
    pub range: std::ops::Range<usize>,
}

impl Sample {
    pub fn bytes(&self) -> &[u8] {
        &self.data[self.range.clone()]
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The boxes in `b`: (type, body, offset of the box in `b`). A box cut off
/// ends the list.
pub(crate) fn boxes(b: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8], usize)> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let (size, header) = whole_box(&b[at..])?;
        let kind: [u8; 4] = b[at + 4..at + 8].try_into().ok()?;
        let start = at;
        let body = &b[at + header..at + size];
        at += size;
        Some((kind, body, start))
    })
}

/// The size of the box starting `b` and of its header, if it's all there.
pub(crate) fn whole_box(b: &[u8]) -> Option<(usize, usize)> {
    let size = u32_at(b, 0)? as usize;
    let (size, header) = match size {
        1 => (usize::try_from(u64_at(b, 8)?).ok()?, 16),
        // "To the end": only an `mdat`, and only when it's all here.
        0 => (b.len(), 8),
        n => (n, 8),
    };
    (size >= header && size <= b.len()).then_some((size, header))
}

fn child<'a>(b: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(b).find(|(k, ..)| k == kind).map(|(_, body, _)| body)
}

/// An initialization segment's `moov` (the box's body).
pub fn parse_moov(moov: &[u8]) -> Init {
    let mut defaults: HashMap<u32, Defaults> = HashMap::new();
    if let Some(mvex) = child(moov, b"mvex") {
        for (kind, trex, _) in boxes(mvex) {
            if &kind == b"trex"
                && let (Some(id), Some(duration), Some(size), Some(flags)) = (u32_at(trex, 4), u32_at(trex, 12), u32_at(trex, 16), u32_at(trex, 20))
            {
                defaults.insert(id, Defaults { duration, size, flags });
            }
        }
    }
    let mut init = Init::default();
    for (kind, trak, _) in boxes(moov) {
        if &kind != b"trak" {
            continue;
        }
        let Some(track) = parse_trak(trak) else { continue };
        let defaults = defaults.get(&track.0).copied().unwrap_or_default();
        init.tracks.push(Track { id: track.0, timescale: track.1, codec: track.2, defaults });
    }
    init
}

fn parse_trak(trak: &[u8]) -> Option<(u32, u32, Codec)> {
    let tkhd = child(trak, b"tkhd")?;
    let id = if tkhd[0] == 1 { u32_at(tkhd, 20)? } else { u32_at(tkhd, 12)? };
    let mdia = child(trak, b"mdia")?;
    let mdhd = child(mdia, b"mdhd")?;
    let timescale = if mdhd[0] == 1 { u32_at(mdhd, 20)? } else { u32_at(mdhd, 12)? };
    let stsd = child(child(child(mdia, b"minf")?, b"stbl")?, b"stsd")?;
    // (version/flags, entry count) then the sample entries.
    let (kind, entry, _) = boxes(stsd.get(8..)?).next()?;
    let codec = match &kind {
        b"avc1" | b"avc3" => {
            let width = u16_at(entry, 24)?;
            let height = u16_at(entry, 26)?;
            let avcc = child(entry.get(78..)?, b"avcC")?.to_vec();
            Codec::Avc { avcc, width, height }
        }
        b"mp4a" => {
            let channels = u16_at(entry, 16)?;
            let rate = u32_at(entry, 24)? >> 16;
            let esds = child(entry.get(28..)?, b"esds")?;
            match es_config(esds.get(4..)?) {
                Some((0x40 | 0x66..=0x68, asc)) => Codec::Aac { asc, rate, channels },
                Some((0x69 | 0x6b, _)) => Codec::Mp3 { rate, channels },
                _ => Codec::Other("mp4a".into()),
            }
        }
        other => Codec::Other(String::from_utf8_lossy(other).into_owned()),
    };
    Some((id, timescale, codec))
}

/// An `esds` ES descriptor's object type and decoder-specific info.
fn es_config(b: &[u8]) -> Option<(u8, Vec<u8>)> {
    // Descriptors: tag, then a length in 7-bit groups.
    fn descriptor(b: &[u8]) -> Option<(u8, &[u8], usize)> {
        let tag = *b.first()?;
        let (mut len, mut at) = (0usize, 1);
        loop {
            let byte = *b.get(at)?;
            at += 1;
            len = (len << 7) | usize::from(byte & 0x7f);
            if byte & 0x80 == 0 || at > 4 {
                break;
            }
        }
        Some((tag, b.get(at..(at + len).min(b.len()))?, at + len))
    }
    let (tag, es, _) = descriptor(b)?;
    if tag != 3 {
        return None;
    }
    let flags = *es.get(2)?;
    let mut at = 3;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at)?);
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    let (tag, config, _) = descriptor(es.get(at..)?)?;
    if tag != 4 {
        return None;
    }
    let object_type = *config.first()?;
    let asc = descriptor(config.get(13..)?).filter(|(t, ..)| *t == 5).map(|(_, d, _)| d.to_vec()).unwrap_or_default();
    Some((object_type, asc))
}

/// A media segment's samples: `moof` (the whole box, `moof_at` into
/// `data`) and the `mdat` after it, all in `data`. Times are seconds plus
/// `offset` (the SourceBuffer's `timestampOffset`).
pub fn parse_fragment(init: &Init, data: &Arc<[u8]>, moof_at: usize, moof: &[u8], offset: f64) -> Vec<Sample> {
    let mut out = Vec::new();
    for (kind, traf, _) in boxes(moof) {
        if &kind != b"traf" {
            continue;
        }
        let Some(tfhd) = child(traf, b"tfhd") else { continue };
        let Some(flags) = u32_at(tfhd, 0) else { continue };
        let Some(id) = u32_at(tfhd, 4) else { continue };
        let Some(track) = init.tracks.iter().find(|t| t.id == id) else { continue };
        let mut at = 8;
        let mut base = moof_at;
        let mut d = track.defaults;
        if flags & 0x1 != 0 {
            // An absolute offset: counted from the segment here.
            base = moof_at + u64_at(tfhd, at).and_then(|v| usize::try_from(v).ok()).unwrap_or(0);
            at += 8;
        }
        if flags & 0x2 != 0 {
            at += 4;
        }
        if flags & 0x8 != 0 {
            d.duration = u32_at(tfhd, at).unwrap_or(d.duration);
            at += 4;
        }
        if flags & 0x10 != 0 {
            d.size = u32_at(tfhd, at).unwrap_or(d.size);
            at += 4;
        }
        if flags & 0x20 != 0 {
            d.flags = u32_at(tfhd, at).unwrap_or(d.flags);
        }
        let mut decode_time = child(traf, b"tfdt").and_then(|t| if t[0] == 1 { u64_at(t, 4) } else { u32_at(t, 4).map(u64::from) }).unwrap_or(0);
        let scale = f64::from(track.timescale.max(1));
        // Runs without their own offset follow the previous one.
        let mut next = base;
        for (kind, trun, _) in boxes(traf) {
            if &kind != b"trun" {
                continue;
            }
            let (Some(vf), Some(count)) = (u32_at(trun, 0), u32_at(trun, 4)) else { continue };
            let (version, tf) = (vf >> 24, vf & 0xff_ffff);
            let mut at = 8;
            let mut pos = next;
            if tf & 0x1 != 0 {
                let off = u32_at(trun, at).unwrap_or(0) as i32;
                pos = base.checked_add_signed(off as isize).unwrap_or(base);
                at += 4;
            }
            let mut first_flags = None;
            if tf & 0x4 != 0 {
                first_flags = u32_at(trun, at);
                at += 4;
            }
            for i in 0..count {
                let mut field = |present: bool| -> Option<u32> {
                    if !present {
                        return None;
                    }
                    let v = u32_at(trun, at);
                    at += 4;
                    v
                };
                let duration = field(tf & 0x100 != 0).unwrap_or(d.duration);
                let size = field(tf & 0x200 != 0).unwrap_or(d.size) as usize;
                let sample_flags = field(tf & 0x400 != 0).or(if i == 0 { first_flags } else { None }).unwrap_or(d.flags);
                let cto = field(tf & 0x800 != 0).map_or(0i64, |v| if version == 1 { i64::from(v as i32) } else { i64::from(v) });
                let Some(end) = pos.checked_add(size).filter(|&e| e <= data.len()) else { break };
                let dts = decode_time as f64 / scale + offset;
                out.push(Sample {
                    track: id,
                    dts,
                    pts: (decode_time as i64 + cto) as f64 / scale + offset,
                    duration: f64::from(duration) / scale,
                    // `sample_is_non_sync_sample`.
                    key: sample_flags & 0x1_0000 == 0,
                    data: data.clone(),
                    range: pos..end,
                });
                pos = end;
                decode_time += u64::from(duration);
            }
            next = pos;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box: size, type, body.
    pub(crate) fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    #[test]
    fn reads_a_fragment() {
        let init = Init {
            tracks: vec![Track { id: 1, timescale: 1000, codec: Codec::Other("test".into()), defaults: Defaults { duration: 40, size: 0, flags: 0x1_0000 } }],
        };
        // tfhd: default-base-is-moof, track 1.
        let tfhd = mp4_box(b"tfhd", &[0, 2, 0, 0, 0, 0, 0, 1]);
        let tfdt = mp4_box(b"tfdt", &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03, 0xe8]);
        // trun: data offset, first-sample flags, sizes; 2 samples.
        let mut trun_body = vec![0, 0, 0x02, 0x05, 0, 0, 0, 2];
        trun_body.extend_from_slice(&0u32.to_be_bytes()); // data offset, patched below
        trun_body.extend_from_slice(&0u32.to_be_bytes()); // first sample: sync
        trun_body.extend_from_slice(&3u32.to_be_bytes());
        trun_body.extend_from_slice(&2u32.to_be_bytes());
        let traf = |trun: &[u8]| mp4_box(b"traf", &[tfhd.clone(), tfdt.clone(), mp4_box(b"trun", trun)].concat());
        let moof_len = mp4_box(b"moof", &traf(&trun_body)).len();
        trun_body[8..12].copy_from_slice(&((moof_len + 8) as u32).to_be_bytes());
        let moof = mp4_box(b"moof", &traf(&trun_body));
        let data: Arc<[u8]> = [moof.clone(), mp4_box(b"mdat", b"abcde")].concat().into();
        let samples = parse_fragment(&init, &data, 0, &moof[8..], 0.5);
        let got: Vec<(f64, bool, &[u8])> = samples.iter().map(|s| (s.pts, s.key, s.bytes())).collect();
        assert_eq!(got, vec![(1.5, true, &b"abc"[..]), (1.54, false, &b"de"[..])]);
    }
}
