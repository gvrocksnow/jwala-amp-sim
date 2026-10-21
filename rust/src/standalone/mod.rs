//! The standalone app's own audio host: ASIO, WASAPI (through cpal) or a DI recording played
//! through the rig. nih-plug's standalone wrapper still provides the window and parameters (on its
//! silent dummy backend); audio runs here, through the same `Engine` the plugin uses.

pub mod asio;

use crate::params::TubeParams;
use crate::plugin::{Engine, Shared};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Driver {
    #[default]
    Asio,
    Wasapi,
    DiFile,
}

/// Audio device configuration, saved with the standalone's state.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub driver: Driver,
    pub asio_driver: String,
    pub input_device: String,
    pub output_device: String,
    pub input_channel: usize,
    pub di_file: String,
}

impl AudioSettings {
    /// First run: ASIO if a driver is installed (preferring one that matches the default recording
    /// device, e.g. "Focusrite"), otherwise WASAPI.
    pub fn first_run() -> AudioSettings {
        let drivers = asio::drivers();
        if drivers.is_empty() {
            return AudioSettings { driver: Driver::Wasapi, ..Default::default() };
        }
        // score each driver by the words it shares with the default input's name
        // ("Focusrite USB ASIO" vs "Analogue 1 + 2 (Focusrite Usb Audio)" → 2)
        let input = cpal::default_host().default_input_device().and_then(|d| d.name().ok()).unwrap_or_default().to_lowercase();
        let words = |s: &str| s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 1 && *w != "asio" && *w != "audio").map(String::from).collect::<Vec<_>>();
        let inw = words(&input);
        let score = |d: &asio::DriverEntry| words(&d.name).iter().filter(|w| inw.contains(w)).count();
        let pick = drivers
            .iter()
            .filter(|d| score(d) > 0)
            .max_by_key(|d| score(d))
            .or_else(|| drivers.iter().find(|d| d.name.contains("ASIO4ALL")))
            .unwrap_or(&drivers[0]);
        AudioSettings { driver: Driver::Asio, asio_driver: pick.name.clone(), ..Default::default() }
    }
}

/// Device lists for the settings page.
#[derive(Default, Clone)]
pub struct Devices {
    pub asio: Vec<String>,
    /// WASAPI inputs with their channel counts.
    pub inputs: Vec<(String, usize)>,
    pub outputs: Vec<String>,
}

pub fn devices() -> Devices {
    let host = cpal::default_host();
    let inputs = host
        .input_devices()
        .map(|it| it.filter_map(|d| Some((d.name().ok()?, d.default_input_config().map(|c| c.channels() as usize).unwrap_or(2)))).collect())
        .unwrap_or_default();
    let outputs = host.output_devices().map(|it| it.filter_map(|d| d.name().ok()).collect()).unwrap_or_default();
    Devices { asio: asio::drivers().into_iter().map(|d| d.name).collect(), inputs, outputs }
}

enum Cmd {
    Stop,
    Panel,
}

pub struct AudioHost {
    tx: Option<Sender<Cmd>>,
    join: Option<JoinHandle<()>>,
    status: Arc<Mutex<String>>,
    running: Arc<AtomicBool>,
    /// Input channel names of the ASIO driver that is running, if any.
    asio_inputs: Arc<Mutex<(String, Vec<String>)>>,
}

impl Default for AudioHost {
    fn default() -> Self {
        AudioHost {
            tx: None,
            join: None,
            status: Arc::new(Mutex::new("audio stopped".into())),
            running: Arc::new(AtomicBool::new(false)),
            asio_inputs: Arc::new(Mutex::new((String::new(), Vec::new()))),
        }
    }
}

const MAX_BLOCK: usize = 8192;

impl AudioHost {
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    /// Starts audio with `s`; blocks until the device is running or has failed.
    pub fn start(&mut self, s: &AudioSettings, params: Arc<TubeParams>, shared: Arc<Shared>) -> Result<(), String> {
        self.stop();
        let (tx, rx) = channel();
        let (done_tx, done_rx) = channel::<Result<String, String>>();
        let (s, running, asio_inputs) = (s.clone(), self.running.clone(), self.asio_inputs.clone());
        let join = std::thread::Builder::new()
            .name("jwala audio".into())
            .spawn(move || {
                let r = match s.driver {
                    Driver::Asio => run_asio(&s, params, shared, rx, &done_tx, &running, &asio_inputs),
                    Driver::Wasapi => run_wasapi(&s, params, shared, rx, &done_tx, &running),
                    Driver::DiFile => run_file(&s, params, shared, rx, &done_tx, &running),
                };
                running.store(false, Ordering::Relaxed);
                if let Err(e) = r {
                    let _ = done_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        self.tx = Some(tx);
        self.join = Some(join);
        match done_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(status)) => {
                *self.status.lock().unwrap() = status;
                Ok(())
            }
            Ok(Err(e)) => {
                self.stop();
                *self.status.lock().unwrap() = e.clone();
                Err(e)
            }
            Err(_) => {
                self.stop();
                let e = "the audio device didn't respond".to_string();
                *self.status.lock().unwrap() = e.clone();
                Err(e)
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Cmd::Stop);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.running.store(false, Ordering::Relaxed);
        *self.status.lock().unwrap() = "audio stopped".into();
    }

    /// Opens the running ASIO driver's control panel.
    pub fn control_panel(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Panel);
        }
    }

    /// Input channel names for an ASIO driver (from the running driver, or by loading it briefly).
    pub fn asio_inputs(&self, name: &str) -> Vec<String> {
        {
            let cached = self.asio_inputs.lock().unwrap();
            if self.running() && cached.0 == name {
                return cached.1.clone();
            }
        }
        let Some(entry) = asio::drivers().into_iter().find(|d| d.name == name) else { return Vec::new() };
        std::thread::spawn(move || asio::Driver::load(&entry).map(|d| d.input_names()).unwrap_or_default()).join().unwrap_or_default()
    }
}

impl Drop for AudioHost {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Waits for commands, building cab IRs when the audio thread asks. Returns on Stop.
fn control_loop(rx: &Receiver<Cmd>, shared: &Shared, mut on_tick: impl FnMut() -> bool, mut panel: impl FnMut()) {
    loop {
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Cmd::Panel) => panel(),
            Err(RecvTimeoutError::Timeout) => {}
        }
        let m = shared.cab_request.swap(-1, Ordering::Relaxed);
        if m >= 0 {
            shared.build_cab(m as usize);
        }
        if !on_tick() {
            return;
        }
    }
}

fn engine_fn(params: &Arc<TubeParams>, shared: &Arc<Shared>, fs: f64) -> impl FnMut(&mut [f32], &mut [f32]) + Send + 'static {
    let mut engine = Engine::new(params.clone(), shared.clone(), fs, MAX_BLOCK);
    let sh = shared.clone();
    move |l: &mut [f32], r: &mut [f32]| {
        let sh = &sh;
        engine.process(l, r, &mut |m| sh.cab_request.store(m as i32, Ordering::Relaxed));
    }
}

// ------------------------------------------------------------------ ASIO

fn run_asio(
    s: &AudioSettings,
    params: Arc<TubeParams>,
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    done: &Sender<Result<String, String>>,
    running: &AtomicBool,
    inputs: &Mutex<(String, Vec<String>)>,
) -> Result<(), String> {
    let entry = asio::drivers().into_iter().find(|d| d.name == s.asio_driver).ok_or_else(|| {
        if s.asio_driver.is_empty() { "no ASIO driver chosen — open ⚙ Settings".to_string() } else { format!("ASIO driver \"{}\" not found", s.asio_driver) }
    })?;
    let mut drv = asio::Driver::load(&entry)?;
    *inputs.lock().unwrap() = (entry.name.clone(), drv.input_names());
    let start = |drv: &mut asio::Driver| -> Result<String, String> {
        let (fs, frames) = (drv.sample_rate(), drv.preferred_buffer());
        let rt = asio::Runtime::new(Box::new(engine_fn(&params, &shared, fs)), frames.max(MAX_BLOCK));
        drv.start(s.input_channel, frames, rt)?;
        Ok(format!("ASIO · {} · {:.1} kHz · {} smp", entry.name, fs / 1000.0, frames))
    };
    let status = start(&mut drv)?;
    running.store(true, Ordering::Relaxed);
    let _ = done.send(Ok(status));
    asio::RESET_REQUEST.store(false, Ordering::Relaxed);
    let drv = std::cell::RefCell::new(drv);
    control_loop(
        &rx,
        &shared,
        || {
            // the driver's buffer size or rate changed (usually in its control panel): restart it
            if asio::RESET_REQUEST.swap(false, Ordering::Relaxed) {
                let mut d = drv.borrow_mut();
                d.stop();
                return start(&mut d).is_ok();
            }
            true
        },
        || drv.borrow().control_panel(),
    );
    drv.borrow_mut().stop();
    Ok(())
}

// ------------------------------------------------------------------ WASAPI (cpal)

/// Single-producer single-consumer sample FIFO between the input and output callbacks.
struct Fifo {
    buf: Vec<AtomicU32>,
    w: AtomicUsize,
    r: AtomicUsize,
}

impl Fifo {
    fn new(n: usize) -> Fifo {
        Fifo { buf: (0..n).map(|_| AtomicU32::new(0)).collect(), w: AtomicUsize::new(0), r: AtomicUsize::new(0) }
    }
    fn push(&self, v: f32) {
        let w = self.w.load(Ordering::Relaxed);
        self.buf[w % self.buf.len()].store(v.to_bits(), Ordering::Relaxed);
        self.w.store(w + 1, Ordering::Release);
    }
    fn len(&self) -> usize {
        self.w.load(Ordering::Acquire).saturating_sub(self.r.load(Ordering::Relaxed))
    }
    fn pop(&self) -> f32 {
        let r = self.r.load(Ordering::Relaxed);
        let v = f32::from_bits(self.buf[r % self.buf.len()].load(Ordering::Relaxed));
        self.r.store(r + 1, Ordering::Release);
        v
    }
    fn skip(&self, n: usize) {
        self.r.fetch_add(n, Ordering::AcqRel);
    }
}

fn find_device(host: &cpal::Host, name: &str, input: bool) -> Option<cpal::Device> {
    let found = if input { host.input_devices().ok()?.find(|d| d.name().ok().as_deref() == Some(name)) } else { host.output_devices().ok()?.find(|d| d.name().ok().as_deref() == Some(name)) };
    found.or_else(|| if input { host.default_input_device() } else { host.default_output_device() })
}

/// Builds an output stream; `fill(l, r)` renders `l.len()` frames of stereo.
fn output_stream(dev: &cpal::Device, mut fill: impl FnMut(&mut [f32], &mut [f32]) + Send + 'static) -> Result<(cpal::Stream, u32, usize), String> {
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    let fmt = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    let ch = config.channels as usize;
    let fs = config.sample_rate.0;
    let mut l = vec![0.0f32; MAX_BLOCK];
    let mut r = vec![0.0f32; MAX_BLOCK];
    fn build<T: SizedSample + FromSample<f32>>(
        dev: &cpal::Device,
        config: &cpal::StreamConfig,
        ch: usize,
        mut render: impl FnMut(usize, &mut dyn FnMut(usize, f32, f32)) + Send + 'static,
    ) -> Result<cpal::Stream, String> {
        dev.build_output_stream(
            config,
            move |data: &mut [T], _| {
                let frames = data.len() / ch;
                render(frames, &mut |i, a, b| {
                    let f = &mut data[i * ch..i * ch + ch];
                    if ch == 1 {
                        f[0] = T::from_sample(0.5 * (a + b));
                    } else {
                        f[0] = T::from_sample(a);
                        f[1] = T::from_sample(b);
                        for x in &mut f[2..] {
                            *x = T::from_sample(0.0);
                        }
                    }
                });
            },
            |e| eprintln!("audio output error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
    }
    let render = move |frames: usize, put: &mut dyn FnMut(usize, f32, f32)| {
        let mut off = 0;
        while off < frames {
            let n = (frames - off).min(MAX_BLOCK);
            fill(&mut l[..n], &mut r[..n]);
            for i in 0..n {
                put(off + i, l[i], r[i]);
            }
            off += n;
        }
    };
    let stream = match fmt {
        cpal::SampleFormat::F32 => build::<f32>(dev, &config, ch, render),
        cpal::SampleFormat::I16 => build::<i16>(dev, &config, ch, render),
        cpal::SampleFormat::I32 => build::<i32>(dev, &config, ch, render),
        cpal::SampleFormat::U16 => build::<u16>(dev, &config, ch, render),
        f => return Err(format!("unsupported output format {f:?}")),
    }?;
    Ok((stream, fs, ch))
}

fn input_stream(dev: &cpal::Device, rate: u32, channel: usize, fifo: Arc<Fifo>) -> Result<cpal::Stream, String> {
    // the input must run at the output's rate (WASAPI shared mode doesn't resample for us)
    let cfg = dev
        .supported_input_configs()
        .map_err(|e| e.to_string())?
        .filter(|c| c.min_sample_rate().0 <= rate && c.max_sample_rate().0 >= rate)
        .max_by_key(|c| (c.sample_format() == cpal::SampleFormat::F32, c.channels()))
        .map(|c| c.with_sample_rate(cpal::SampleRate(rate)))
        .ok_or_else(|| format!("the input device can't run at {} kHz like the output — set both to the same rate in Windows sound settings", rate as f32 / 1000.0))?;
    let fmt = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    let ch = config.channels as usize;
    let pick = channel.min(ch - 1);
    fn build<T: SizedSample>(dev: &cpal::Device, config: &cpal::StreamConfig, ch: usize, pick: usize, fifo: Arc<Fifo>) -> Result<cpal::Stream, String>
    where
        f32: FromSample<T>,
    {
        dev.build_input_stream(
            config,
            move |data: &[T], _| {
                for f in data.chunks(ch) {
                    fifo.push(<f32 as FromSample<T>>::from_sample_(f[pick]));
                }
            },
            |e| eprintln!("audio input error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
    }
    match fmt {
        cpal::SampleFormat::F32 => build::<f32>(dev, &config, ch, pick, fifo),
        cpal::SampleFormat::I16 => build::<i16>(dev, &config, ch, pick, fifo),
        cpal::SampleFormat::I32 => build::<i32>(dev, &config, ch, pick, fifo),
        cpal::SampleFormat::U16 => build::<u16>(dev, &config, ch, pick, fifo),
        f => Err(format!("unsupported input format {f:?}")),
    }
}

fn run_wasapi(s: &AudioSettings, params: Arc<TubeParams>, shared: Arc<Shared>, rx: Receiver<Cmd>, done: &Sender<Result<String, String>>, running: &AtomicBool) -> Result<(), String> {
    let host = cpal::default_host();
    let out_dev = find_device(&host, &s.output_device, false).ok_or("no output device")?;
    let in_dev = find_device(&host, &s.input_device, true).ok_or("no input device")?;
    let fs = out_dev.default_output_config().map_err(|e| e.to_string())?.sample_rate().0;
    let fifo = Arc::new(Fifo::new(1 << 16));
    let mut process = engine_fn(&params, &shared, fs as f64);
    let f2 = fifo.clone();
    let (out, fs, _) = output_stream(&out_dev, move |l, r| {
        let n = l.len();
        // keep the input→output latency bounded: drop input that piled up beyond a few blocks
        let have = f2.len();
        if have > n * 3 + 1024 {
            f2.skip(have - n * 2);
        }
        for v in l.iter_mut() {
            *v = if f2.len() > 0 { f2.pop() } else { 0.0 };
        }
        process(l, r);
    })?;
    let inp = input_stream(&in_dev, fs, s.input_channel, fifo)?;
    inp.play().map_err(|e| e.to_string())?;
    out.play().map_err(|e| e.to_string())?;
    running.store(true, Ordering::Relaxed);
    let names = (in_dev.name().unwrap_or_default(), out_dev.name().unwrap_or_default());
    let _ = done.send(Ok(format!("WASAPI · {} → {} · {:.1} kHz", names.0, names.1, fs as f32 / 1000.0)));
    control_loop(&rx, &shared, || true, || {});
    drop(inp);
    drop(out);
    Ok(())
}

// ------------------------------------------------------------------ DI file

fn run_file(s: &AudioSettings, params: Arc<TubeParams>, shared: Arc<Shared>, rx: Receiver<Cmd>, done: &Sender<Result<String, String>>, running: &AtomicBool) -> Result<(), String> {
    if s.di_file.is_empty() {
        return Err("no DI file chosen — open ⚙ Settings".into());
    }
    let mut reader = hound::WavReader::open(&s.di_file).map_err(|e| format!("couldn't open the DI file: {e}"))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|v| v.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|v| v.unwrap_or(0) as f32 * scale).collect()
        }
    };
    let mono: Vec<f32> = samples.chunks(ch).map(|f| f[0]).collect();
    if mono.is_empty() {
        return Err("the DI file is empty".into());
    }
    let host = cpal::default_host();
    let out_dev = find_device(&host, &s.output_device, false).ok_or("no output device")?;
    let fs = out_dev.default_output_config().map_err(|e| e.to_string())?.sample_rate().0;
    let di = crate::dsp::convolver::resample(&mono, spec.sample_rate, fs);
    let mut process = engine_fn(&params, &shared, fs as f64);
    let mut pos = 0usize;
    let (out, _, _) = output_stream(&out_dev, move |l, r| {
        for v in l.iter_mut() {
            *v = di[pos];
            pos = (pos + 1) % di.len();
        }
        process(l, r);
    })?;
    out.play().map_err(|e| e.to_string())?;
    running.store(true, Ordering::Relaxed);
    let name = std::path::Path::new(&s.di_file).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let _ = done.send(Ok(format!("DI FILE · {name} (looping) · {:.1} kHz", fs as f32 / 1000.0)));
    control_loop(&rx, &shared, || true, || {});
    drop(out);
    Ok(())
}
