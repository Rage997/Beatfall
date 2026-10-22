use crate::spectrum::FFT_SIZE;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct Shared {
    ring: [f32; FFT_SIZE],
    cursor: usize,
    sequence: u64,
    updated: Instant,
    pub status: String,
}

impl Shared {
    pub fn push(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.ring[self.cursor] = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            self.cursor = (self.cursor + 1) % FFT_SIZE;
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.updated = Instant::now();
    }
}

pub struct Audio {
    pub shared: Arc<Mutex<Shared>>,
    pub stop: Arc<AtomicBool>,
    pub(crate) worker: Option<JoinHandle<()>>,
    sequence: u64,
}

impl Audio {
    pub fn demo(rate: u32) -> Self {
        let mut audio = Self::empty("DEMO | synthetic only - not capturing system audio".into());
        let shared = audio.shared.clone();
        let stop = audio.stop.clone();
        audio.worker = Some(thread::spawn(move || {
            let start = Instant::now();
            let mut index = 0_u64;
            let mut block = [0.0; 512];
            let frequencies: [f64; 12] =
                std::array::from_fn(|i| 110.0 * 2.0_f64.powf(i as f64 * 0.53));
            while !stop.load(Ordering::Relaxed) {
                for value in &mut block {
                    let t = index as f64 / rate as f64;
                    let phase = t % 0.5;
                    let kick = (-phase * 24.0).exp()
                        * (std::f64::consts::TAU
                            * (55.0 * phase + 3.0 * (1.0 - (-phase * 18.0).exp())))
                        .sin();
                    let mut sample = kick * 0.55;
                    for (voice, frequency) in frequencies.iter().enumerate() {
                        let envelope =
                            0.5 + 0.5 * (t * (0.6 + voice as f64 * 0.13) + voice as f64).sin();
                        sample += 0.035 * envelope * (std::f64::consts::TAU * frequency * t).sin();
                    }
                    *value = sample as f32;
                    index += 1;
                }
                if let Ok(mut state) = shared.lock() {
                    state.push(&block);
                }
                let deadline = start + Duration::from_secs_f64(index as f64 / rate as f64);
                thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
        }));
        audio
    }

    pub fn empty(status: String) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                ring: [0.0; FFT_SIZE],
                cursor: 0,
                sequence: 0,
                updated: Instant::now(),
                status,
            })),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            sequence: 0,
        }
    }

    pub fn snapshot(&mut self, output: &mut [f32; FFT_SIZE], status: &mut String) -> (bool, bool) {
        // try_lock makes the render thread independent of capture scheduling.
        let Ok(state) = self.shared.try_lock() else {
            return (false, false);
        };
        if *status != state.status {
            status.clone_from(&state.status);
        }
        let changed = state.sequence != self.sequence;
        if changed {
            let count = FFT_SIZE - state.cursor;
            output[..count].copy_from_slice(&state.ring[state.cursor..]);
            output[count..].copy_from_slice(&state.ring[..state.cursor]);
            self.sequence = state.sequence;
        }
        (
            changed,
            state.updated.elapsed() > Duration::from_millis(180),
        )
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
