//! Low-cost streaming audio analysis; no renderer or GPU dependencies.
use anyhow::{bail, Context, Result};
use std::{io::{Read, ErrorKind}, os::fd::AsRawFd, process::{Child, Command, Stdio},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};

const RATE: f32 = 48_000.0;
const BLOCK: usize = 480; // 10 ms
const DT: f32 = BLOCK as f32 / RATE;

#[derive(Debug, Default, Clone, Copy)]
pub struct Signals {
    pub beat: f32,
    pub beat_count: u64,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub volume: f32,
}

// Two-pole kick-band filter. Unlike the broad bass meter, this attenuates
// speech fundamentals and DC, concentrating on roughly 40–100 Hz.
struct KickFilter {
    b: f32, a1: f32, a2: f32, z1: f32, z2: f32,
}
impl KickFilter {
    fn new() -> Self {
        let w = std::f32::consts::TAU * 65.0 / RATE;
        let alpha = w.sin() / (2.0 * 1.2);
        let a0 = 1.0 + alpha;
        Self { b: alpha/a0, a1: -2.0*w.cos()/a0, a2: (1.0-alpha)/a0, z1: 0., z2: 0. }
    }
    fn sample(&mut self, x: f32) -> f32 {
        let y = self.b*x + self.z1;
        self.z1 = self.z2 - self.a1*y;
        self.z2 = -self.b*x - self.a2*y;
        y
    }
}
struct Analyzer {
    low: f32,
    high: f32,
    kick: KickFilter,
    kick_reference: f32,
    novelty_floor: f32,
    armed: bool,
    since_beat: f32,
    elapsed: f32,
    sensitivity: f32,
    signals: Signals,
}
impl Analyzer {
    fn new(sensitivity: f32) -> Self {
        Self { low: 0., high: 0., kick: KickFilter::new(), kick_reference: 0., novelty_floor: 0., armed: true, since_beat: 1., elapsed: 0., sensitivity, signals: Signals::default() }
    }
    fn process(&mut self, samples: &[f32; BLOCK]) -> Signals {
        // First-order crossover filters: approximate bands, not FFT bins.
        let low_alpha = 1.0 - (-std::f32::consts::TAU * 180.0 / RATE).exp();
        let high_alpha = 1.0 - (-std::f32::consts::TAU * 2500.0 / RATE).exp();
        let mut energy = [0.0; 5];
        for &sample in samples {
            let x = if sample.is_finite() { sample.clamp(-1., 1.) } else { 0. };
            self.low += low_alpha * (x - self.low);
            self.high += high_alpha * (x - self.high);
            let mid = self.high - self.low;
            let treble = x - self.high;
            energy[0] += self.low * self.low;
            energy[1] += mid * mid;
            energy[2] += treble * treble;
            energy[3] += x * x;
            let kick = self.kick.sample(x);
            energy[4] += kick * kick;
        }
        let rms = energy.map(|e| (e / BLOCK as f32).sqrt());
        self.elapsed += DT;
        self.since_beat += DT;
        // Positive log-energy novelty against a short local reference.
        // The reference follows sustained bass, so repeated kicks can trigger
        // even after the song has reached its normal loudness.
        let kick = rms[4];
        let novelty = ((kick + 0.003) / (self.kick_reference + 0.003)).ln().max(0.0);
        let threshold = (0.18 / self.sensitivity.sqrt()).max(2.2 * self.novelty_floor);
        let dominance = kick / rms[3].max(0.003);
        if novelty < threshold * 0.55 { self.armed = true; }
        let onset = self.elapsed > 0.15 && self.since_beat >= 0.22 && self.armed
            && kick > 0.006 && dominance > 0.22 && novelty > threshold;
        self.kick_reference += (1.0 - (-DT / 0.075).exp()) * (kick - self.kick_reference);
        self.novelty_floor += (1.0 - (-DT / 1.0).exp()) * (novelty.min(0.5) - self.novelty_floor);
        self.signals.beat *= (-DT / 0.18).exp();
        if onset {
            self.signals.beat = 1.0;
            self.since_beat = 0.0;
            self.armed = false;
            self.signals.beat_count += 1;
        }
        let smooth = |old: f32, rms: f32| {
            let next = (rms * 4.0 * self.sensitivity).clamp(0., 1.);
            let tau = if next > old { 0.025 } else { 0.12 };
            old + (1.0 - (-DT / tau).exp()) * (next - old)
        };
        self.signals.bass = smooth(self.signals.bass, rms[0]);
        self.signals.mid = smooth(self.signals.mid, rms[1]);
        self.signals.treble = smooth(self.signals.treble, rms[2]);
        self.signals.volume = smooth(self.signals.volume, rms[3]);
        self.signals
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
                    let count = analyzer.signals.beat_count;
                    analyzer = Analyzer::new(sensitivity);
                    analyzer.signals.beat_count = count;
                    *output.lock().expect("audio mutex poisoned") = analyzer.signals;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e).context("Read audio capture"),
        }
    }
    Ok(())
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
    let mut last_count = 0;
    while running.load(Ordering::Acquire) {
        let s = *values.lock().expect("audio mutex poisoned");
        println!("beat={:.2} count={} bass={:.2} mid={:.2} treble={:.2} volume={:.2}{}", s.beat, s.beat_count, s.bass, s.mid, s.treble, s.volume, if s.beat_count != last_count { "  KICK" } else { "" });
        last_count = s.beat_count;
        thread::sleep(Duration::from_millis(100));
    }
    worker.join().map_err(|_| anyhow::anyhow!("Audio worker panicked"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_and_invalid_samples_are_safe() {
        let mut a = Analyzer::new(1.);
        for _ in 0..100 {
            let s = a.process(&[f32::NAN; BLOCK]);
            assert_eq!(s.beat, 0.);
            assert_eq!(s.volume, 0.);
        }
    }
    #[test]
    fn bass_transient_triggers_then_decays() {
        let mut a = Analyzer::new(1.);
        for _ in 0..100 { a.process(&[0.; BLOCK]); }
        let tone = std::array::from_fn(|i| 0.5 * (std::f32::consts::TAU * 100. * i as f32 / RATE).sin());
        let s = a.process(&tone);
        assert_eq!(s.beat, 1.);
        assert!(s.bass > s.treble);
        for _ in 0..100 { a.process(&[0.; BLOCK]); }
        assert!(a.signals.beat < 0.01);
        assert!(a.signals.volume < 0.01);
    }
    #[test]
    fn repeated_kicks_over_sustained_bass() {
        let mut a = Analyzer::new(1.);
        for block in 0..1000 {
            let phase = block % 50;
            let tone = std::array::from_fn(|i| {
                let t = (block * BLOCK + i) as f32 / RATE;
                let kick = if phase < 8 { 0.30 * (-phase as f32 / 4.0).exp() } else { 0.0 };
                0.12 * (std::f32::consts::TAU * 65.0 * t).sin()
                    + kick * (std::f32::consts::TAU * 65.0 * t).sin()
            });
            a.process(&tone);
        }
        assert!((17..=20).contains(&a.signals.beat_count), "detected {}", a.signals.beat_count);
    }

    #[test]
    fn higher_frequency_pulses_are_not_kicks() {
        let mut a = Analyzer::new(1.);
        for block in 0..300 {
            let tone = std::array::from_fn(|i| {
                let t = (block * BLOCK + i) as f32 / RATE;
                let amp = if block % 50 < 10 { 0.4 } else { 0.0 };
                amp * (std::f32::consts::TAU * 350.0 * t).sin()
            });
            a.process(&tone);
        }
        assert_eq!(a.signals.beat_count, 0);
    }

}
