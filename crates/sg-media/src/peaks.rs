//! Multi-resolution audio peaks, computed once at import so that drawing a
//! waveform never touches audio samples.
//!
//! Level 0 holds the minimum and maximum of every [`BASE_BLOCK`] sample
//! frames per channel; each further level covers [`FANOUT`] blocks of the
//! one below. Drawing picks the level whose blocks are about a pixel wide.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::tool::{Cancel, ToolError};

pub const BASE_BLOCK: u64 = 256;
pub const FANOUT: usize = 4;
const MAGIC: &[u8; 8] = b"SGPEAKS1";
/// Levels stop when one block covers this many base blocks or more.
const MAX_LEVELS: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct Peaks {
    pub sample_rate: u32,
    pub channels: u16,
    /// Sample frames in the source.
    pub samples: u64,
    /// Per level: `[min, max]` per channel per block, as i16.
    levels: Vec<Vec<i16>>,
}

fn quantise(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

impl Peaks {
    /// Builds peaks from interleaved float samples delivered in pieces.
    pub fn build(
        sample_rate: u32,
        channels: u16,
        mut next: impl FnMut(&mut Vec<f32>) -> Result<bool, ToolError>,
    ) -> Result<Peaks, ToolError> {
        let ch = channels.max(1) as usize;
        let mut level0: Vec<i16> = Vec::new();
        // Running min/max of the block being filled, per channel.
        let mut acc = vec![(f32::MAX, f32::MIN); ch];
        let mut in_block = 0u64;
        let mut frames = 0u64;
        let mut buf = Vec::new();
        let flush = |acc: &mut Vec<(f32, f32)>, level0: &mut Vec<i16>| {
            for a in acc.iter_mut() {
                level0.extend([quantise(a.0), quantise(a.1)]);
                *a = (f32::MAX, f32::MIN);
            }
        };
        loop {
            buf.clear();
            if !next(&mut buf)? {
                break;
            }
            for frame in buf.chunks_exact(ch) {
                for (a, &s) in acc.iter_mut().zip(frame) {
                    a.0 = a.0.min(s);
                    a.1 = a.1.max(s);
                }
                in_block += 1;
                frames += 1;
                if in_block == BASE_BLOCK {
                    flush(&mut acc, &mut level0);
                    in_block = 0;
                }
            }
        }
        if in_block > 0 {
            flush(&mut acc, &mut level0);
        }
        let mut levels = vec![level0];
        while levels.len() < MAX_LEVELS && levels[levels.len() - 1].len() / (2 * ch) > FANOUT {
            let below = &levels[levels.len() - 1];
            let mut above = Vec::with_capacity(below.len() / FANOUT + 2 * ch);
            for group in below.chunks(2 * ch * FANOUT) {
                for c in 0..ch {
                    let blocks = group.chunks_exact(2 * ch);
                    let min = blocks.clone().map(|b| b[2 * c]).min().unwrap_or(0);
                    let max = blocks.map(|b| b[2 * c + 1]).max().unwrap_or(0);
                    above.extend([min, max]);
                }
            }
            levels.push(above);
        }
        Ok(Peaks { sample_rate, channels: ch as u16, samples: frames, levels })
    }

    pub fn levels(&self) -> usize {
        self.levels.len()
    }

    /// Sample frames covered by one block of `level`.
    pub fn block_len(&self, level: usize) -> u64 {
        BASE_BLOCK * (FANOUT as u64).pow(level as u32)
    }

    pub fn blocks(&self, level: usize) -> usize {
        self.levels.get(level).map_or(0, |l| l.len() / (2 * self.channels as usize))
    }

    /// `(min, max)` of one block in -1..=1.
    pub fn get(&self, level: usize, channel: usize, block: usize) -> Option<(f32, f32)> {
        let ch = self.channels as usize;
        let data = self.levels.get(level)?;
        let i = (block * ch + channel.min(ch - 1)) * 2;
        let (min, max) = (*data.get(i)?, *data.get(i + 1)?);
        Some((min as f32 / i16::MAX as f32, max as f32 / i16::MAX as f32))
    }

    /// The coarsest level whose blocks are not longer than `frames_per_pixel`.
    pub fn level_for(&self, frames_per_pixel: f64) -> usize {
        (0..self.levels.len()).rev().find(|&l| self.block_len(l) as f64 <= frames_per_pixel).unwrap_or(0)
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let mut out = BufWriter::new(std::fs::File::create(path)?);
        out.write_all(MAGIC)?;
        out.write_all(&self.sample_rate.to_le_bytes())?;
        out.write_all(&(self.channels as u32).to_le_bytes())?;
        out.write_all(&self.samples.to_le_bytes())?;
        out.write_all(&(self.levels.len() as u32).to_le_bytes())?;
        for level in &self.levels {
            out.write_all(&(level.len() as u64).to_le_bytes())?;
        }
        for level in &self.levels {
            for v in level {
                out.write_all(&v.to_le_bytes())?;
            }
        }
        out.flush()
    }

    pub fn read(path: &Path) -> std::io::Result<Peaks> {
        let bytes = std::fs::read(path)?;
        let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not a peak file");
        let mut at = 0usize;
        let mut take = |n: usize| -> std::io::Result<&[u8]> {
            let slice = bytes.get(at..at + n).ok_or_else(bad)?;
            at += n;
            Ok(slice)
        };
        if take(8)? != MAGIC {
            return Err(bad());
        }
        let u32_of = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let u64_of = |b: &[u8]| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
        let sample_rate = u32_of(take(4)?);
        let channels = u32_of(take(4)?) as u16;
        let samples = u64_of(take(8)?);
        let count = u32_of(take(4)?) as usize;
        if channels == 0 || count > MAX_LEVELS {
            return Err(bad());
        }
        let mut lens = Vec::with_capacity(count);
        for _ in 0..count {
            lens.push(u64_of(take(8)?) as usize);
        }
        let mut levels = Vec::with_capacity(count);
        for len in lens {
            let raw = take(len.checked_mul(2).ok_or_else(bad)?)?;
            levels.push(raw.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect());
        }
        Ok(Peaks { sample_rate, channels, samples, levels })
    }
}

/// Opens a 32-bit float WAV as written by [`crate::derive::audio_wav`] and
/// leaves the reader at the first sample. Returns it with the sample rate
/// and the channel count.
pub fn open_wav(path: &Path) -> Result<(BufReader<std::fs::File>, u32, u16), ToolError> {
    let mut file = BufReader::with_capacity(1 << 20, std::fs::File::open(path)?);
    let mut head = [0u8; 12];
    file.read_exact(&mut head)?;
    if &head[8..12] != b"WAVE" {
        return Err(ToolError::other("conformed audio is not a WAV file"));
    }
    let (mut sample_rate, mut channels) = (48_000u32, 2u16);
    // Walk the chunks up to the sample data.
    loop {
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        match &chunk[0..4] {
            b"data" => break,
            b"fmt " => {
                let mut fmt = vec![0u8; size as usize];
                file.read_exact(&mut fmt)?;
                if fmt.len() < 16 || u16::from_le_bytes([fmt[14], fmt[15]]) != 32 {
                    return Err(ToolError::other("conformed audio is not 32-bit float"));
                }
                channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                if size % 2 == 1 {
                    file.read_exact(&mut [0u8; 1])?;
                }
            }
            _ => {
                std::io::copy(&mut (&mut file).take(size + size % 2), &mut std::io::sink())?;
            }
        }
    }
    Ok((file, sample_rate, channels))
}

/// The whole file as one channel (the mean of its channels), for analysis.
pub fn read_mono(path: &Path, cancel: &Cancel) -> Result<(Vec<f32>, u32), ToolError> {
    let (mut file, sample_rate, channels) = open_wav(path)?;
    let channels = channels.max(1) as usize;
    let mut raw = vec![0u8; (1 << 16) * 4 * channels];
    let mut mono = Vec::new();
    loop {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let mut filled = 0;
        while filled < raw.len() {
            match file.read(&mut raw[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        if filled == 0 {
            return Ok((mono, sample_rate));
        }
        let samples = raw[..filled].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b));
        let frames: Vec<f32> = samples.collect();
        mono.extend(frames.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32));
    }
}

/// Builds peaks from a 32-bit float WAV as written by [`crate::derive::audio_wav`].
pub fn from_wav(path: &Path, cancel: &Cancel) -> Result<Peaks, ToolError> {
    let (mut file, sample_rate, channels) = open_wav(path)?;
    let mut raw = vec![0u8; (1 << 16) * 4 * channels.max(1) as usize];
    // The data size in the header is not trusted (RF64, streamed files): read to the end.
    Peaks::build(sample_rate, channels, |out| {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let mut filled = 0;
        while filled < raw.len() {
            match file.read(&mut raw[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        out.extend(raw[..filled].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        Ok(filled > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(samples: &[f32], channels: u16) -> Peaks {
        let mut once = Some(samples.to_vec());
        Peaks::build(48_000, channels, |out| Ok(once.take().map(|s| out.extend(s)).is_some())).unwrap()
    }

    #[test]
    fn level_zero_is_min_max_per_block() {
        // Mono ramp: block 0 is 0..255, block 1 holds one loud sample.
        let mut s: Vec<f32> = (0..256).map(|i| i as f32 / 1000.0).collect();
        s.extend([0.0; 100]);
        s[300] = -0.9;
        let p = build(&s, 1);
        assert_eq!((p.samples, p.blocks(0)), (356, 2));
        let (min, max) = p.get(0, 0, 0).unwrap();
        assert!(min.abs() < 1e-4 && (max - 0.255).abs() < 1e-3);
        let (min, max) = p.get(0, 0, 1).unwrap();
        assert!((min + 0.9).abs() < 1e-3 && max.abs() < 1e-4);
        assert_eq!(p.get(0, 0, 2), None);
    }

    #[test]
    fn coarser_levels_cover_the_finer_ones() {
        let n = 256 * 64;
        let s: Vec<f32> = (0..n * 2).map(|i| if i % 2 == 0 { (i as f32 * 0.001).sin() } else { 0.25 }).collect();
        let p = build(&s, 2);
        assert!(p.levels() >= 3);
        assert_eq!((p.blocks(0), p.blocks(1), p.blocks(2)), (64, 16, 4));
        for block in 0..p.blocks(1) {
            let fine: Vec<(f32, f32)> = (0..4).map(|k| p.get(0, 0, block * 4 + k).unwrap()).collect();
            let (min, max) = p.get(1, 0, block).unwrap();
            assert_eq!(min, fine.iter().map(|f| f.0).fold(f32::MAX, f32::min));
            assert_eq!(max, fine.iter().map(|f| f.1).fold(f32::MIN, f32::max));
        }
        // The right channel is constant.
        let (min, max) = p.get(2, 1, 0).unwrap();
        assert!((min - 0.25).abs() < 1e-3 && (max - 0.25).abs() < 1e-3);
        assert_eq!(p.level_for(100.0), 0);
        assert_eq!(p.level_for(1024.0), 1);
        assert_eq!(p.level_for(1e9), p.levels() - 1);
    }

    #[test]
    fn file_round_trip() {
        let s: Vec<f32> = (0..5000).map(|i| ((i * 37) % 200) as f32 / 100.0 - 1.0).collect();
        let p = build(&s, 2);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pk");
        p.write(&path).unwrap();
        assert_eq!(Peaks::read(&path).unwrap(), p);
        std::fs::write(&path, b"nonsense").unwrap();
        assert!(Peaks::read(&path).is_err());
    }
}
