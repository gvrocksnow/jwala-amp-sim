//! The nih-plug plugin: audio callback, cab-IR background builder and state shared with the UI.

use crate::dsp::convolver::{build_cab, PreparedIr};
use crate::dsp::rig::Rig;
use crate::dsp::tuner::TunerShared;
use crate::params::TubeParams;
use atomic_float::AtomicF32;
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// A user impulse response loaded from a .wav file.
pub struct UserIr {
    pub data: Vec<f32>,
    pub rate: u32,
    pub path: String,
}

/// State shared between the audio thread, the background builder and the editor.
#[derive(Default)]
pub struct Shared {
    pub tuner: TunerShared,
    pub meter_in: AtomicF32,
    pub meter_l: AtomicF32,
    pub meter_r: AtomicF32,
    pub cpu: AtomicF32,
    pub sample_rate: AtomicF32,
    pub block_size: AtomicU32,
    /// The loaded user IR, if any; `user_ir_version` bumps whenever it changes.
    pub user_ir: Mutex<Option<UserIr>>,
    pub user_ir_version: AtomicU32,
    /// A freshly built cab IR waiting for the audio thread.
    pending_ir: Mutex<Option<PreparedIr>>,
    /// Cab model the standalone host should build (-1 = none), set from the audio thread.
    pub cab_request: std::sync::atomic::AtomicI32,
    /// IRs the audio thread swapped out, dropped on the next build (keeps deallocation off the audio thread).
    retired_ir: Mutex<Vec<PreparedIr>>,
    /// True while running as the standalone app (the editor then saves/restores the last state itself).
    pub standalone: AtomicBool,
}

impl Shared {
    /// Builds the IR for `model` and hands it to the audio thread.
    pub fn build_cab(&self, model: usize) {
        let fs = self.sample_rate.load(Ordering::Relaxed) as f64;
        if fs <= 0.0 {
            return;
        }
        let ir = {
            let user = self.user_ir.lock().unwrap();
            build_cab(fs, model, user.as_ref().map(|u| (u.data.as_slice(), u.rate)))
        };
        *self.pending_ir.lock().unwrap() = Some(ir);
        self.retired_ir.lock().unwrap().clear();
    }

    /// Loads a .wav impulse response (first channel, at most 8x the convolver length).
    pub fn load_user_ir(&self, path: &str) -> Result<(), String> {
        let mut r = hound::WavReader::open(path).map_err(|e| e.to_string())?;
        let spec = r.spec();
        let ch = spec.channels.max(1) as usize;
        let max = crate::dsp::convolver::MAX_LENGTH * 8 * ch;
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => r.samples::<f32>().take(max).map(|s| s.unwrap_or(0.0)).collect(),
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
                r.samples::<i32>().take(max).map(|s| s.unwrap_or(0) as f32 * scale).collect()
            }
        };
        let data: Vec<f32> = samples.chunks(ch).map(|f| f[0]).collect();
        if data.is_empty() {
            return Err("the file contains no audio".into());
        }
        *self.user_ir.lock().unwrap() = Some(UserIr { data, rate: spec.sample_rate, path: path.to_string() });
        self.user_ir_version.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

pub enum Task {
    BuildCab(usize),
}

/// The signal chain plus everything around it (cab IR swaps, tuner feed, meters, CPU load).
/// Used by the plugin and by the standalone's own audio host.
pub struct Engine {
    params: Arc<TubeParams>,
    shared: Arc<Shared>,
    rig: Rig,
    input: Vec<f32>,
    cab_built: (i32, u32),
}

impl Engine {
    /// Allocates and builds the current cab IR; call off the audio thread.
    pub fn new(params: Arc<TubeParams>, shared: Arc<Shared>, fs: f64, max_block: usize) -> Engine {
        let max = max_block.max(64);
        shared.sample_rate.store(fs as f32, Ordering::Relaxed);
        shared.block_size.store(max_block as u32, Ordering::Relaxed);
        // reload the user IR saved with the state, if it isn't loaded yet
        let ir_path = params.persisted.read().unwrap().ir_path.clone();
        if !ir_path.is_empty() && shared.user_ir.lock().unwrap().is_none() {
            let _ = shared.load_user_ir(&ir_path);
        }
        let st = params.snapshot();
        let mut rig = Rig::new(fs, &st, max);
        let model = st.cab.model as usize;
        {
            let user = shared.user_ir.lock().unwrap();
            rig.swap_cab(build_cab(fs, model, user.as_ref().map(|u| (u.data.as_slice(), u.rate))));
        }
        let cab_built = (model as i32, shared.user_ir_version.load(Ordering::Relaxed));
        Engine { params, shared, rig, input: vec![0.0; max], cab_built }
    }

    /// `l` holds the mono input on entry; `l`/`r` hold the stereo output on return.
    /// `request_cab(model)` is called when the cab IR must be rebuilt off the audio thread.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32], request_cab: &mut dyn FnMut(usize)) {
        let t0 = std::time::Instant::now();
        let shared = &*self.shared;
        let st = self.params.snapshot();

        // cab model or user IR changed: rebuild in the background, swap in when ready
        let want = (st.cab.model as i32, shared.user_ir_version.load(Ordering::Relaxed));
        if want != self.cab_built {
            self.cab_built = want;
            request_cab(want.0 as usize);
        }
        if let Ok(mut slot) = shared.pending_ir.try_lock() {
            if let Some(ir) = slot.take() {
                let old = self.rig.swap_cab(ir);
                if let Ok(mut retired) = shared.retired_ir.try_lock() {
                    if retired.len() < retired.capacity() {
                        retired.push(old);
                    } else {
                        *slot = Some(old); // no room without allocating: park it in the slot instead
                    }
                }
            }
        }

        let n = l.len();
        let muted = shared.tuner.active.load(Ordering::Relaxed) && shared.tuner.mute_output.load(Ordering::Relaxed);
        let mut off = 0;
        while off < n {
            let len = (n - off).min(self.input.len());
            self.input[..len].copy_from_slice(&l[off..off + len]);
            shared.tuner.write(&self.input[..len]);
            self.rig.process(&self.input[..len], &mut l[off..off + len], &mut r[off..off + len], &st, muted);
            off += len;
        }

        let m = self.rig.meters;
        shared.meter_in.store(m.input, Ordering::Relaxed);
        shared.meter_l.store(m.out_l, Ordering::Relaxed);
        shared.meter_r.store(m.out_r, Ordering::Relaxed);
        let budget = n as f64 / self.rig.sample_rate;
        if budget > 0.0 {
            let load = (t0.elapsed().as_secs_f64() / budget) as f32;
            let prev = shared.cpu.load(Ordering::Relaxed);
            shared.cpu.store(prev + 0.1 * (load - prev), Ordering::Relaxed);
        }
    }
}

pub struct Jwala {
    pub params: Arc<TubeParams>,
    pub shared: Arc<Shared>,
    engine: Option<Engine>,
    /// Right channel scratch for mono-output hosts.
    scratch: Vec<f32>,
}

impl Default for Jwala {
    fn default() -> Self {
        let shared = Shared::default();
        shared.standalone.store(crate::STANDALONE.load(Ordering::Relaxed), Ordering::Relaxed);
        shared.retired_ir.lock().unwrap().reserve(8);
        shared.cab_request.store(-1, Ordering::Relaxed);
        Jwala { params: Arc::new(TubeParams::default()), shared: Arc::new(shared), engine: None, scratch: Vec::new() }
    }
}

impl Plugin for Jwala {
    const NAME: &'static str = "Jwala";
    const VENDOR: &'static str = "Jwala";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout { main_input_channels: NonZeroU32::new(2), main_output_channels: NonZeroU32::new(2), ..AudioIOLayout::const_default() },
        AudioIOLayout { main_input_channels: NonZeroU32::new(1), main_output_channels: NonZeroU32::new(2), ..AudioIOLayout::const_default() },
    ];

    type SysExMessage = ();
    type BackgroundTask = Task;

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn task_executor(&mut self) -> TaskExecutor<Self> {
        let shared = self.shared.clone();
        Box::new(move |task| match task {
            Task::BuildCab(model) => shared.build_cab(model),
        })
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        crate::ui::create_editor(self.params.clone(), self.shared.clone())
    }

    fn initialize(&mut self, _layout: &AudioIOLayout, config: &BufferConfig, _context: &mut impl InitContext<Self>) -> bool {
        // the standalone runs its own audio host (ASIO/WASAPI/DI file); nih-plug's wrapper only
        // drives the window and parameters there, on its silent dummy backend
        if self.shared.standalone.load(Ordering::Relaxed) {
            return true;
        }
        let max = config.max_buffer_size.max(64) as usize;
        self.engine = Some(Engine::new(self.params.clone(), self.shared.clone(), config.sample_rate as f64, max));
        self.scratch = vec![0.0; max];
        true
    }

    fn process(&mut self, buffer: &mut Buffer, _aux: &mut AuxiliaryBuffers, context: &mut impl ProcessContext<Self>) -> ProcessStatus {
        let Some(engine) = self.engine.as_mut() else {
            for ch in buffer.as_slice() {
                ch.fill(0.0);
            }
            return ProcessStatus::Normal;
        };
        let mut request = |model: usize| context.execute_background(Task::BuildCab(model));
        let channels = buffer.as_slice();
        let (l, rest) = channels.split_at_mut(1);
        match rest.first_mut() {
            Some(r) => engine.process(&mut l[0][..], &mut r[..], &mut request),
            None => {
                let n = l[0].len().min(self.scratch.len());
                engine.process(&mut l[0][..n], &mut self.scratch[..n], &mut request);
            }
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for Jwala {
    const CLAP_ID: &'static str = "com.jwala.amp-rig";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("Tube amp, cab and effects rig");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::AudioEffect, ClapFeature::Stereo, ClapFeature::Distortion];
}

impl Vst3Plugin for Jwala {
    const VST3_CLASS_ID: [u8; 16] = *b"JwalaGuitarRig01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Fx, Vst3SubCategory::Distortion];
}
