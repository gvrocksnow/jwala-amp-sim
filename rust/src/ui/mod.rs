//! GPU editor: a baseview window (parented by the host, or by nih-plug's standalone wrapper)
//! rendered with wgpu. Immediate mode: every frame draws the whole UI and records hit regions;
//! input is matched against the regions of the previous frame.

mod assets;
mod gpu;
mod pages;
mod painter;
mod text;

use crate::dsp::tuner::Tuner;
use crate::params::{Knob, TubeParams, EDITOR_MIN};
use crate::plugin::Shared;
use crate::presets;
use atomic_float::AtomicF32;
use baseview::{Event, EventStatus, MouseButton, MouseEvent, Window, WindowEvent, WindowHandler, WindowOpenOptions, WindowScalePolicy};
use nih_plug::prelude::*;
use painter::{Painter, Rect};
use raw_window_handle::{HasRawDisplayHandle, HasRawWindowHandle, RawWindowHandle};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub fn create_editor(params: Arc<TubeParams>, shared: Arc<Shared>) -> Option<Box<dyn Editor>> {
    Some(Box::new(TubeEditor { params, shared, scale: Arc::new(AtomicF32::new(0.0)) }))
}

struct TubeEditor {
    params: Arc<TubeParams>,
    shared: Arc<Shared>,
    /// Scale factor set by the host; 0 = ask the system.
    scale: Arc<AtomicF32>,
}

struct Handle(baseview::WindowHandle);
// SAFETY: the handle is only used to close the window, which baseview supports from any thread.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct Parent(ParentWindowHandle);
unsafe impl HasRawWindowHandle for Parent {
    fn raw_window_handle(&self) -> RawWindowHandle {
        self.0.raw_window_handle()
    }
}

impl Editor for TubeEditor {
    fn spawn(&self, parent: ParentWindowHandle, context: Arc<dyn GuiContext>) -> Box<dyn std::any::Any + Send> {
        // decode the images while the renderer sets up the GPU (the renderer waits for them if they aren't done)
        std::thread::spawn(|| {
            assets::images();
        });
        let (w, h) = self.params.persisted.read().unwrap().editor_size;
        let host_scale = self.scale.load(Ordering::Relaxed);
        let (params, shared) = (self.params.clone(), self.shared.clone());
        let handle = Window::open_parented(
            &Parent(parent),
            WindowOpenOptions {
                title: "Jwala".into(),
                size: baseview::Size::new(w as f64, h as f64),
                scale: if host_scale > 0.0 { WindowScalePolicy::ScaleFactor(host_scale as f64) } else { WindowScalePolicy::SystemScaleFactor },
                gl_config: None,
            },
            move |window: &mut Window| App::new(window, params, shared, context, (w, h), if host_scale > 0.0 { host_scale } else { 1.0 }),
        );
        Box::new(Handle(handle))
    }

    fn size(&self) -> (u32, u32) {
        self.params.persisted.read().unwrap().editor_size
    }

    fn set_scale_factor(&self, factor: f32) -> bool {
        self.scale.store(factor, Ordering::Relaxed);
        true
    }

    fn param_value_changed(&self, _id: &str, _normalized_value: f32) {}
    fn param_modulation_changed(&self, _id: &str, _modulation_offset: f32) {}
    fn param_values_changed(&self) {}
}

/// Something clickable, recorded while drawing.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Knob(Knob),
    /// A graphic EQ slider (its track is in `App::fader_tracks`).
    Fader(Knob),
    EqOn,
    EqFlat,
    /// Bypass footswitch: (post page?, effect index).
    Stomp(bool, usize),
    Tap,
    Tab(usize),
    PresetPrev,
    PresetNext,
    PresetMenu,
    Save,
    Tuner,
    Channel(u8),
    Bright,
    /// The acoustic channel's DECAY / ATTACK / DYNAMIC switch.
    Acoustic(u8),
    CabOn,
    CabModel(u8),
    LoadIr,
    Mic,
    RefDown,
    RefUp,
    TunerMute,
    TunerClose,
    MenuItem(usize),
    MenuDelete,
    /// Swallows clicks (menu/dialog backgrounds).
    Block,
    DialogOk,
    DialogCancel,
    Resize,
    /// Standalone only: audio on/off, settings page and its controls.
    Power,
    Gear,
    SetDriver(u8),
    Cycle(Field, i8),
    AsioPanel,
    ChooseDi,
    ApplyAudio,
    CloseSettings,
}

/// A cycling selector on the audio settings page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    AsioDriver,
    AsioInput,
    WasapiIn,
    WasapiChannel,
    WasapiOut,
}

/// The standalone's audio host and its settings page state.
pub struct Audio {
    pub host: crate::standalone::AudioHost,
    pub settings: crate::standalone::AudioSettings,
    /// Edited copy while the settings page is open.
    pub pending: Option<crate::standalone::AudioSettings>,
    pub devices: crate::standalone::Devices,
    pub asio_inputs: Vec<String>,
    pub error: Option<String>,
}

enum Drag {
    Knob { knob: Knob, start: (f32, f32), value: f32 },
    Mic,
    /// `grab`: where on the thumb it was picked up, so it doesn't jump.
    Fader { knob: Knob, track: Rect, grab: f32 },
    Resize { start: (f32, f32), size: (u32, u32) },
    Press,
}

pub struct Menu {
    pub items: Vec<String>,
    pub user_from: usize,
    pub scroll: usize,
}

pub struct Dialog {
    pub text: String,
}

pub struct App {
    pub params: Arc<TubeParams>,
    pub shared: Arc<Shared>,
    ctx: Arc<dyn GuiContext>,
    /// `None` if neither a GPU nor the software renderer could start: the window stays blank
    /// (the audio keeps running) instead of crashing the app or the host.
    renderer: Option<gpu::Renderer>,
    atlas: text::Atlas,
    pub page: usize,
    pub tuner_open: bool,
    pub tuner: Tuner,
    pub needle: f64,
    pub strobe_phase: f64,
    last_frame: Instant,
    pub mouse: (f32, f32),
    pub hits: Vec<(Rect, Hit)>,
    /// Travel of each EQ slider's thumb centre (top = max), recorded while drawing.
    pub fader_tracks: Vec<(Knob, Rect)>,
    pub hot: Option<Hit>,
    pub pressed: Option<Hit>,
    drag: Option<Drag>,
    last_click: Option<(Instant, Hit)>,
    pub menu: Option<Menu>,
    pub dialog: Option<Dialog>,
    pub preset_name: String,
    pub dirty: bool,
    pub message: Option<(String, Instant)>,
    last_tap: Option<Instant>,
    save_at: Option<Instant>,
    restored: bool,
    shift: bool,
    hwnd: isize,
    /// Standalone only.
    pub audio: Option<Audio>,
    /// Redraw on the next frame tick regardless of `last_sig` (input, resize).
    force_redraw: bool,
    /// The window's DPI scale (physical / logical pixels).
    dpi: f32,
    last_sig: u64,
    last_meters: u64,
    /// When the resize grip last asked the host/standalone frame for a new size.
    resize_requested: Option<Instant>,
    /// The logical size last applied to our window.
    applied_size: (u32, u32),
    /// A file dialog open on its own thread, and where its answer arrives (see `pick_file`).
    picker: Option<(Pick, mpsc::Receiver<Option<String>>)>,
}

/// What an open file dialog is choosing.
#[derive(Clone, Copy)]
enum Pick {
    Ir,
    Di,
}

impl App {
    fn new(window: &mut Window, params: Arc<TubeParams>, shared: Arc<Shared>, ctx: Arc<dyn GuiContext>, size: (u32, u32), scale: f32) -> App {
        let hwnd = match window.raw_window_handle() {
            RawWindowHandle::Win32(h) => h.hwnd as isize,
            _ => 0,
        };
        let phys = ((size.0 as f32 * scale) as u32, (size.1 as f32 * scale) as u32);
        let target = gpu::target(window.raw_window_handle(), window.raw_display_handle());
        let renderer = match target.and_then(|t| gpu::Renderer::new(t, phys, scale)) {
            Ok(r) => Some(r),
            Err(e) => {
                renderer_failed(&e);
                None
            }
        };
        let preset_name = params.persisted.read().unwrap().preset.clone();
        App {
            params,
            shared,
            ctx,
            renderer,
            atlas: text::Atlas::default(),
            page: 0,
            tuner_open: false,
            tuner: Tuner::default(),
            needle: 0.0,
            strobe_phase: 0.0,
            last_frame: Instant::now(),
            mouse: (-1.0, -1.0),
            hits: Vec::new(),
            fader_tracks: Vec::new(),
            hot: None,
            pressed: None,
            drag: None,
            last_click: None,
            menu: None,
            dialog: None,
            preset_name: if preset_name.is_empty() { "Classic Crunch".into() } else { preset_name },
            dirty: false,
            message: None,
            last_tap: None,
            save_at: None,
            restored: false,
            shift: false,
            hwnd,
            audio: None,
            resize_requested: None,
            force_redraw: true,
            dpi: scale,
            last_sig: 0,
            last_meters: 0,
            applied_size: size,
            picker: None,
        }
        .with_page()
    }

    fn with_page(mut self) -> Self {
        self.page = load_ui_state().page.min(4);
        self
    }

    /// Everything (besides input, which forces a redraw) that can change what is on screen.
    fn meter_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for m in [&self.shared.meter_in, &self.shared.meter_l, &self.shared.meter_r] {
            ((20.0 * (m.load(Ordering::Relaxed) + 1e-6).log10() / 3.75).floor() as i32).hash(&mut h);
        }
        ((self.shared.cpu.load(Ordering::Relaxed) * 100.0).round() as i32).hash(&mut h);
        h.finish()
    }

    /// `with_meters`: include the level meters and CPU readout.
    fn visual_signature(&self, with_meters: bool) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let st = self.params.snapshot();
        for v in [st.input, st.gate, st.output, st.amp.gain, st.amp.bass, st.amp.middle, st.amp.treble, st.amp.presence, st.amp.depth, st.amp.master] {
            v.to_bits().hash(&mut h);
        }
        (st.amp.channel, st.amp.bright, st.cab.enabled, st.cab.model).hash(&mut h);
        for v in [st.cab.mic_pos, st.cab.distance, st.cab.low_cut, st.cab.high_cut].iter().chain(st.eq.p.iter()) {
            v.to_bits().hash(&mut h);
        }
        st.eq.on.hash(&mut h);
        for fx in st.pre.iter().chain(st.post.iter()) {
            fx.on.hash(&mut h);
            for v in fx.p {
                v.to_bits().hash(&mut h);
            }
        }
        // meters and CPU quantised to what the display can show
        if with_meters {
            self.meter_signature().hash(&mut h);
        }
        if let Some(a) = &self.audio {
            (a.host.running(), a.host.status(), a.error.clone(), a.pending.as_ref().map(|p| format!("{p:?}")), a.asio_inputs.len()).hash(&mut h);
        }
        (self.page, self.tuner_open, self.preset_name.as_str(), self.dirty).hash(&mut h);
        (format!("{:?}{:?}", self.hot, self.pressed), self.menu.as_ref().map(|m| (m.items.len(), m.scroll))).hash(&mut h);
        (self.dialog.as_ref().map(|d| d.text.clone()), self.message.as_ref().map(|m| m.0.clone())).hash(&mut h);
        self.params.persisted.read().unwrap().editor_size.hash(&mut h);
        h.finish()
    }

    /// Something on screen moves by itself: the tuner, the tap-tempo LED, the dialog's caret.
    fn animating(&self) -> bool {
        self.tuner_open || self.dialog.is_some() || (self.page == 4 && self.params.post[3].on.value())
    }

    /// Drawing with Windows' software renderer (no usable GPU).
    pub fn software_render(&self) -> bool {
        self.renderer.as_ref().is_some_and(|r| r.software)
    }

    /// Pixel size of an image asset (1x1 if missing).
    pub fn tex_size(&self, name: &str) -> (f32, f32) {
        let t = self.tex(name);
        self.renderer.as_ref().and_then(|r| r.tex_sizes.get(t.0).copied()).map(|(w, h)| (w as f32, h as f32)).unwrap_or((1.0, 1.0))
    }

    pub fn tex(&self, name: &str) -> painter::TexId {
        self.renderer.as_ref().map(|r| r.tex(name)).unwrap_or(painter::TexId::WHITE)
    }

    /// The layout size: the window's, or while the resize grip is dragged, the size being dragged to
    /// (the window is then temporarily larger, see `follow_size`).
    pub fn size(&self) -> (f32, f32) {
        let (w, h) = self.renderer.as_ref().map(|r| r.logical_size()).unwrap_or((1.0, 1.0));
        let want = self.params.persisted.read().unwrap().editor_size;
        (w.min(want.0 as f32), h.min(want.1 as f32))
    }

    pub fn flash(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), Instant::now()));
    }

    // ------------------------------------------------------------------ parameters

    fn setter(&self) -> ParamSetter<'_> {
        ParamSetter::new(self.ctx.as_ref())
    }

    pub fn set_bool(&self, p: &BoolParam, v: bool) {
        let s = self.setter();
        s.begin_set_parameter(p);
        s.set_parameter(p, v);
        s.end_set_parameter(p);
    }

    pub fn set_int(&self, p: &IntParam, v: i32) {
        let s = self.setter();
        s.begin_set_parameter(p);
        s.set_parameter(p, v);
        s.end_set_parameter(p);
    }

    fn set_knob(&mut self, k: Knob, v: f32) {
        let p = self.params.knob(k);
        self.setter().set_parameter(p, v.clamp(0.0, 1.0));
        self.changed();
    }

    fn knob_once(&mut self, k: Knob, v: f32) {
        let p = self.params.knob(k);
        let s = self.setter();
        s.begin_set_parameter(p);
        s.set_parameter(p, v.clamp(0.0, 1.0));
        s.end_set_parameter(p);
        self.changed();
    }

    /// Sets a parameter by preset key to a plain value.
    fn set_key(&self, key: &str, plain: f32) {
        if let Some(ptr) = self.params.ptr(key) {
            unsafe {
                let norm = ptr.preview_normalized(plain);
                self.ctx.raw_begin_set_parameter(ptr);
                self.ctx.raw_set_parameter_normalized(ptr, norm);
                self.ctx.raw_end_set_parameter(ptr);
            }
        }
    }

    /// Something the user changed: mark the preset as edited and schedule a save (standalone).
    pub fn changed(&mut self) {
        self.dirty = true;
        self.save_at = Some(Instant::now() + Duration::from_millis(600));
    }

    /// Applies a full state: everything missing goes to its default (effects off).
    /// Presets leave the global input/gate/output alone unless `globals` is set.
    fn apply_state(&mut self, st: &presets::State, globals: bool) {
        let mut full = TubeParams::default().to_map();
        if !globals {
            full.retain(|k, _| !k.starts_with("global."));
        }
        for (k, v) in st {
            if globals || !k.starts_with("global.") {
                full.insert(k.clone(), *v);
            }
        }
        if full.get("cab.MODEL").copied() == Some(3.0) && self.shared.user_ir.lock().unwrap().is_none() {
            full.insert("cab.MODEL".into(), 0.0);
        }
        for (k, v) in &full {
            self.set_key(k, *v);
        }
    }

    pub fn apply_preset(&mut self, name: &str) {
        if let Some(st) = presets::get(name) {
            self.apply_state(&st, false);
            self.preset_name = name.to_string();
            self.dirty = false;
            self.params.persisted.write().unwrap().preset = name.to_string();
            self.save_at = Some(Instant::now() + Duration::from_millis(600));
        }
    }

    pub fn step_preset(&mut self, dir: i32) {
        let all = presets::all_names();
        let i = all.iter().position(|n| *n == self.preset_name).map(|i| i as i32 + dir).unwrap_or(0);
        let n = all.len() as i32;
        let name = all[(((i % n) + n) % n) as usize].clone();
        self.apply_preset(&name);
    }

    pub fn tap(&mut self) {
        let now = Instant::now();
        if let Some(prev) = self.last_tap {
            let dt = now.duration_since(prev).as_secs_f64() * 1000.0;
            if dt > 60.0 && dt < 1800.0 {
                // inverse of spec::delay_time_ms
                let v = ((dt / 30.0).ln() / (1800.0f64 / 30.0).ln()) as f32;
                self.knob_once(Knob::Post(3, 0), v);
            }
        }
        self.last_tap = Some(now);
    }

    /// Opens a .wav file dialog on its own thread; `poll_picker` handles the answer. A dialog run
    /// from inside an event handler would pump this window's messages in its modal loop, and
    /// baseview's re-entered handler panics (the "already borrowed" abort that took hosts down).
    fn pick_file(&mut self, kind: Pick, title: &'static str, filter: &'static str) {
        if self.picker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let path = rfd::FileDialog::new().set_title(title).add_filter(filter, &["wav"]).pick_file();
            let _ = tx.send(path.map(|p| p.to_string_lossy().into_owned()));
        });
        self.picker = Some((kind, rx));
    }

    fn poll_picker(&mut self) {
        let Some((kind, rx)) = &self.picker else {
            return;
        };
        let kind = *kind;
        let path = match rx.try_recv() {
            Ok(p) => p,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.picker = None;
        self.force_redraw = true;
        let Some(path) = path else {
            return;
        };
        match kind {
            Pick::Ir => self.load_ir(path),
            Pick::Di => {
                if let Some(p) = self.audio.as_mut().and_then(|a| a.pending.as_mut()) {
                    p.di_file = path;
                }
            }
        }
    }

    fn load_ir(&mut self, path: String) {
        match self.shared.load_user_ir(&path) {
            Ok(()) => {
                self.params.persisted.write().unwrap().ir_path = path;
                self.set_int(&self.params.cab_model, 3);
                self.changed();
            }
            Err(e) => self.flash(format!("Couldn't load IR: {e}")),
        }
    }

    // ------------------------------------------------------------------ standalone persistence

    fn restore_standalone(&mut self) {
        self.restored = true;
        if !self.shared.standalone.load(Ordering::Relaxed) {
            return;
        }
        set_window_icon(self.hwnd);
        let saved = load_ui_state();
        if !saved.ir_path.is_empty() && self.shared.user_ir.lock().unwrap().is_none() && self.shared.load_user_ir(&saved.ir_path).is_ok() {
            self.params.persisted.write().unwrap().ir_path = saved.ir_path.clone();
        }
        if let Some(st) = saved.state {
            self.apply_state(&st, true);
        } else if let Some(st) = presets::get(&self.preset_name) {
            self.apply_state(&st, false);
        }
        if !saved.preset.is_empty() {
            self.preset_name = saved.preset;
        }
        self.dirty = false;
        // power is on by default: start audio with the last device
        let settings = saved.audio.unwrap_or_else(crate::standalone::AudioSettings::first_run);
        self.audio = Some(Audio { host: Default::default(), settings, pending: None, devices: Default::default(), asio_inputs: Vec::new(), error: None });
        if !crate::NO_AUDIO.load(Ordering::Relaxed) {
            self.start_audio();
        }
    }

    pub fn audio_running(&self) -> bool {
        self.audio.as_ref().is_some_and(|a| a.host.running())
    }

    fn start_audio(&mut self) {
        let (params, shared) = (self.params.clone(), self.shared.clone());
        let Some(a) = self.audio.as_mut() else { return };
        a.error = a.host.start(&a.settings, params, shared).err();
        if let Some(e) = a.error.clone() {
            self.flash(e);
        }
    }

    fn toggle_power(&mut self) {
        if self.audio_running() {
            if let Some(a) = self.audio.as_mut() {
                a.host.stop();
            }
        } else {
            self.start_audio();
        }
    }

    fn open_settings(&mut self) {
        let Some(a) = self.audio.as_mut() else { return };
        a.devices = crate::standalone::devices();
        let mut p = a.settings.clone();
        if p.asio_driver.is_empty() {
            p.asio_driver = a.devices.asio.first().cloned().unwrap_or_default();
        }
        a.asio_inputs = if p.asio_driver.is_empty() { Vec::new() } else { a.host.asio_inputs(&p.asio_driver) };
        a.pending = Some(p);
    }

    fn cycle(&mut self, f: Field, dir: i8) {
        let Some(a) = self.audio.as_mut() else { return };
        let Some(p) = a.pending.as_mut() else { return };
        fn step(list: &[String], cur: &str, dir: i8) -> Option<String> {
            if list.is_empty() {
                return None;
            }
            let n = list.len() as i32;
            let i = list.iter().position(|x| x == cur).map(|i| i as i32 + dir as i32).unwrap_or(0);
            Some(list[(((i % n) + n) % n) as usize].clone())
        }
        match f {
            Field::AsioDriver => {
                if let Some(n) = step(&a.devices.asio, &p.asio_driver, dir) {
                    p.asio_driver = n;
                    p.input_channel = 0;
                    a.asio_inputs = a.host.asio_inputs(&p.asio_driver);
                }
            }
            Field::AsioInput | Field::WasapiChannel => {
                let count = if f == Field::AsioInput {
                    a.asio_inputs.len()
                } else {
                    a.devices.inputs.iter().find(|(n, _)| *n == p.input_device).or(a.devices.inputs.first()).map(|(_, c)| *c).unwrap_or(2)
                };
                if count > 0 {
                    p.input_channel = (p.input_channel as i32 + dir as i32).rem_euclid(count as i32) as usize;
                }
            }
            Field::WasapiIn => {
                let names: Vec<String> = a.devices.inputs.iter().map(|(n, _)| n.clone()).collect();
                if let Some(n) = step(&names, &p.input_device, dir) {
                    p.input_device = n;
                    p.input_channel = 0;
                }
            }
            Field::WasapiOut => {
                if let Some(n) = step(&a.devices.outputs, &p.output_device, dir) {
                    p.output_device = n;
                }
            }
        }
    }

    fn apply_audio(&mut self) {
        let Some(a) = self.audio.as_mut() else { return };
        let Some(p) = a.pending.clone() else { return };
        a.settings = p;
        self.start_audio();
        let ok = self.audio_running();
        if let Some(a) = self.audio.as_mut() {
            if ok {
                a.pending = None;
            }
        }
        self.save_at = Some(Instant::now());
    }

    fn save_now(&mut self) {
        self.save_at = None;
        let ir = self.params.persisted.read().unwrap().ir_path.clone();
        save_ui_state(&UiState {
            state: self.shared.standalone.load(Ordering::Relaxed).then(|| self.params.to_map()),
            preset: self.preset_name.clone(),
            ir_path: ir,
            page: self.page,
            audio: self.audio.as_ref().map(|a| a.settings.clone()),
        });
    }

    // ------------------------------------------------------------------ input

    fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h)
    }

    fn mouse_down(&mut self) {
        let (x, y) = self.mouse;
        let hit = self.hit_at(x, y);
        if self.menu.is_some() && !matches!(hit, Some(Hit::MenuItem(_) | Hit::MenuDelete | Hit::Block)) {
            self.menu = None;
            return;
        }
        let Some(hit) = hit else { return };
        let now = Instant::now();
        let double = matches!(self.last_click, Some((t, h)) if h == hit && now.duration_since(t) < Duration::from_millis(400));
        self.last_click = Some((now, hit));
        self.pressed = Some(hit);
        let p = self.params.clone();
        match hit {
            Hit::Knob(k) => {
                if double {
                    self.knob_once(k, p.knob_spec(k).default);
                    self.last_click = None;
                    return;
                }
                let param = p.knob(k);
                self.setter().begin_set_parameter(param);
                self.drag = Some(Drag::Knob { knob: k, start: (x, y), value: param.value() });
            }
            Hit::Fader(k) => {
                if double {
                    self.knob_once(k, p.knob_spec(k).default);
                    self.last_click = None;
                    return;
                }
                let Some(track) = self.fader_tracks.iter().find(|(f, _)| *f == k).map(|(_, r)| *r) else { return };
                let param = p.knob(k);
                let thumb_y = track.bottom() - param.value() * track.h;
                // grabbed by the thumb: move relative to it; anywhere else on the slot: jump there
                let grab = if (y - thumb_y).abs() <= pages::FADER_THUMB_H / 2.0 + 2.0 { y - thumb_y } else { 0.0 };
                self.setter().begin_set_parameter(param);
                self.drag = Some(Drag::Fader { knob: k, track, grab });
                self.mouse_move(x, y);
            }
            Hit::EqOn => {
                self.set_bool(&p.eq_on, !p.eq_on.value());
                self.changed();
            }
            Hit::EqFlat => {
                for i in 0..crate::spec::EQ_BANDS {
                    self.knob_once(Knob::Eq(i), 0.5);
                }
            }
            Hit::Stomp(post, i) => {
                let fx = if post { &p.post[i] } else { &p.pre[i] };
                self.set_bool(&fx.on, !fx.on.value());
                self.changed();
                self.drag = Some(Drag::Press);
            }
            Hit::Tap => {
                self.tap();
                self.drag = Some(Drag::Press);
            }
            Hit::Tab(i) => {
                self.page = i;
                self.set_tuner(false);
                self.save_at = Some(Instant::now() + Duration::from_millis(600));
            }
            Hit::PresetPrev => self.step_preset(-1),
            Hit::PresetNext => self.step_preset(1),
            Hit::PresetMenu => {
                let mut items: Vec<String> = presets::FACTORY.iter().map(|(n, _)| n.to_string()).collect();
                let user_from = items.len();
                items.extend(presets::user_names());
                self.menu = Some(Menu { items, user_from, scroll: 0 });
            }
            Hit::Save => {
                let n = if presets::is_factory(&self.preset_name) { format!("{} (mine)", self.preset_name) } else { self.preset_name.clone() };
                self.dialog = Some(Dialog { text: n });
            }
            Hit::Tuner => self.set_tuner(!self.tuner_open),
            Hit::Channel(c) => {
                self.set_int(&p.channel, c as i32);
                self.changed();
            }
            Hit::Bright => {
                self.set_bool(&p.bright, !p.bright.value());
                self.changed();
            }
            Hit::Acoustic(i) => {
                let sw = [&p.decay, &p.attack, &p.dynamic][i as usize];
                self.set_bool(sw, !sw.value());
                self.changed();
            }
            Hit::CabOn => {
                self.set_bool(&p.cab_on, !p.cab_on.value());
                self.changed();
            }
            Hit::CabModel(m) => {
                if m == 3 && self.shared.user_ir.lock().unwrap().is_none() {
                    self.pick_file(Pick::Ir, "Load a cabinet impulse response", "Impulse responses");
                } else {
                    self.set_int(&p.cab_model, m as i32);
                    self.changed();
                }
            }
            Hit::LoadIr => self.pick_file(Pick::Ir, "Load a cabinet impulse response", "Impulse responses"),
            Hit::Mic => {
                self.setter().begin_set_parameter(&p.cab[0]);
                self.drag = Some(Drag::Mic);
                self.mouse_move(x, y);
            }
            Hit::RefDown => self.tuner.reference_a = (self.tuner.reference_a - 1.0).max(425.0),
            Hit::RefUp => self.tuner.reference_a = (self.tuner.reference_a + 1.0).min(455.0),
            Hit::TunerMute => {
                let m = &self.shared.tuner.mute_output;
                m.store(!m.load(Ordering::Relaxed), Ordering::Relaxed);
            }
            Hit::TunerClose => self.set_tuner(false),
            Hit::MenuItem(i) => {
                if let Some(menu) = self.menu.take() {
                    if let Some(name) = menu.items.get(i) {
                        self.apply_preset(&name.clone());
                    }
                }
            }
            Hit::MenuDelete => {
                self.menu = None;
                presets::delete_user(&self.preset_name);
                self.dirty = true;
                self.flash(format!("Deleted preset \"{}\"", self.preset_name));
            }
            Hit::Block => {}
            Hit::DialogOk => self.commit_dialog(),
            Hit::DialogCancel => self.dialog = None,
            Hit::Power => self.toggle_power(),
            Hit::Gear => self.open_settings(),
            Hit::SetDriver(d) => {
                if let Some(p) = self.audio.as_mut().and_then(|a| a.pending.as_mut()) {
                    p.driver = [crate::standalone::Driver::Asio, crate::standalone::Driver::Wasapi, crate::standalone::Driver::DiFile][d as usize];
                    p.input_channel = 0;
                }
            }
            Hit::Cycle(f, dir) => self.cycle(f, dir),
            Hit::AsioPanel => {
                if let Some(a) = &self.audio {
                    a.host.control_panel();
                }
            }
            Hit::ChooseDi => self.pick_file(Pick::Di, "Load a dry DI guitar recording", "WAV audio"),
            Hit::ApplyAudio => self.apply_audio(),
            Hit::CloseSettings => {
                if let Some(a) = self.audio.as_mut() {
                    a.pending = None;
                }
            }
            Hit::Resize => {
                let size = self.params.persisted.read().unwrap().editor_size;
                self.drag = Some(Drag::Resize { start: (x, y), size });
            }
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) {
        self.mouse = (x, y);
        match self.drag {
            Some(Drag::Knob { knob, start, value }) => {
                let sens = if self.shift { 0.0012 } else { 0.005 };
                self.set_knob(knob, value + ((start.1 - y) + (x - start.0) * 0.5) * sens);
            }
            Some(Drag::Fader { knob, track, grab }) => {
                let v = (track.bottom() - (y - grab)) / track.h.max(1.0);
                self.set_knob(knob, v);
            }
            Some(Drag::Mic) => {
                if let Some(v) = pages::mic_value(self, x) {
                    let p = self.params.clone();
                    self.setter().set_parameter(&p.cab[0], v);
                    self.changed();
                }
            }
            Some(Drag::Resize { start, size }) => {
                let w = ((size.0 as f32 + x - start.0) as u32).max(EDITOR_MIN.0);
                let h = ((size.1 as f32 + y - start.1) as u32).max(EDITOR_MIN.1);
                let cur = self.params.persisted.read().unwrap().editor_size;
                if (w, h) != cur {
                    self.params.persisted.write().unwrap().editor_size = (w, h);
                    self.ctx.request_resize();
                    self.resize_requested = Some(Instant::now());
                }
            }
            _ => self.hot = self.hit_at(x, y),
        }
    }

    fn mouse_up(&mut self) {
        let p = self.params.clone();
        match self.drag.take() {
            Some(Drag::Knob { knob, .. } | Drag::Fader { knob, .. }) => self.setter().end_set_parameter(p.knob(knob)),
            Some(Drag::Mic) => self.setter().end_set_parameter(&p.cab[0]),
            _ => {}
        }
        self.pressed = None;
        self.hot = self.hit_at(self.mouse.0, self.mouse.1);
    }

    fn wheel(&mut self, dy: f32) {
        if let Some(menu) = &mut self.menu {
            menu.scroll = if dy < 0.0 { (menu.scroll + 1).min(menu.items.len().saturating_sub(1)) } else { menu.scroll.saturating_sub(1) };
            return;
        }
        if let Some(Hit::Knob(k) | Hit::Fader(k)) = self.hit_at(self.mouse.0, self.mouse.1) {
            let step = if self.shift { 0.005 } else { 0.02 };
            let v = self.params.knob(k).value() + dy.signum() * step;
            self.knob_once(k, v);
        }
    }

    pub fn set_tuner(&mut self, on: bool) {
        self.tuner_open = on;
        self.shared.tuner.active.store(on, Ordering::Relaxed);
    }

    fn commit_dialog(&mut self) {
        let Some(d) = self.dialog.take() else { return };
        let name = presets::safe_name(&d.text);
        if name.is_empty() {
            return;
        }
        if presets::is_factory(&name) {
            self.flash("That name belongs to a factory preset — pick another.");
            return;
        }
        match presets::save_user(&name, &self.params.to_map()) {
            Ok(()) => {
                self.preset_name = name.clone();
                self.params.persisted.write().unwrap().preset = name;
                self.dirty = false;
            }
            Err(e) => self.flash(format!("Couldn't save: {e}")),
        }
    }

    fn key(&mut self, ev: &keyboard_types::KeyboardEvent) -> bool {
        use keyboard_types::{Key, KeyState, Modifiers};
        self.shift = ev.modifiers.contains(Modifiers::SHIFT);
        if ev.state != KeyState::Down {
            return false;
        }
        if let Some(d) = &mut self.dialog {
            match &ev.key {
                Key::Character(c) => d.text.push_str(c),
                Key::Backspace => {
                    d.text.pop();
                }
                Key::Enter => self.commit_dialog(),
                Key::Escape => self.dialog = None,
                _ => return false,
            }
            return true;
        }
        let ctrl = ev.modifiers.contains(Modifiers::CONTROL);
        let settings_open = self.audio.as_ref().is_some_and(|a| a.pending.is_some());
        match &ev.key {
            Key::Escape if settings_open => {
                if let Some(a) = self.audio.as_mut() {
                    a.pending = None;
                }
            }
            Key::Character(c) if c == " " && self.audio.is_some() => self.toggle_power(),
            Key::Escape if self.menu.is_some() => self.menu = None,
            Key::Escape if self.tuner_open => self.set_tuner(false),
            Key::ArrowLeft if ctrl => self.step_preset(-1),
            Key::ArrowRight if ctrl => self.step_preset(1),
            Key::Character(c) if c.eq_ignore_ascii_case("t") => self.set_tuner(!self.tuner_open),
            Key::Character(c) if ["1", "2", "3", "4", "5"].contains(&c.as_str()) => {
                self.page = c.parse::<usize>().unwrap() - 1;
                self.set_tuner(false);
            }
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------------------ frame

    fn frame(&mut self) {
        if !self.restored {
            self.restore_standalone();
        }
        if self.save_at.is_some_and(|t| Instant::now() >= t) {
            self.save_now();
        }
        if self.message.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(4)) {
            self.message = None;
        }
        if self.tuner_open {
            let fs = self.shared.sample_rate.load(Ordering::Relaxed) as f64;
            if fs > 0.0 {
                self.tuner.analyze(&self.shared.tuner, fs);
            }
        }
        // redraw only when something visible changed (or something is animating); the software
        // renderer additionally caps the frame rate, since every frame costs real CPU time
        let software = self.software_render();
        let sig = self.visual_signature(!software);
        let meters = self.meter_signature();
        let animating = self.animating();
        let since = self.last_frame.elapsed();
        let min_interval = if software { Duration::from_millis(50) } else { Duration::ZERO };
        // on the software renderer meter movement alone redraws at most 5x a second, and there is no
        // periodic refresh (each frame is real CPU work)
        let meters_due = software && meters != self.last_meters && since > Duration::from_millis(200);
        let due = self.force_redraw || sig != self.last_sig || meters_due || animating || (!software && since > Duration::from_secs(1));
        if !due || (since < min_interval && !self.force_redraw) {
            return;
        }
        self.force_redraw = false;
        self.last_sig = sig;
        self.last_meters = meters;
        let dt = since.as_secs_f64();
        self.last_frame = Instant::now();

        self.atlas.maintain();
        let Some(r) = self.renderer.as_mut() else { return };
        let size = r.logical_size();
        let scale = r.scale();
        let tex_sizes = r.tex_sizes.clone();
        let mut keys = std::mem::take(&mut r.shadow_keys);
        let mut atlas = std::mem::take(&mut self.atlas);
        let (layer, jobs) = {
            let mut p = Painter::new(size, scale, &mut atlas, &tex_sizes, &mut keys);
            self.hits.clear();
            self.fader_tracks.clear();
            pages::draw(self, &mut p, dt);
            (p.layer, p.shadow_jobs)
        };
        if let Some(r) = self.renderer.as_mut() {
            r.shadow_keys = keys;
            r.render(&layer, &jobs, &mut atlas);
        }
        self.atlas = atlas;
    }
}

impl App {
    /// Keeps our window a margin larger than the layout size. The host's or standalone's frame clips
    /// it to the requested size and is resized around us (`request_resize`) a little later than we
    /// can redraw, so when it grows it only ever uncovers area that is already painted. While the
    /// resize grip is dragged the margin grows ahead of the cursor.
    fn follow_size(&mut self, window: &mut Window) {
        // spare painted area around the layout; every extra pixel costs CPU on the software renderer
        let margin: u32 = if self.software_render() { 64 } else { 320 };
        // standalone: the window can also be resized by its border or maximised, which nobody reports
        // to us, so adopt the frame's actual size as the layout size
        if crate::STANDALONE.load(Ordering::Relaxed) && !matches!(self.drag, Some(Drag::Resize { .. })) {
            if let Some((pw, ph)) = parent_client_size(self.hwnd) {
                // the window's DPI scale, not the render scale (the software renderer draws at 1x)
                let s = self.dpi.max(0.5);
                let frame = (((pw as f32 / s).round() as u32).max(EDITOR_MIN.0), ((ph as f32 / s).round() as u32).max(EDITOR_MIN.1));
                let want = self.params.persisted.read().unwrap().editor_size;
                let close = |a: u32, b: u32| (a as i64 - b as i64).abs() <= 1;
                let caught_up = close(frame.0, want.0) && close(frame.1, want.1);
                // after a grip resize the frame follows a little later; don't mistake its old size for a new one
                if caught_up || self.resize_requested.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                    self.resize_requested = None;
                }
                if pw > 0 && ph > 0 && !caught_up && self.resize_requested.is_none() {
                    self.params.persisted.write().unwrap().editor_size = frame;
                }
            }
        }
        let want = self.params.persisted.read().unwrap().editor_size;
        let (cur_w, cur_h) = self.applied_size;
        let target = if matches!(self.drag, Some(Drag::Resize { .. })) {
            if want.0 + 150 > cur_w || want.1 + 150 > cur_h { (want.0 + 600, want.1 + 400) } else { (cur_w, cur_h) }
        } else {
            (want.0 + margin, want.1 + margin)
        };
        if target != self.applied_size {
            self.applied_size = target;
            window.resize(baseview::Size::new(target.0 as f64, target.1 as f64));
        }
    }
}

impl WindowHandler for App {
    fn on_frame(&mut self, window: &mut Window) {
        self.poll_picker();
        self.follow_size(window);
        self.frame();
    }

    fn on_event(&mut self, window: &mut Window, event: Event) -> EventStatus {
        self.force_redraw = true;
        match event {
            Event::Mouse(e) => {
                // take keyboard focus on click so the shortcuts work inside hosts and the standalone
                if matches!(e, MouseEvent::ButtonPressed { .. }) && !window.has_focus() {
                    window.focus();
                }
                // keep receiving moves while dragging outside the window (knobs, resize grip)
                match e {
                    MouseEvent::ButtonPressed { button: MouseButton::Left, .. } => capture(self.hwnd, true),
                    MouseEvent::ButtonReleased { button: MouseButton::Left, .. } => capture(self.hwnd, false),
                    _ => {}
                }
                match e {
                    MouseEvent::CursorMoved { position, modifiers } => {
                        self.shift = modifiers.contains(keyboard_types::Modifiers::SHIFT);
                        self.mouse_move(position.x as f32, position.y as f32);
                        // while dragging the grip, grow our own window right away (before the host's
                        // frame catches up) so no unpainted frame background shows through
                        self.follow_size(window);
                    }
                    MouseEvent::ButtonPressed { button: MouseButton::Left, .. } => {
                        self.mouse_down();
                        self.follow_size(window);
                    }
                    MouseEvent::ButtonReleased { button: MouseButton::Left, .. } => {
                        self.mouse_up();
                        self.follow_size(window);
                    }
                    MouseEvent::WheelScrolled { delta, .. } => {
                        let dy = match delta {
                            baseview::ScrollDelta::Lines { y, .. } => y,
                            baseview::ScrollDelta::Pixels { y, .. } => y / 40.0,
                        };
                        if dy != 0.0 {
                            self.wheel(dy);
                        }
                    }
                    MouseEvent::CursorLeft => {
                        if self.drag.is_none() {
                            self.hot = None;
                        }
                    }
                    _ => {}
                }
                EventStatus::Captured
            }
            Event::Keyboard(k) => {
                if self.key(&k) {
                    EventStatus::Captured
                } else {
                    EventStatus::Ignored
                }
            }
            Event::Window(WindowEvent::Resized(info)) => {
                let ph = info.physical_size();
                self.dpi = info.scale() as f32;
                if let Some(r) = self.renderer.as_mut() {
                    r.resize((ph.width, ph.height), info.scale() as f32);
                }
                let l = info.logical_size();
                let size = (l.width.round() as u32, l.height.round() as u32);
                let ours = (size.0 as i64 - self.applied_size.0 as i64).abs() <= 1 && (size.1 as i64 - self.applied_size.1 as i64).abs() <= 1;
                if !ours && self.drag.is_none() {
                    // the host resized us: that is the new layout size
                    self.params.persisted.write().unwrap().editor_size = size;
                    self.applied_size = size;
                }
                // redraw at the new size immediately, not on the next frame tick, so the newly
                // exposed area never shows unrendered
                self.frame();
                EventStatus::Captured
            }
            Event::Window(WindowEvent::WillClose) => {
                self.save_now();
                if let Some(a) = self.audio.as_mut() {
                    a.host.stop();
                }
                EventStatus::Captured
            }
            _ => EventStatus::Ignored,
        }
    }
}

// ------------------------------------------------------------------ UI state file

/// Saved by the standalone app (the plugin's host saves the plugin state instead).
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct UiState {
    #[serde(default)]
    state: Option<presets::State>,
    #[serde(default)]
    preset: String,
    #[serde(default)]
    ir_path: String,
    #[serde(default)]
    page: usize,
    #[serde(default)]
    audio: Option<crate::standalone::AudioSettings>,
}

fn ui_state_path() -> std::path::PathBuf {
    presets::app_dir().join("jwala-rs.json")
}

fn load_ui_state() -> UiState {
    if let Ok(t) = std::fs::read_to_string(ui_state_path()) {
        if let Ok(s) = serde_json::from_str(&t) {
            return s;
        }
    }
    // first run: pick up the C# app's last state
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct CsSettings {
        state: Option<presets::State>,
        preset: Option<String>,
        ir_file: Option<String>,
        page: Option<usize>,
    }
    std::fs::read_to_string(presets::app_dir().join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<CsSettings>(&t).ok())
        .map(|c| UiState { state: c.state, preset: c.preset.unwrap_or_default(), ir_path: c.ir_file.unwrap_or_default(), page: c.page.unwrap_or(0), audio: None })
        .unwrap_or_default()
}

fn save_ui_state(s: &UiState) {
    let _ = std::fs::create_dir_all(presets::app_dir());
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(ui_state_path(), t);
    }
}

/// Win32 mouse capture, so drags keep tracking outside the window.
#[cfg(windows)]
fn capture(hwnd: isize, on: bool) {
    #[link(name = "user32")]
    extern "system" {
        fn SetCapture(hwnd: isize) -> isize;
        fn ReleaseCapture() -> i32;
    }
    unsafe {
        if on {
            SetCapture(hwnd);
        } else {
            ReleaseCapture();
        }
    }
}

#[cfg(not(windows))]
fn capture(_hwnd: isize, _on: bool) {}

/// Client size (physical pixels) of the window our view is embedded in.
#[cfg(windows)]
fn parent_client_size(hwnd: isize) -> Option<(u32, u32)> {
    #[repr(C)]
    struct R {
        l: i32,
        t: i32,
        r: i32,
        b: i32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetParent(hwnd: isize) -> isize;
        fn GetClientRect(hwnd: isize, r: *mut R) -> i32;
    }
    unsafe {
        let parent = GetParent(hwnd);
        if parent == 0 {
            return None;
        }
        let mut r = R { l: 0, t: 0, r: 0, b: 0 };
        (GetClientRect(parent, &mut r) != 0).then(|| ((r.r - r.l).max(0) as u32, (r.b - r.t).max(0) as u32))
    }
}

#[cfg(not(windows))]
fn parent_client_size(_hwnd: isize) -> Option<(u32, u32)> {
    None
}

/// Neither a GPU nor the software renderer could start. Tell the user once; the window stays
/// blank but the app (or the host) keeps running.
fn renderer_failed(err: &str) {
    eprintln!("Jwala: no renderer available: {err}");
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, kind: u32) -> i32;
        }
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let text = w(&format!(
            "Jwala couldn't start its display: no DirectX 12 / Vulkan graphics, and the Windows software renderer (WARP) wasn't available either.\n\nThe audio keeps working; updating the graphics driver usually fixes the display.\n\nDetails: {err}"
        ));
        unsafe { MessageBoxW(0, text.as_ptr(), w("Jwala").as_ptr(), 0x30 /* MB_ICONWARNING */) };
    }
}

/// Gives the standalone's top-level window the app icon embedded by build.rs (resource id 1), for its
/// title bar and taskbar button; nih-plug's window doesn't set one.
#[cfg(windows)]
fn set_window_icon(hwnd: isize) {
    #[link(name = "user32")]
    extern "system" {
        fn GetAncestor(hwnd: isize, flags: u32) -> isize;
        fn LoadImageW(inst: isize, name: *const u16, kind: u32, cx: i32, cy: i32, flags: u32) -> isize;
        fn SendMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn GetSystemMetrics(index: i32) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
    }
    const GA_ROOT: u32 = 2;
    const IMAGE_ICON: u32 = 1;
    const WM_SETICON: u32 = 0x0080;
    unsafe {
        let top = GetAncestor(hwnd, GA_ROOT);
        let module = GetModuleHandleW(std::ptr::null());
        if top == 0 || module == 0 {
            return;
        }
        // MAKEINTRESOURCE(1)
        let id = 1usize as *const u16;
        for (which, cx, cy) in [(1usize, GetSystemMetrics(11), GetSystemMetrics(12)), (0usize, GetSystemMetrics(49), GetSystemMetrics(50))] {
            // ICON_BIG = 1 (SM_CXICON/SM_CYICON), ICON_SMALL = 0 (SM_CXSMICON/SM_CYSMICON)
            let icon = LoadImageW(module, id, IMAGE_ICON, cx, cy, 0);
            if icon != 0 {
                SendMessageW(top, WM_SETICON, which, icon);
            }
        }
    }
}

#[cfg(not(windows))]
fn set_window_icon(_hwnd: isize) {}
