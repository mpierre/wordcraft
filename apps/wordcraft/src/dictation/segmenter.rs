//! Cuts the microphone's sound into phrases: speech between pauses. Each phrase is transcribed
//! on its own, so text appears a moment after the speaker pauses rather than at Stop.
//!
//! Speech is told from silence by loudness against the room's noise: the quietest moment of the
//! last few seconds (speech has gaps between words; a fan or a hum never goes quieter than
//! itself). A fan raises the bar, a quiet room lowers it, and speech from the very first moment
//! counts, since the bar starts low. A phrase starts after a
//! few loud frames (with a little sound from before them, so the first consonant isn't cut) and
//! ends after a pause, or when it gets too long to wait for.

use std::collections::VecDeque;

/// The model's sample rate.
pub const RATE: usize = 16_000;
/// Samples per analysis frame (30 ms).
const FRAME: usize = RATE * 30 / 1000;
/// Loud frames in a row that start a phrase.
const START_FRAMES: usize = 3;
/// Frames of sound kept from before a phrase starts (300 ms).
const PREROLL_FRAMES: usize = 10;
/// A pause this long ends a phrase (750 ms).
const PAUSE_FRAMES: usize = 25;
/// Phrases with less speech than this are coughs and clicks, and are dropped (240 ms).
const MIN_VOICED_FRAMES: usize = 8;
/// The longest phrase (25 s; the model hears at most 30 s at once).
const MAX_SAMPLES: usize = RATE * 25;
/// The quietest sound counted as speech (RMS of full scale), however quiet the room.
const MIN_THRESHOLD: f32 = 0.004;
/// Speech is this many times louder than the room's noise.
const NOISE_FACTOR: f32 = 3.0;
/// Frames over which the room's noise is the quietest one (3 s).
const NOISE_FRAMES: usize = 100;

#[derive(Default)]
pub struct Segmenter {
    /// Samples not yet making up a whole frame.
    partial: Vec<f32>,
    /// The levels (RMS) of the last [`NOISE_FRAMES`] frames.
    levels: VecDeque<f32>,
    preroll: VecDeque<Vec<f32>>,
    /// Loud frames in a row before a phrase.
    loud_run: usize,
    /// The phrase being heard.
    phrase: Option<Vec<f32>>,
    voiced: usize,
    quiet_run: usize,
}

/// Root mean square of `s` (0 for none).
pub fn rms(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

impl Segmenter {
    /// Feed 16 kHz mono samples; returns the phrases they finished.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        let mut done = Vec::new();
        self.partial.extend_from_slice(samples);
        while self.partial.len() >= FRAME {
            let frame: Vec<f32> = self.partial.drain(..FRAME).collect();
            if let Some(p) = self.frame(frame) {
                done.push(p);
            }
        }
        done
    }

    /// The phrase being heard, if it has enough speech (at Stop).
    pub fn finish(&mut self) -> Option<Vec<f32>> {
        let mut phrase = self.phrase.take()?;
        phrase.append(&mut self.partial);
        let voiced = std::mem::take(&mut self.voiced);
        self.quiet_run = 0;
        (voiced >= MIN_VOICED_FRAMES).then_some(phrase)
    }

    fn frame(&mut self, frame: Vec<f32>) -> Option<Vec<f32>> {
        let level = rms(&frame);
        let noise = self.levels.iter().copied().fold(f32::INFINITY, f32::min);
        let loud = level > (if noise.is_finite() { noise * NOISE_FACTOR } else { 0.0 }).max(MIN_THRESHOLD);
        self.levels.push_back(level);
        if self.levels.len() > NOISE_FRAMES {
            self.levels.pop_front();
        }
        match &mut self.phrase {
            None => {
                self.preroll.push_back(frame);
                if self.preroll.len() > PREROLL_FRAMES {
                    self.preroll.pop_front();
                }
                self.loud_run = if loud { self.loud_run + 1 } else { 0 };
                if self.loud_run >= START_FRAMES {
                    self.phrase = Some(self.preroll.drain(..).flatten().collect());
                    self.voiced = self.loud_run;
                    self.quiet_run = 0;
                    self.loud_run = 0;
                }
                None
            }
            Some(phrase) => {
                phrase.extend_from_slice(&frame);
                if loud {
                    self.voiced += 1;
                    self.quiet_run = 0;
                } else {
                    self.quiet_run += 1;
                }
                if self.quiet_run >= PAUSE_FRAMES || phrase.len() >= MAX_SAMPLES {
                    let phrase = self.phrase.take()?;
                    let voiced = std::mem::take(&mut self.voiced);
                    self.quiet_run = 0;
                    return (voiced >= MIN_VOICED_FRAMES).then_some(phrase);
                }
                None
            }
        }
    }
}

/// Turns sound at the microphone's rate into the model's 16 kHz, by linear interpolation
/// (speech has nothing the model listens to above 8 kHz, so no sharper filter is needed).
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Where the next output sample falls, from the start of `last`.
    at: f64,
    /// The previous chunk's last sample.
    last: f32,
}

impl Resampler {
    pub fn new(from_rate: u32) -> Resampler {
        Resampler { step: f64::from(from_rate.clamp(1_000, 384_000)) / RATE as f64, at: 1.0, last: 0.0 }
    }

    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        if (self.step - 1.0).abs() < f64::EPSILON {
            return input.to_vec();
        }
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 1);
        // Position 0 is `last`, 1… are `input`.
        let sample = |i: usize| if i == 0 { Some(self.last) } else { input.get(i - 1).copied() };
        let end = input.len() as f64;
        while self.at <= end {
            let i = self.at.floor();
            let frac = (self.at - i) as f32;
            let i = i as usize;
            let (Some(a), b) = (sample(i), sample(i + 1)) else { break };
            out.push(a + (b.unwrap_or(a) - a) * frac);
            self.at += self.step;
        }
        self.at -= end;
        if let Some(l) = input.last() {
            self.last = *l;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| amp * (i as f32 * 0.07).sin()).collect()
    }

    /// Speech-like sound: a tone in syllables, four a second, with quiet between them.
    fn speech(secs: f32, amp: f32) -> Vec<f32> {
        let rate = RATE as f32;
        (0..(secs * rate) as usize).map(|i| amp * (i as f32 * 0.07).sin() * (i as f32 / rate * std::f32::consts::TAU * 4.0).sin().abs()).collect()
    }

    #[test]
    fn speech_between_pauses_is_one_phrase() {
        let mut s = Segmenter::default();
        let mut sound = tone(1.0, 0.001);
        sound.extend(speech(1.5, 0.3));
        sound.extend(tone(1.0, 0.001));
        let phrases = s.push(&sound);
        assert_eq!(phrases.len(), 1);
        let len = phrases[0].len() as f32 / RATE as f32;
        assert!((1.5..2.6).contains(&len), "{len} s");
        assert!(s.finish().is_none());
    }

    #[test]
    fn two_phrases_and_a_click() {
        let mut s = Segmenter::default();
        let mut sound = tone(0.5, 0.001);
        sound.extend(speech(1.0, 0.2));
        sound.extend(tone(1.0, 0.001));
        // A 90 ms click is no phrase.
        sound.extend(tone(0.09, 0.4));
        sound.extend(tone(1.0, 0.001));
        sound.extend(speech(0.8, 0.2));
        sound.extend(tone(1.0, 0.001));
        assert_eq!(s.push(&sound).len(), 2);
    }

    #[test]
    fn speech_from_the_first_moment_counts() {
        let mut s = Segmenter::default();
        let mut sound = speech(1.2, 0.2);
        sound.extend(tone(1.0, 0.001));
        assert_eq!(s.push(&sound).len(), 1);
    }

    #[test]
    fn a_steady_fan_is_not_speech() {
        let mut s = Segmenter::default();
        let mut sound = tone(4.0, 0.02);
        sound.extend(speech(1.0, 0.2));
        sound.extend(tone(1.0, 0.02));
        let phrases = s.push(&sound);
        // The fan's first seconds may pass for a phrase before the bar rises; then only the speech.
        assert!(phrases.len() <= 2, "{}", phrases.len());
        let last = phrases.last().unwrap().len() as f32 / RATE as f32;
        assert!(last < 2.5, "{last} s");
    }

    #[test]
    fn stop_keeps_the_phrase_being_spoken() {
        let mut s = Segmenter::default();
        let mut sound = tone(0.5, 0.001);
        sound.extend(speech(0.6, 0.2));
        assert!(s.push(&sound).is_empty());
        assert!(s.finish().is_some());
    }

    #[test]
    fn long_speech_is_cut() {
        let mut s = Segmenter::default();
        let mut sound = tone(0.3, 0.001);
        sound.extend(speech(30.0, 0.2));
        let phrases = s.push(&sound);
        assert_eq!(phrases.len(), 1);
        assert!(phrases[0].len() <= MAX_SAMPLES + FRAME);
    }

    #[test]
    fn resampling_keeps_duration() {
        for rate in [8_000u32, 16_000, 44_100, 48_000] {
            let mut r = Resampler::new(rate);
            let mut n = 0;
            // In uneven chunks, as audio callbacks deliver it.
            for chunk in [441usize, 1000, 37, 4410, 2] {
                n += r.push(&vec![0.5; chunk]).len();
            }
            let input = 441 + 1000 + 37 + 4410 + 2;
            let want = input as f64 * RATE as f64 / f64::from(rate);
            assert!((n as f64 - want).abs() <= 2.0, "{rate}: {n} vs {want}");
        }
        let mut r = Resampler::new(48_000);
        assert!(r.push(&[0.25; 4800]).iter().all(|x| (x - 0.25).abs() < 1e-6 || *x < 0.25));
        assert!(Resampler::new(0).push(&[1.0; 10]).len() < 1000);
    }
}
