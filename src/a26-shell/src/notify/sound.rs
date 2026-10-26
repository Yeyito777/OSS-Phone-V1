//! Synthesized alert tones and the vibrator (docs/notifications-protocol.md §7).
//!
//! Output is 48 kHz stereo signed 16-bit little-endian PCM for moon-audiod's
//! alert FIFO. Every tone is built from short notes with 5 ms attack/release
//! ramps, so the speaker never clicks.

use std::f32::consts::TAU;
use std::fs;
use std::io;

use super::model::Sound;

pub const RATE: u32 = 48_000;
const PEAK: f32 = 14_000.0;
const VIBRATOR: &str = "/sys/class/timed_output/vibrator/enable";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Looping alarm/timer sound.
    Ring(Sound),
    /// One-shot notification chime.
    Chime,
}

/// One note: start/length in ms within the pattern, frequency at start/end
/// (a sweep when they differ), and how fast it decays (0 = flat).
struct Note {
    at_ms: u32,
    len_ms: u32,
    from_hz: f32,
    to_hz: f32,
    decay: f32,
}

const fn note(at_ms: u32, len_ms: u32, from_hz: f32, to_hz: f32, decay: f32) -> Note {
    Note {
        at_ms,
        len_ms,
        from_hz,
        to_hz,
        decay,
    }
}

const RADAR: (&[Note], u32) = (
    &[
        note(0, 90, 880.0, 880.0, 0.0),
        note(150, 90, 880.0, 880.0, 0.0),
        note(300, 90, 880.0, 880.0, 0.0),
        note(450, 90, 880.0, 880.0, 0.0),
    ],
    1200,
);
const CHIMES: (&[Note], u32) = (
    &[
        note(0, 700, 1318.5, 1318.5, 5.0),
        note(350, 900, 1046.5, 1046.5, 4.0),
        note(700, 1000, 1568.0, 1568.0, 3.5),
    ],
    1800,
);
const BEACON: (&[Note], u32) = (&[note(0, 320, 620.0, 1240.0, 0.0)], 1000);
/// Incoming call: a rising marimba arpeggio, twice, then a pause.
const RINGTONE: (&[Note], u32) = (
    &[
        note(0, 260, 1046.5, 1046.5, 7.0),
        note(140, 260, 1318.5, 1318.5, 7.0),
        note(280, 260, 1568.0, 1568.0, 7.0),
        note(420, 420, 2093.0, 2093.0, 6.0),
        note(800, 260, 1046.5, 1046.5, 7.0),
        note(940, 260, 1318.5, 1318.5, 7.0),
        note(1080, 260, 1568.0, 1568.0, 7.0),
        note(1220, 420, 2093.0, 2093.0, 6.0),
    ],
    2600,
);
const CHIME: (&[Note], u32) = (
    &[
        note(0, 220, 1318.5, 1318.5, 9.0),
        note(130, 320, 1760.0, 1760.0, 7.0),
    ],
    470,
);

pub struct Tone {
    notes: &'static [Note],
    period: u64,
    looping: bool,
    frame: u64,
    gain: f32,
}

impl Tone {
    pub fn new(pattern: Pattern, volume: u8) -> Option<Self> {
        let ((notes, period_ms), looping) = match pattern {
            Pattern::Ring(Sound::Radar) => (RADAR, true),
            Pattern::Ring(Sound::Chimes) => (CHIMES, true),
            Pattern::Ring(Sound::Beacon) => (BEACON, true),
            Pattern::Ring(Sound::Ringtone) => (RINGTONE, true),
            Pattern::Ring(Sound::None) => return None,
            Pattern::Chime => (CHIME, false),
        };
        let v = f32::from(volume.min(100)) / 100.0;
        Some(Self {
            notes,
            period: u64::from(period_ms) * u64::from(RATE) / 1000,
            looping,
            frame: 0,
            // Square law: equal steps of the setting sound like equal steps.
            gain: v * v,
        })
    }

    pub fn finished(&self) -> bool {
        !self.looping && self.frame >= self.period
    }

    /// Appends up to `frames` stereo frames; fewer only when a one-shot ends.
    pub fn render(&mut self, frames: usize, out: &mut Vec<u8>) {
        for _ in 0..frames {
            if self.finished() {
                break;
            }
            let position = self.frame % self.period;
            let mut value = 0.0f32;
            for note in self.notes {
                value += sample(note, position);
            }
            let sample = (value * PEAK * self.gain).clamp(-32767.0, 32767.0) as i16;
            let bytes = sample.to_le_bytes();
            out.extend_from_slice(&bytes);
            out.extend_from_slice(&bytes);
            self.frame += 1;
        }
    }
}

fn sample(note: &Note, position: u64) -> f32 {
    let start = u64::from(note.at_ms) * u64::from(RATE) / 1000;
    let length = u64::from(note.len_ms) * u64::from(RATE) / 1000;
    if position < start || position >= start + length {
        return 0.0;
    }
    let i = (position - start) as f32;
    let t = i / RATE as f32;
    let len = length as f32;
    let ramp = (RATE as f32) * 0.005;
    let envelope = (i / ramp).min(1.0) * ((len - i) / ramp).min(1.0) * (-note.decay * t).exp();
    // Phase of a linear sweep from_hz -> to_hz.
    let sweep = (note.to_hz - note.from_hz) / (len / RATE as f32);
    let phase = TAU * (note.from_hz * t + 0.5 * sweep * t * t);
    phase.sin() * envelope * 0.6
}

/// Starts the vibrator for `ms` (the driver stops it by itself; ≤ 10 s).
pub fn vibrate(ms: u32) -> io::Result<()> {
    fs::write(VIBRATOR, format!("{}\n", ms.min(10_000)))
}

pub fn stop_vibration() {
    let _ = fs::write(VIBRATOR, "0\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_are_bounded_and_click_free() {
        for pattern in [
            Pattern::Ring(Sound::Radar),
            Pattern::Ring(Sound::Chimes),
            Pattern::Ring(Sound::Beacon),
            Pattern::Ring(Sound::Ringtone),
            Pattern::Chime,
        ] {
            let mut tone = Tone::new(pattern, 100).unwrap();
            let mut out = Vec::new();
            tone.render(RATE as usize * 2, &mut out);
            let samples: Vec<i16> = out
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            assert!(
                samples.iter().any(|s| s.unsigned_abs() > 4000),
                "{pattern:?} audible"
            );
            // No jump larger than a 1.8 kHz full-scale sine allows (no clicks).
            let max_step = samples
                .chunks_exact(2)
                .map(|f| f[0])
                .collect::<Vec<_>>()
                .windows(2)
                .map(|w| (i32::from(w[1]) - i32::from(w[0])).abs())
                .max()
                .unwrap();
            assert!(max_step < 4000, "{pattern:?} step {max_step}");
        }
        let mut chime = Tone::new(Pattern::Chime, 50).unwrap();
        let mut out = Vec::new();
        chime.render(RATE as usize, &mut out);
        assert!(chime.finished());
        assert_eq!(out.len(), 470 * 48 * 4);
        assert!(Tone::new(Pattern::Ring(Sound::None), 100).is_none());
        let mut quiet = Tone::new(Pattern::Ring(Sound::Radar), 0).unwrap();
        out.clear();
        quiet.render(4800, &mut out);
        assert!(out.iter().all(|b| *b == 0));
    }
}
