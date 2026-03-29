//! Dead-input guard: catch a capture device that delivers no audio.
//!
//! A microphone that is powered off, muted in hardware, or asleep still
//! opens, still streams, and still fills a recording. The samples carry no
//! sound: either digital silence or a constant DC bias with nothing on top
//! of it. Transcription of such a recording returns an empty string, so the
//! dictation is lost without any error the user can see.
//!
//! [`DeadInputDetector`] watches the first moments of a capture and answers
//! one question: did *any* sound reach the daemon? It high-passes the sample
//! stream (which removes both DC offset and sub-audible drift), measures the
//! RMS of each 100 ms frame, and keeps the loudest frame. A single frame
//! above the threshold settles the question as [`Verdict::Live`] and the
//! detector stops working. If the whole observation window passes without
//! one, the verdict is [`Verdict::Dead`].
//!
//! ## Why the peak frame, and not the mean
//!
//! A live capture can start with silence: PipeWire can hand over a stretch
//! of zeros while the device starts up (runs of up to 1.09 s appear in a
//! corpus of 22,929 recordings), and the user needs a moment to start
//! speaking. Taking the loudest frame of the window means that leading
//! silence only delays the answer, it never produces a wrong one.
//!
//! ## Where the threshold comes from
//!
//! Measured over 22,889 saved dictations (2026-03 to 2026-11, 16 kHz mono,
//! captured from the same sources the daemon records from), with this exact
//! statistic (150 Hz high-pass, 100 ms frames, peak frame RMS over 1.5 s
//! after a 100 ms settle):
//!
//! | population | n | peak frame RMS |
//! |---|---|---|
//! | dead: digital silence | 29 | 0 (-inf dBFS), all 29 |
//! | live: quietest clip | 1 | -88.3 dBFS |
//! | live: next four quietest | 4 | -82.8 to -79.8 dBFS |
//! | live: median | 22,811 | -29.9 dBFS |
//!
//! The default threshold of -100 dBFS sits 11.7 dB below the quietest live
//! recording in that corpus, and the dead population carries no energy at
//! all, so neither direction is close to the line. One live recording of
//! the 22,811 is flagged: its first 1.6 s are bit-exact silence before the
//! capture starts delivering sound.
//!
//! What this does not catch: a capture that delivers a DC offset plus slow
//! sub-audible drift and no sound (49 clips of the same corpus, all but one
//! from 2026-03 to 2026-05) peaks at -66.7 dBFS and higher through this
//! high-pass, which is inside the range live recordings occupy. No
//! threshold separates that population, so the guard leaves it alone rather
//! than throw away real dictations.

use std::f32::consts::PI;

use crate::config::DeadInputConfig;

/// High-pass corner frequency. Above every DC offset and the slow drift a
/// sleeping capture chip produces; below the energy of room noise and
/// speech.
const CUTOFF_HZ: f32 = 150.0;

/// Analysis frame length. Short enough that one word, or one knock on the
/// desk, lands inside a single frame.
const FRAME_MS: u32 = 100;

/// Samples discarded at the start of a capture, before measurement begins.
/// Covers the device's own start-up click and settles the filter.
const SETTLE_MS: u32 = 100;

/// Shortest observation window that still holds a useful number of frames.
const MIN_WINDOW_MS: u32 = 300;

/// What the detector knows so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not enough audio observed yet.
    Pending,
    /// Sound reached the daemon; the guard has nothing left to do.
    Live,
    /// The observation window closed without any sound at all.
    Dead,
}

/// Direct-form-I biquad, high-pass, primed so that a constant input
/// produces no step at the start of the stream.
struct HighPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    primed: bool,
}

impl HighPass {
    /// Second-order Butterworth (Q = 1/sqrt(2)) high-pass at `cutoff_hz`.
    fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let w0 = 2.0 * PI * cutoff_hz / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        // Q = 1/sqrt(2) gives the Butterworth (maximally flat) response.
        let alpha = sin_w0 / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cos_w0) / 2.0) / a0,
            b1: (-(1.0 + cos_w0)) / a0,
            b2: ((1.0 + cos_w0) / 2.0) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
            primed: false,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        if !self.primed {
            // Start from the steady state of a constant input equal to the
            // first sample. Without this, a stream that opens at a DC offset
            // rings through the filter and the opening frames read as sound.
            self.x1 = x;
            self.x2 = x;
            self.primed = true;
        }
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Watches the opening of a capture for any sound at all.
///
/// Feed every captured chunk to [`push`](DeadInputDetector::push) in order.
/// The detector is cheap: one biquad and one accumulator per sample, and it
/// stops processing as soon as it has an answer.
pub struct DeadInputDetector {
    filter: HighPass,
    threshold: f32,
    frame_len: usize,
    settle_left: usize,
    window_left: usize,
    frame_energy: f64,
    frame_filled: usize,
    peak_rms: f32,
    verdict: Verdict,
}

impl DeadInputDetector {
    /// Build a detector for a capture running at `sample_rate`.
    pub fn new(sample_rate: u32, config: &DeadInputConfig) -> Self {
        let sample_rate = sample_rate.max(1);
        let samples_per_ms = sample_rate as f32 / 1000.0;
        let frame_len = ((FRAME_MS as f32 * samples_per_ms) as usize).max(1);
        let window_ms = config.window_ms.max(MIN_WINDOW_MS);
        Self {
            filter: HighPass::new(CUTOFF_HZ, sample_rate as f32),
            threshold: dbfs_to_linear(config.threshold_dbfs),
            frame_len,
            settle_left: (SETTLE_MS as f32 * samples_per_ms) as usize,
            window_left: ((window_ms as f32 * samples_per_ms) as usize).max(frame_len),
            frame_energy: 0.0,
            frame_filled: 0,
            peak_rms: 0.0,
            verdict: Verdict::Pending,
        }
    }

    /// Feed one captured chunk. Returns the verdict as it stands after the
    /// chunk; once it is [`Verdict::Live`] or [`Verdict::Dead`] it never
    /// changes again and further calls do no work.
    pub fn push(&mut self, chunk: &[f32]) -> Verdict {
        if self.verdict != Verdict::Pending {
            return self.verdict;
        }
        for &sample in chunk {
            let filtered = self.filter.process(sample);
            if self.settle_left > 0 {
                self.settle_left -= 1;
                continue;
            }
            self.frame_energy += (filtered as f64) * (filtered as f64);
            self.frame_filled += 1;
            self.window_left -= 1;

            if self.frame_filled == self.frame_len {
                let rms = (self.frame_energy / self.frame_filled as f64).sqrt() as f32;
                self.peak_rms = self.peak_rms.max(rms);
                self.frame_energy = 0.0;
                self.frame_filled = 0;
                if rms >= self.threshold {
                    self.verdict = Verdict::Live;
                    return self.verdict;
                }
            }
            if self.window_left == 0 {
                self.verdict = Verdict::Dead;
                return self.verdict;
            }
        }
        self.verdict
    }

    /// Loudest frame seen so far, in dBFS. `f32::NEG_INFINITY` for a stream
    /// that carried no energy at all.
    pub fn peak_dbfs(&self) -> f32 {
        linear_to_dbfs(self.peak_rms)
    }
}

/// What the daemon is told when a capture turns out to be dead.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeadInputReport {
    /// Loudest 100 ms frame observed, in dBFS (`NEG_INFINITY` for digital
    /// silence).
    pub peak_dbfs: f32,
    /// Level the input had to reach to count as alive, in dBFS.
    pub threshold_dbfs: f32,
    /// How long the guard listened, in milliseconds.
    pub window_ms: u32,
}

/// A [`DeadInputDetector`] plus the channel that carries its one message.
///
/// Lives in the chunk pump next to the capture, so it sees exactly the
/// samples the transcription backend sees.
pub struct DeadInputGuard {
    detector: DeadInputDetector,
    tx: tokio::sync::mpsc::Sender<DeadInputReport>,
    threshold_dbfs: f32,
    window_ms: u32,
}

impl DeadInputGuard {
    /// Build a guard for a capture at `sample_rate`, plus the receiver the
    /// daemon selects on. The channel holds one message: the guard reports
    /// at most once and is dropped afterwards.
    pub fn new(
        sample_rate: u32,
        config: &DeadInputConfig,
    ) -> (Self, tokio::sync::mpsc::Receiver<DeadInputReport>) {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        (
            Self {
                detector: DeadInputDetector::new(sample_rate, config),
                tx,
                threshold_dbfs: config.threshold_dbfs,
                window_ms: config.window_ms,
            },
            rx,
        )
    }

    /// Observe one captured chunk. Returns `true` once the guard is done
    /// with its job (either sound arrived, or the dead-input report has
    /// been sent), so the caller can drop it.
    pub fn observe(&mut self, chunk: &[f32]) -> bool {
        match self.detector.push(chunk) {
            Verdict::Pending => false,
            Verdict::Live => true,
            Verdict::Dead => {
                let report = DeadInputReport {
                    peak_dbfs: self.detector.peak_dbfs(),
                    threshold_dbfs: self.threshold_dbfs,
                    window_ms: self.window_ms,
                };
                if let Err(e) = self.tx.try_send(report) {
                    tracing::debug!("dead-input report dropped: {}", e);
                }
                true
            }
        }
    }
}

/// Convert a dBFS level to a linear RMS amplitude.
pub fn dbfs_to_linear(dbfs: f32) -> f32 {
    10.0_f32.powf(dbfs / 20.0)
}

/// Convert a linear RMS amplitude to dBFS.
pub fn linear_to_dbfs(linear: f32) -> f32 {
    if linear <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * linear.log10()
    }
}

/// Name of the audio source a dead-input alert should point at.
///
/// With an explicit `[audio] device` the configured name is the answer. With
/// `"default"` the daemon's own device name is also `"default"` (that is the
/// name the ALSA host reports for the default PCM), which tells the user
/// nothing, so ask PipeWire/PulseAudio which source that currently is.
pub async fn source_label(configured_device: &str) -> String {
    if configured_device != "default" {
        return configured_device.to_string();
    }
    match tokio::process::Command::new("pactl")
        .arg("get-default-source")
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if name.is_empty() {
                "default".to_string()
            } else {
                name
            }
        }
        Ok(_) | Err(_) => "default".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn config() -> DeadInputConfig {
        DeadInputConfig::default()
    }

    /// Feed a whole buffer in one call.
    fn verdict_of(samples: &[f32], config: &DeadInputConfig) -> Verdict {
        let mut det = DeadInputDetector::new(SR, config);
        det.push(samples)
    }

    fn seconds(secs: f32) -> usize {
        (SR as f32 * secs) as usize
    }

    /// Deterministic pseudo-random noise at a given RMS amplitude.
    fn noise(len: usize, rms: f32) -> Vec<f32> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let unit = (state >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0;
                unit * rms * 1.732
            })
            .collect()
    }

    fn tone(len: usize, freq: f32, amplitude: f32, offset: f32) -> Vec<f32> {
        (0..len)
            .map(|i| offset + amplitude * (2.0 * PI * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    #[test]
    fn digital_silence_is_dead() {
        let samples = vec![0.0f32; seconds(2.0)];
        assert_eq!(verdict_of(&samples, &config()), Verdict::Dead);
    }

    #[test]
    fn constant_dc_offset_is_dead() {
        // The signature of a capture chip that is powered but not listening:
        // a hard DC bias, no sound on top of it.
        let samples = vec![-0.19f32; seconds(2.0)];
        assert_eq!(verdict_of(&samples, &config()), Verdict::Dead);
    }

    #[test]
    fn room_noise_is_live() {
        // -60 dBFS: far below speech, still 30 dB above the threshold.
        let samples = noise(seconds(2.0), 0.001);
        assert_eq!(verdict_of(&samples, &config()), Verdict::Live);
    }

    #[test]
    fn room_noise_on_a_dc_offset_is_live() {
        let mut samples = noise(seconds(2.0), 0.001);
        for s in samples.iter_mut() {
            *s -= 0.19;
        }
        assert_eq!(verdict_of(&samples, &config()), Verdict::Live);
    }

    #[test]
    fn speech_band_tone_is_live() {
        let samples = tone(seconds(2.0), 440.0, 0.01, -0.19);
        assert_eq!(verdict_of(&samples, &config()), Verdict::Live);
    }

    #[test]
    fn sub_audible_drift_without_sound_is_dead() {
        // 3 Hz wander at -40 dBFS around a DC offset: a dead capture that is
        // not bit-exact silent. The high-pass removes it.
        let samples = tone(seconds(2.0), 3.0, 0.01, -0.19);
        assert_eq!(verdict_of(&samples, &config()), Verdict::Dead);
    }

    #[test]
    fn silence_before_speech_is_live() {
        // A capture that opens with 1.1 s of zeros and then carries sound is
        // live: the peak frame decides, not the average.
        let mut samples = vec![0.0f32; seconds(1.1)];
        samples.extend(noise(seconds(1.5), 0.001));
        let config = DeadInputConfig {
            window_ms: 2500,
            ..config()
        };
        assert_eq!(verdict_of(&samples, &config), Verdict::Live);
    }

    #[test]
    fn verdict_is_pending_until_the_window_closes() {
        let mut det = DeadInputDetector::new(SR, &config());
        // 100 ms settle + 1500 ms window; 1 s of silence is not yet an answer.
        assert_eq!(det.push(&vec![0.0f32; seconds(1.0)]), Verdict::Pending);
        assert_eq!(det.push(&vec![0.0f32; seconds(1.0)]), Verdict::Dead);
    }

    #[test]
    fn chunked_feeding_matches_a_single_buffer() {
        let samples = noise(seconds(2.0), 0.001);
        let mut chunked = DeadInputDetector::new(SR, &config());
        let mut verdict = Verdict::Pending;
        for chunk in samples.chunks(137) {
            verdict = chunked.push(chunk);
            if verdict != Verdict::Pending {
                break;
            }
        }
        assert_eq!(verdict, Verdict::Live);
        assert_eq!(verdict_of(&samples, &config()), Verdict::Live);
    }

    #[test]
    fn a_decided_verdict_never_changes() {
        let mut det = DeadInputDetector::new(SR, &config());
        assert_eq!(det.push(&vec![0.0f32; seconds(2.0)]), Verdict::Dead);
        assert_eq!(det.push(&noise(seconds(2.0), 0.1)), Verdict::Dead);
    }

    #[test]
    fn the_start_up_click_of_a_dead_capture_is_ignored() {
        // The settle window swallows a 10 ms burst at the very start, which
        // is what a device's own power-on transient looks like.
        let mut samples = noise(seconds(0.01), 0.05);
        samples.extend(vec![-0.19f32; seconds(2.0)]);
        assert_eq!(verdict_of(&samples, &config()), Verdict::Dead);
    }

    #[test]
    fn threshold_is_honoured() {
        let samples = noise(seconds(2.0), 0.001); // -60 dBFS
        let deaf = DeadInputConfig {
            threshold_dbfs: -40.0,
            ..config()
        };
        assert_eq!(verdict_of(&samples, &deaf), Verdict::Dead);
    }

    #[test]
    fn peak_dbfs_reports_the_loudest_frame() {
        let mut det = DeadInputDetector::new(SR, &config());
        det.push(&vec![0.0f32; seconds(2.0)]);
        assert_eq!(det.peak_dbfs(), f32::NEG_INFINITY);

        let mut det = DeadInputDetector::new(SR, &config());
        det.push(&noise(seconds(2.0), 0.01));
        assert!(
            (det.peak_dbfs() - (-40.0)).abs() < 3.0,
            "peak {} dBFS should be near -40 dBFS",
            det.peak_dbfs()
        );
    }

    #[test]
    fn dbfs_round_trips() {
        assert!((linear_to_dbfs(dbfs_to_linear(-90.0)) + 90.0).abs() < 1e-3);
        assert!((dbfs_to_linear(0.0) - 1.0).abs() < 1e-6);
    }
}
