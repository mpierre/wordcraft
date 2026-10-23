//! The microphone, as chunks of mono samples at a known rate.
//!
//! - **macOS and Windows:** the default input device through cpal (Core Audio, WASAPI), whose
//!   safe API needs no system libraries at build time.
//! - **Linux:** the desktop's own recorder streaming raw 16 kHz mono PCM to us: `pw-record`
//!   (PipeWire), else `parecord` (PulseAudio), else `arecord` (ALSA). This is how Read Aloud
//!   reaches the speakers there too, and it keeps the build free of audio development headers.
//! - **Tests:** `WORDCRAFT_DICTATION_WAV` names a 16-bit PCM WAV file that is played in as if
//!   spoken into the microphone in real time, followed by silence, so dictation can be checked
//!   end to end without a microphone or a person.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Sound from the microphone: mono samples (−1…1) at `rate`.
pub struct Capture {
    pub rate: u32,
    pub chunks: Receiver<Vec<f32>>,
    /// Keeps the source running; dropping it stops the microphone.
    _source: Source,
}

#[allow(dead_code)]
enum Source {
    #[cfg(any(target_os = "macos", windows))]
    Cpal(cpal::Stream),
    Process(ChildGuard),
    File,
}

/// A recorder process, killed when dictation stops.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Open the microphone (or the test file).
pub fn open() -> Result<Capture, String> {
    if let Some(p) = std::env::var_os("WORDCRAFT_DICTATION_WAV").filter(|p| !p.is_empty()) {
        return wav_file(std::path::Path::new(&p));
    }
    microphone()
}

#[cfg(any(target_os = "macos", windows))]
fn microphone() -> Result<Capture, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleFormat, SizedSample};

    fn stream<T>(device: &cpal::Device, config: cpal::StreamConfig, tx: Sender<Vec<f32>>) -> Result<cpal::Stream, cpal::Error>
    where
        T: SizedSample,
        f32: cpal::FromSample<T>,
    {
        let channels = usize::from(config.channels.max(1));
        device.build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                let mono: Vec<f32> =
                    data.chunks(channels).map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / frame.len().max(1) as f32).collect();
                let _ = tx.send(mono);
            },
            |e| log::warn!("dictation: microphone: {e}"),
            None,
        )
    }

    let host = cpal::default_host();
    let device = host.default_input_device().ok_or("there's no microphone")?;
    let supported = device.default_input_config().map_err(|e| format!("the microphone can't be opened: {e}"))?;
    let rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let (tx, rx) = channel();
    let s = match format {
        SampleFormat::F32 => stream::<f32>(&device, config, tx),
        SampleFormat::I16 => stream::<i16>(&device, config, tx),
        SampleFormat::U16 => stream::<u16>(&device, config, tx),
        SampleFormat::I32 => stream::<i32>(&device, config, tx),
        SampleFormat::I8 => stream::<i8>(&device, config, tx),
        SampleFormat::U8 => stream::<u8>(&device, config, tx),
        SampleFormat::F64 => stream::<f64>(&device, config, tx),
        other => return Err(format!("the microphone sends {other} samples, which dictation can't read")),
    }
    .map_err(|e| format!("the microphone can't be opened: {e}"))?;
    s.play().map_err(|e| format!("the microphone didn't start: {e}"))?;
    Ok(Capture { rate, chunks: rx, _source: Source::Cpal(s) })
}

#[cfg(target_os = "linux")]
fn microphone() -> Result<Capture, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    const RECORDERS: &[(&str, &[&str])] = &[
        ("pw-record", &["--rate", "16000", "--channels", "1", "--format", "s16", "-"]),
        ("parecord", &["--raw", "--format=s16le", "--rate=16000", "--channels=1"]),
        ("arecord", &["-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"]),
    ];
    let mut tried = Vec::new();
    for (program, args) in RECORDERS {
        let Ok(mut child) = Command::new(program).args(*args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
            tried.push(*program);
            continue;
        };
        let Some(mut out) = child.stdout.take() else {
            let _ = child.kill();
            continue;
        };
        // A recorder that can't reach the sound server exits at once.
        std::thread::sleep(Duration::from_millis(150));
        if let Ok(Some(_)) = child.try_wait() {
            tried.push(*program);
            continue;
        }
        let (tx, rx) = channel();
        let spawned = std::thread::Builder::new().name("dictation-mic".into()).spawn(move || {
            let mut buf = [0u8; 3200];
            let mut odd: Option<u8> = None;
            loop {
                let n = match out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let mut bytes: Vec<u8> = odd.take().into_iter().collect();
                bytes.extend_from_slice(buf.get(..n).unwrap_or(&[]));
                if bytes.len() % 2 == 1 {
                    odd = bytes.pop();
                }
                let samples = bytes.chunks_exact(2).map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0).collect();
                if tx.send(samples).is_err() {
                    break;
                }
            }
        });
        if spawned.is_err() {
            let _ = child.kill();
            return Err("couldn't start reading the microphone".into());
        }
        log::info!("dictation: recording with {program}");
        return Ok(Capture { rate: 16_000, chunks: rx, _source: Source::Process(ChildGuard(child)) });
    }
    Err(format!("no sound recorder worked ({}); install PipeWire's pw-record, PulseAudio's parecord or ALSA's arecord", tried.join(", ")))
}

/// 16-bit PCM WAV: (rate, mono samples).
pub fn read_wav(bytes: &[u8]) -> Result<(u32, Vec<f32>), String> {
    let bad = || "not a 16-bit PCM WAV file".to_string();
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err(bad());
    }
    let u16_at = |i: usize| bytes.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |i: usize| bytes.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut at = 12usize;
    let mut fmt: Option<(u16, u32, u16)> = None;
    while let (Some(id), Some(len)) = (bytes.get(at..at + 4), u32_at(at + 4)) {
        let body = at + 8;
        let len = len as usize;
        if id == b"fmt " {
            let (Some(kind), Some(channels), Some(rate), Some(bits)) = (u16_at(body), u16_at(body + 2), u32_at(body + 4), u16_at(body + 14)) else {
                return Err(bad());
            };
            if kind != 1 || bits != 16 || channels == 0 {
                return Err(bad());
            }
            fmt = Some((channels, rate, bits));
        } else if id == b"data" {
            let (channels, rate, _) = fmt.ok_or_else(bad)?;
            let data = bytes.get(body..body.saturating_add(len).min(bytes.len())).unwrap_or(&[]);
            let ch = usize::from(channels);
            let samples = data
                .chunks_exact(2 * ch)
                .map(|frame| frame.chunks_exact(2).map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0).sum::<f32>() / ch as f32)
                .collect();
            return Ok((rate, samples));
        }
        at = body.saturating_add(len).saturating_add(len & 1);
    }
    Err(bad())
}

fn wav_file(path: &std::path::Path) -> Result<Capture, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    let (rate, samples) = read_wav(&bytes)?;
    let (tx, rx) = channel::<Vec<f32>>();
    let chunk = (rate as usize / 10).max(1);
    std::thread::Builder::new()
        .name("dictation-wav".into())
        .spawn(move || {
            for c in samples.chunks(chunk) {
                if tx.send(c.to_vec()).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // Silence afterwards, as from a quiet room.
            while tx.send(vec![0.0005; chunk]).is_ok() {
                std::thread::sleep(Duration::from_millis(100));
            }
        })
        .map_err(|e| format!("couldn't play {}: {e}", path.display()))?;
    Ok(Capture { rate, chunks: rx, _source: Source::File })
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_files_are_read_and_junk_refused() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(2u16.to_le_bytes()); // stereo
        wav.extend(22_050u32.to_le_bytes());
        wav.extend(88_200u32.to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(8u32.to_le_bytes());
        for s in [16384i16, 0, -16384, -16384] {
            wav.extend(s.to_le_bytes());
        }
        let (rate, samples) = super::read_wav(&wav).unwrap();
        assert_eq!(rate, 22_050);
        assert_eq!(samples, vec![0.25, -0.5]);
        assert!(super::read_wav(b"RIFF").is_err());
        assert!(super::read_wav(&wav[..30]).is_err());
        let mut huge = wav.clone();
        huge.splice(40..44, u32::MAX.to_le_bytes());
        assert_eq!(super::read_wav(&huge).unwrap().1.len(), 2);
    }
}
