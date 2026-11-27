//! Low-cost streaming audio analysis; no renderer or GPU dependencies.
use anyhow::{bail, Context, Result};
use std::{io::{Read, ErrorKind}, os::fd::AsRawFd, process::{Child, Command, Stdio},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};

use rustfft::{Fft, FftPlanner, num_complex::Complex32};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 512;
const WINDOW: usize = 2048;
const BINS: usize = WINDOW / 2 + 1;
const DT: f32 = BLOCK as f32 / RATE;

#[derive(Debug, Default, Clone, Copy)]
pub struct Signals {
    pub onset: f32,
    pub kick: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub volume: f32,
    pub onset_strength: f32,
    pub kick_strength: f32,
    // Instantaneous log spectral flux and the threshold for that same hop.
    pub onset_flux: f32,
    pub kick_flux: f32,
    pub onset_threshold: f32,
    pub kick_threshold: f32,
    pub onset_linear_flux: f32,
    pub kick_linear_flux: f32,
    pub onset_count: u64,
    pub kick_count: u64,
}
// Causal adaptive peak picking: compare positive spectral flux with
// its recent mean and require a local maximum (one hop of lookahead).
// See FMP chapter 6, Spectral-Based Novelty and Peak Picking:
// https://www.audiolabs-erlangen.de/resources/MIR/FMP/C6/C6S1_NoveltySpectral.html
// https://www.audiolabs-erlangen.de/resources/MIR/FMP/C6/C6S1_PeakPicking.html
#[derive(Default)]
struct Peaks {
    mean: f32,
    older: f32,
    previous: f32,
    previous_amplitude: f32,
    threshold: f32,
    previous_audible: bool,
    cooldown: f32,
    envelope: f32,
    strength: f32,
    count: u64,
}
impl Peaks {
    fn update(&mut self, flux: f32, amplitude: f32, audible: bool, sensitivity: f32) -> f32 {
        self.cooldown = (self.cooldown - DT).max(0.);
        self.envelope *= (-DT / 0.15).exp();
        if self.cooldown == 0. && self.previous_audible
            && self.previous > self.older && self.previous >= flux
            && self.previous > self.threshold {
            // The adaptive threshold decides whether an event occurred, never
            // its visual amplitude. Use uncompressed positive magnitude flux
            // with a fixed saturating mapping (no running gain normalization).
            self.strength = impact_strength(self.previous_amplitude, sensitivity);
            self.envelope = 1.;
            self.count += 1;
            self.cooldown = 0.09;
        }
        self.older = self.previous;
        self.previous = flux;
        self.previous_amplitude = amplitude;
        self.previous_audible = audible;
        self.threshold = (1.5 * self.mean + 0.002) / sensitivity.sqrt();
        self.mean += (1. - (-DT / 0.4).exp()) * (flux - self.mean);
        self.envelope
    }
}

// Fixed half-saturation point in normalized PCM magnitude units.
// This is a visual gain choice, not part of the onset detection algorithm.
fn impact_strength(amplitude: f32, sensitivity: f32) -> f32 {
    let x = amplitude * sensitivity;
    x / (x + 0.02)
}

struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    spectrum: Vec<Complex32>,
    history: [f32; WINDOW],
    window: [f32; WINDOW],
    previous: [f32; BINS],
    previous_magnitude: [f32; BINS],
    filled: usize,
    onset: Peaks,
    kick: Peaks,
    sensitivity: f32,
    signals: Signals,
}
impl Analyzer {
    fn new(sensitivity: f32) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(WINDOW);
        let scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
        Self { fft, scratch, spectrum: vec![Complex32::default(); WINDOW],
            history: [0.; WINDOW],
            window: std::array::from_fn(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos()),
            previous: [0.; BINS], previous_magnitude: [0.; BINS], filled: 0, onset: Peaks::default(), kick: Peaks::default(),
            sensitivity, signals: Signals::default() }
    }
    fn process(&mut self, samples: &[f32; BLOCK]) -> Signals {
        self.history.copy_within(BLOCK.., 0);
        for (dst, &x) in self.history[WINDOW-BLOCK..].iter_mut().zip(samples) {
            *dst = if x.is_finite() { x.clamp(-1.,1.) } else { 0. };
        }
        self.filled = (self.filled + BLOCK).min(WINDOW);
        for ((dst, &x), &w) in self.spectrum.iter_mut().zip(&self.history).zip(&self.window) {
            *dst = Complex32::new(x*w, 0.);
        }
        self.fft.process_with_scratch(&mut self.spectrum, &mut self.scratch);
        let mut energy = [0.; 3];
        let mut flux = [0.; 3];
        let mut linear_flux = [0.; 3];
        let mut counts = [0usize; 3];
        let mut kick_flux = 0.;
        let mut kick_linear_flux = 0.;
        let mut kick_bins = 0;
        let mut kick_energy = 0.;
        for k in 1..BINS-1 {
            let hz = k as f32 * RATE / WINDOW as f32;
            let power = self.spectrum[k].norm_sqr() * (2. / (WINDOW as f32 * WINDOW as f32 * 0.375));
            let magnitude = power.sqrt();
            // Log compression makes quieter transients visible; half-wave
            // rectification ignores energy decreases.
            let compressed = (1. + 100. * magnitude).ln();
            let change = (compressed - self.previous[k]).max(0.);
            self.previous[k] = compressed;
            let linear_change = (magnitude - self.previous_magnitude[k]).max(0.);
            self.previous_magnitude[k] = magnitude;
            let band = if hz < 200. { 0 } else if hz < 2500. { 1 } else { 2 };
            energy[band] += power;
            flux[band] += change;
            linear_flux[band] += linear_change * linear_change;
            counts[band] += 1;
            if hz <= 150. {
                kick_flux += change;
                kick_linear_flux += linear_change * linear_change;
                kick_energy += power;
                kick_bins += 1;
            }
        }
        let rms = energy.map(f32::sqrt);
        let volume = energy.iter().sum::<f32>().sqrt();
        // Equal weighting of the three bands prevents the numerous high
        // frequency bins from overwhelming low-frequency impacts.
        let novelty = (0..3).map(|i| flux[i] / counts[i] as f32).sum::<f32>() / 3.;
        let kick_novelty = kick_flux / kick_bins as f32;
        let ready = self.filled == WINDOW;
        let onset_amplitude = linear_flux.iter().sum::<f32>().sqrt();
        let kick_amplitude = kick_linear_flux.sqrt();
        self.signals.onset = self.onset.update(novelty, onset_amplitude, ready && volume > 0.003, self.sensitivity);
        self.signals.kick = self.kick.update(kick_novelty, kick_amplitude, ready && kick_energy.sqrt() > 0.003, self.sensitivity);
        self.signals.onset_flux = novelty;
        self.signals.kick_flux = kick_novelty;
        self.signals.onset_threshold = self.onset.threshold;
        self.signals.kick_threshold = self.kick.threshold;
        self.signals.onset_linear_flux = onset_amplitude;
        self.signals.kick_linear_flux = kick_amplitude;
        self.signals.onset_strength = self.onset.strength;
        self.signals.kick_strength = self.kick.strength;
        self.signals.onset_count = self.onset.count;
        self.signals.kick_count = self.kick.count;
        let smooth = |old: f32, rms: f32| {
            let next = (rms * 4. * self.sensitivity).clamp(0.,1.);
            let tau = if next > old { 0.025 } else { 0.12 };
            old + (1. - (-DT/tau).exp()) * (next-old)
        };
        self.signals.bass = smooth(self.signals.bass,rms[0]);
        self.signals.mid = smooth(self.signals.mid,rms[1]);
        self.signals.treble = smooth(self.signals.treble,rms[2]);
        self.signals.volume = smooth(self.signals.volume,volume);
        self.signals
    }
    fn reset(&mut self) {
        self.history.fill(0.);
        self.previous.fill(0.);
        self.previous_magnitude.fill(0.);
        self.filled = 0;
        let (onsets,kicks) = (self.onset.count,self.kick.count);
        self.onset = Peaks { count: onsets, ..Peaks::default() };
        self.kick = Peaks { count: kicks, ..Peaks::default() };
        self.signals = Signals { onset_count: onsets, kick_count: kicks, ..Signals::default() };
    }
}

struct Capture(Child);
impl Drop for Capture {
    fn drop(&mut self) {
        if let Err(e) = self.0.kill() {
            if e.kind() != ErrorKind::InvalidInput { eprintln!("Stop audio capture: {e}"); }
        }
        if let Err(e) = self.0.wait() { eprintln!("Reap audio capture: {e}"); }
    }
}

fn capture(target: Option<&str>, sensitivity: f32, running: &AtomicBool, output: &Mutex<Signals>) -> Result<()> {
    // PulseAudio compatibility via PipeWire: an explicit monitor source avoids
    // accidentally capturing the default microphone. No shell or disk recording.
    let target = target.unwrap_or("@DEFAULT_MONITOR@");
    let mut child = Capture(Command::new("parec")
        .args(["--raw", "--format=float32le", "--rate=48000", "--channels=1", "--latency-msec=20", "--device", target])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit())
        .spawn().context("Start parec (install libpulse and enable pipewire-pulse)")?);
    let mut pipe = child.0.stdout.take().context("Missing capture stdout")?;
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("Set nonblocking audio capture");
    }
    let mut analyzer = Analyzer::new(sensitivity);
    let mut bytes = [0u8; BLOCK * 4];
    let mut samples = [0f32; BLOCK];
    let mut used = 0;
    let mut last = Instant::now();
    while running.load(Ordering::Acquire) {
        match pipe.read(&mut bytes[used..]) {
            Ok(0) => bail!("Audio capture closed"),
            Ok(n) => {
                used += n;
                last = Instant::now();
                if used == bytes.len() {
                    for (sample, chunk) in samples.iter_mut().zip(bytes.chunks_exact(4)) {
                        *sample = f32::from_le_bytes(chunk.try_into().expect("four bytes"));
                    }
                    *output.lock().expect("audio mutex poisoned") = analyzer.process(&samples);
                    used = 0;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                if last.elapsed() > Duration::from_secs(2) {
                    analyzer.reset();
                    *output.lock().expect("audio mutex poisoned") = analyzer.signals;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e).context("Read audio capture"),
        }
    }
    Ok(())
}

// Each distinct signal/envelope pair is evaluated once per desktop tick.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    #[default] Onset,
    Kick, Bass, Mid, Treble, Volume, OnsetStrength, KickStrength,
}
impl Signal {
    pub fn read(self, s: &Signals) -> f32 {
        match self {
            Self::Onset => s.onset, Self::Kick => s.kick, Self::Bass => s.bass,
            Self::Mid => s.mid, Self::Treble => s.treble, Self::Volume => s.volume,
            Self::OnsetStrength => s.onset*s.onset_strength,
            Self::KickStrength => s.kick*s.kick_strength,
        }
    }
}
fn default_attack() -> f32 { 0.01 }
fn default_power() -> f32 { 1.0 }
fn default_true() -> bool { true }
fn default_decay() -> f32 { 0.15 }
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Response {
    #[serde(default)] pub signal: Signal,
    #[serde(default = "default_attack")] pub attack: f32,
    #[serde(default = "default_decay")] pub decay: f32,
    #[serde(default = "default_power")] pub power: f32,
    #[serde(default = "default_true")] pub logarithmic: bool,
}
impl Response {
    pub fn validate(self) -> Result<()> {
        if !(0.0..=5.0).contains(&self.attack) || !(0.0..=10.0).contains(&self.decay) {
            bail!("Audio attack must be 0..=5s and decay 0..=10s");
        }
        if !(0.1..=8.0).contains(&self.power) { bail!("Audio power must be 0.1..=8"); }
        if self.logarithmic && self.power != 1.0 { bail!("Logarithmic audio response cannot be combined with nondefault power"); }
        Ok(())
    }
    pub fn key(self) -> (Signal,u32,u32,u32,bool) {
        (self.signal,self.attack.to_bits(),self.decay.to_bits(),self.power.to_bits(),self.logarithmic)
    }
}
#[derive(Debug, Clone, Copy, Default)]
pub struct Modulation { pub value: f32, pub integral: f64 }
#[derive(Default)]
pub struct Frame {
    pub signals: Signals,
    values: std::collections::BTreeMap<(Signal,u32,u32,u32,bool), Modulation>,
}
impl Frame {
    pub fn get(&self, response: Response) -> Modulation {
        self.values.get(&response.key()).copied().unwrap_or_default()
    }
    pub fn advance(&mut self, signals: Signals, responses: &[Response], dt: f64, active: bool) {
        self.signals = if active { signals } else { Signals::default() };
        for response in responses {
            let state = self.values.entry(response.key()).or_default();
            if !active { state.value = 0.; continue; }
            let input = response.signal.read(&signals).clamp(0.,1.);
            let target = if response.logarithmic { ((9.0 * input).ln_1p() / std::f32::consts::LN_10).clamp(0.,1.) } else if response.power == 1.0 { input } else if response.power == 2.0 { input*input } else { input.powf(response.power) };
            let tau = if target > state.value { response.attack } else { response.decay } as f64;
            let old = state.value as f64;
            let target = target as f64;
            // Exact integral of an exponentially smoothed constant input.
            // Rotation speed can change without resetting orientation at beats.
            if tau == 0. {
                state.value = target as f32;
                state.integral += target*dt;
            } else {
                let a = -(-dt/tau).exp_m1();
                state.value = (old+(target-old)*a) as f32;
                state.integral += target*dt + (old-target)*tau*a;
            }
        }
    }
}
// Live capture owns one worker per desktop, not per renderer/output.
pub struct LiveAudio {
    running: Arc<AtomicBool>,
    values: Arc<Mutex<Signals>>,
    worker: Option<thread::JoinHandle<()>>,
}
impl LiveAudio {
    pub fn new() -> Self {
        Self { running: Arc::new(AtomicBool::new(false)), values: Arc::new(Mutex::new(Signals::default())), worker: None }
    }
    pub fn set_active(&mut self, active: bool, sensitivity: f32) {
        if !active {
            self.running.store(false, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                if worker.join().is_err() { eprintln!("Audio worker panicked"); }
            }
            *self.values.lock().expect("audio mutex poisoned") = Signals::default();
        } else if self.worker.is_none() {
            self.running.store(true, Ordering::Release);
            let running = self.running.clone();
            let values = self.values.clone();
            match thread::Builder::new().name("floatpaper-audio".into()).spawn(move || {
                while running.load(Ordering::Acquire) {
                    if let Err(e) = capture(None, sensitivity, &running, &values) {
                        eprintln!("Audio capture: {e:#}; using silence, retrying in 5s");
                        *values.lock().expect("audio mutex poisoned") = Signals::default();
                        for _ in 0..100 {
                            if !running.load(Ordering::Acquire) { break; }
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }) {
                Ok(worker) => self.worker = Some(worker),
                Err(e) => { eprintln!("Start audio worker: {e}"); self.running.store(false, Ordering::Release); }
            }
        }
    }
    pub fn snapshot(&self) -> Signals { *self.values.lock().expect("audio mutex poisoned") }
}
impl Drop for LiveAudio {
    fn drop(&mut self) { self.set_active(false, 1.0); }
}

pub fn diagnose(target: Option<String>, sensitivity: f32) -> Result<()> {
    if !sensitivity.is_finite() || !(0.25..=4.0).contains(&sensitivity) { bail!("Sensitivity must be 0.25..=4"); }
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    ctrlc::set_handler(move || signal.store(false, Ordering::Release))?;
    let values = Arc::new(Mutex::new(Signals::default()));
    let worker_values = values.clone();
    let worker_running = running.clone();
    let worker = thread::Builder::new().name("floatpaper-audio".into()).spawn(move || {
        let result = capture(target.as_deref(), sensitivity, &worker_running, &worker_values);
        worker_running.store(false, Ordering::Release);
        result
    })?;
    eprintln!("Audio diagnostic: playback monitor, Ctrl-C to stop");
    eprintln!("flux=log novelty/threshold; linear_flux=onset/kick; strength=decaying weighted impact");
    let mut last = (0,0);
    while running.load(Ordering::Acquire) {
        let s = *values.lock().expect("audio mutex poisoned");
        println!("onset={:.2} kick={:.2} bass={:.2} mid={:.2} treble={:.2} volume={:.2} onset_strength={:.2} kick_strength={:.2} onset_flux={:.4}/{:.4} kick_flux={:.4}/{:.4} linear_flux={:.4}/{:.4} onsets={} kicks={}{}{}",
            s.onset,s.kick,s.bass,s.mid,s.treble,s.volume,
            s.onset*s.onset_strength,s.kick*s.kick_strength,
            s.onset_flux,s.onset_threshold,s.kick_flux,s.kick_threshold,
            s.onset_linear_flux,s.kick_linear_flux,s.onset_count,s.kick_count,
            if s.onset_count != last.0 { " ONSET" } else { "" },
            if s.kick_count != last.1 { " KICK" } else { "" });
        last = (s.onset_count,s.kick_count);
        thread::sleep(Duration::from_millis(100));
    }
    worker.join().map_err(|_| anyhow::anyhow!("Audio worker panicked"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logarithmic_response_preserves_endpoints_and_boosts_quiet_input() {
        let response = Response { signal: Signal::Volume, attack: 0., decay: 0., power: 1., logarithmic: true };
        response.validate().unwrap();
        let mut frame = Frame::default();
        for (volume, expected) in [(0.,0.), (0.5, 5.5_f32.ln()/std::f32::consts::LN_10), (1.,1.)] {
            frame.advance(Signals { volume, ..Signals::default() }, &[response], 1., true);
            assert!((frame.get(response).value-expected).abs()<1e-6);
        }
    }
    #[test]
    fn response_power_is_applied_before_smoothing_and_integration() {
        let linear = Response { signal: Signal::Volume, attack: 0., decay: 0., power: 1., logarithmic: false };
        let squared = Response { power: 2., ..linear };
        let mut frame = Frame::default();
        frame.advance(Signals { volume: 0.5, ..Signals::default() }, &[linear,squared], 1., true);
        assert_eq!(frame.get(linear).value, 0.5);
        assert_eq!(frame.get(squared).value, 0.25);
        assert_eq!(frame.get(squared).integral, 0.25);
        assert!(Response { power: 0., ..linear }.validate().is_err());
    }
    #[test]
    fn impact_amplitude_is_independent_of_detection_threshold() {
        let mut quiet = Peaks { previous: 0.2, previous_amplitude: 0.04, threshold: 0.01,
            previous_audible: true, ..Peaks::default() };
        let mut loud = Peaks { threshold: 0.15, ..Peaks {
            previous: 0.2, previous_amplitude: 0.04, previous_audible: true, ..Peaks::default()
        }};
        quiet.update(0.0, 0.0, true, 1.0);
        loud.update(0.0, 0.0, true, 1.0);
        assert_eq!(quiet.count, 1);
        assert_eq!(loud.count, 1);
        assert_eq!(quiet.strength, loud.strength);
        assert!(impact_strength(0.08, 1.0) > impact_strength(0.04, 1.0));
        assert_eq!(impact_strength(0.0, 1.0), 0.0);
    }
    #[test]
    fn silence_and_invalid_samples_are_safe() {
        let mut a = Analyzer::new(1.);
        for _ in 0..100 {
            let s = a.process(&[f32::NAN; BLOCK]);
            assert_eq!(s.onset,0.);
            assert_eq!(s.kick,0.);
            assert_eq!(s.volume,0.);
        }
    }
    #[test]
    fn repeated_impacts_over_sustained_tone() {
        let mut a = Analyzer::new(1.);
        for block in 0..1000 {
            let phase = block % 48;
            let samples = std::array::from_fn(|i| {
                let t = (block*BLOCK+i) as f32 / RATE;
                let pulse = if phase < 6 { 0.35 * (-(phase as f32)/3.).exp() } else { 0. };
                (0.10+pulse)*(std::f32::consts::TAU*70.*t).sin()
            });
            a.process(&samples);
        }
        assert!(a.signals.kick_count >= 15, "detected {}",a.signals.kick_count);
        assert!(a.signals.kick_count <= 30, "detected {}",a.signals.kick_count);
        for _ in 0..150 { a.process(&[0.;BLOCK]); }
        assert!(a.signals.kick < 0.01);
        assert!(a.signals.volume < 0.01);
    }
    #[test]
    fn steady_tone_does_not_trigger_continuously() {
        let mut a = Analyzer::new(1.);
        for block in 0..500 {
            let samples = std::array::from_fn(|i| {
                let t = (block*BLOCK+i) as f32/RATE;
                0.3*(std::f32::consts::TAU*750.*t).sin()
            });
            a.process(&samples);
        }
        assert!(a.signals.onset_count <= 2);
        assert!(a.signals.mid > a.signals.bass);
        assert!(a.signals.mid > a.signals.treble);
    }
}
