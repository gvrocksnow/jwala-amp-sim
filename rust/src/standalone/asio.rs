//! Minimal ASIO host. ASIO drivers are COM objects registered under HKLM\SOFTWARE\ASIO; we talk to
//! them through the IASIO vtable directly (the same interface the ASIO SDK wraps), on one thread
//! that owns the driver for its whole life. The driver calls `buffer_switch` on its own thread.

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

type Long = i32;
type AsioBool = i32;
type AsioError = i32;
const ASE_OK: AsioError = 0;
const ASE_SUCCESS: AsioError = 0x3f4847a0;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

#[repr(C)]
struct ChannelInfo {
    channel: Long,
    is_input: AsioBool,
    is_active: AsioBool,
    channel_group: Long,
    sample_type: Long,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BufferInfo {
    is_input: AsioBool,
    channel_num: Long,
    buffers: [*mut c_void; 2],
}

#[repr(C)]
struct Callbacks {
    buffer_switch: extern "system" fn(Long, AsioBool),
    sample_rate_did_change: extern "system" fn(f64),
    asio_message: extern "system" fn(Long, Long, *mut c_void, *mut f64) -> Long,
    buffer_switch_time_info: extern "system" fn(*mut c_void, Long, AsioBool) -> *mut c_void,
}

/// The IASIO interface: IUnknown followed by the ASIO methods, in vtable order.
#[repr(C)]
struct Vtbl {
    query_interface: unsafe extern "system" fn(*mut Iasio, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut Iasio) -> u32,
    release: unsafe extern "system" fn(*mut Iasio) -> u32,
    init: unsafe extern "system" fn(*mut Iasio, *mut c_void) -> AsioBool,
    get_driver_name: unsafe extern "system" fn(*mut Iasio, *mut u8),
    get_driver_version: unsafe extern "system" fn(*mut Iasio) -> Long,
    get_error_message: unsafe extern "system" fn(*mut Iasio, *mut u8),
    start: unsafe extern "system" fn(*mut Iasio) -> AsioError,
    stop: unsafe extern "system" fn(*mut Iasio) -> AsioError,
    get_channels: unsafe extern "system" fn(*mut Iasio, *mut Long, *mut Long) -> AsioError,
    get_latencies: unsafe extern "system" fn(*mut Iasio, *mut Long, *mut Long) -> AsioError,
    get_buffer_size: unsafe extern "system" fn(*mut Iasio, *mut Long, *mut Long, *mut Long, *mut Long) -> AsioError,
    can_sample_rate: unsafe extern "system" fn(*mut Iasio, f64) -> AsioError,
    get_sample_rate: unsafe extern "system" fn(*mut Iasio, *mut f64) -> AsioError,
    set_sample_rate: unsafe extern "system" fn(*mut Iasio, f64) -> AsioError,
    get_clock_sources: unsafe extern "system" fn(*mut Iasio, *mut c_void, *mut Long) -> AsioError,
    set_clock_source: unsafe extern "system" fn(*mut Iasio, Long) -> AsioError,
    get_sample_position: unsafe extern "system" fn(*mut Iasio, *mut c_void, *mut c_void) -> AsioError,
    get_channel_info: unsafe extern "system" fn(*mut Iasio, *mut ChannelInfo) -> AsioError,
    create_buffers: unsafe extern "system" fn(*mut Iasio, *mut BufferInfo, Long, Long, *const Callbacks) -> AsioError,
    dispose_buffers: unsafe extern "system" fn(*mut Iasio) -> AsioError,
    control_panel: unsafe extern "system" fn(*mut Iasio) -> AsioError,
    future: unsafe extern "system" fn(*mut Iasio, Long, *mut c_void) -> AsioError,
    output_ready: unsafe extern "system" fn(*mut Iasio) -> AsioError,
}

#[repr(C)]
struct Iasio {
    vtbl: *const Vtbl,
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(clsid: *const Guid, outer: *mut c_void, ctx: u32, iid: *const Guid, out: *mut *mut c_void) -> i32;
    fn CLSIDFromString(s: *const u16, clsid: *mut Guid) -> i32;
}

/// An installed ASIO driver.
#[derive(Clone)]
pub struct DriverEntry {
    pub name: String,
    clsid: String,
}

/// Drivers registered under HKLM\SOFTWARE\ASIO.
pub fn drivers() -> Vec<DriverEntry> {
    let Ok(asio) = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey("SOFTWARE\\ASIO") else { return Vec::new() };
    let mut v: Vec<DriverEntry> = asio
        .enum_keys()
        .filter_map(|k| k.ok())
        .filter_map(|k| {
            let sub = asio.open_subkey(&k).ok()?;
            let clsid: String = sub.get_value("CLSID").ok()?;
            Some(DriverEntry { name: k, clsid })
        })
        .collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

/// A loaded, initialised driver. Must stay on the thread that created it.
pub struct Driver {
    p: *mut Iasio,
}

fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).trim().to_string()
}

impl Driver {
    /// Initialises COM on this thread and loads the driver.
    pub fn load(entry: &DriverEntry) -> Result<Driver, String> {
        unsafe {
            CoInitializeEx(std::ptr::null_mut(), 2 /* COINIT_APARTMENTTHREADED */);
            let mut clsid = Guid { d1: 0, d2: 0, d3: 0, d4: [0; 8] };
            let wide: Vec<u16> = entry.clsid.encode_utf16().chain([0]).collect();
            if CLSIDFromString(wide.as_ptr(), &mut clsid) != 0 {
                return Err(format!("{}: bad CLSID in the registry", entry.name));
            }
            let mut p: *mut c_void = std::ptr::null_mut();
            // ASIO drivers use their CLSID as the interface id
            let hr = CoCreateInstance(&clsid, std::ptr::null_mut(), 1 /* CLSCTX_INPROC_SERVER */, &clsid, &mut p);
            if hr != 0 || p.is_null() {
                return Err(format!("couldn't load the {} driver (0x{:08x})", entry.name, hr as u32));
            }
            let d = Driver { p: p as *mut Iasio };
            if ((*d.vt()).init)(d.p, std::ptr::null_mut()) == 0 {
                let mut msg = [0u8; 128];
                ((*d.vt()).get_error_message)(d.p, msg.as_mut_ptr());
                let m = cstr(&msg);
                return Err(format!("{} didn't start{}", entry.name, if m.is_empty() { String::new() } else { format!(": {m}") }));
            }
            Ok(d)
        }
    }

    fn vt(&self) -> *const Vtbl {
        unsafe { (*self.p).vtbl }
    }

    pub fn channels(&self) -> (usize, usize) {
        let (mut i, mut o) = (0, 0);
        unsafe { ((*self.vt()).get_channels)(self.p, &mut i, &mut o) };
        (i.max(0) as usize, o.max(0) as usize)
    }

    fn channel_info(&self, ch: usize, input: bool) -> ChannelInfo {
        let mut ci = ChannelInfo { channel: ch as Long, is_input: input as AsioBool, is_active: 0, channel_group: 0, sample_type: 0, name: [0; 32] };
        unsafe { ((*self.vt()).get_channel_info)(self.p, &mut ci) };
        ci
    }

    pub fn input_names(&self) -> Vec<String> {
        (0..self.channels().0).map(|c| {
            let n = cstr(&self.channel_info(c, true).name);
            if n.is_empty() { format!("Input {}", c + 1) } else { n }
        }).collect()
    }

    pub fn sample_rate(&self) -> f64 {
        let mut r = 0.0;
        unsafe { ((*self.vt()).get_sample_rate)(self.p, &mut r) };
        if r > 0.0 { r } else { 48000.0 }
    }

    pub fn preferred_buffer(&self) -> usize {
        let (mut min, mut max, mut pref, mut gran) = (0, 0, 0, 0);
        unsafe { ((*self.vt()).get_buffer_size)(self.p, &mut min, &mut max, &mut pref, &mut gran) };
        if pref > 0 { pref as usize } else { 256 }
    }

    pub fn control_panel(&self) {
        unsafe { ((*self.vt()).control_panel)(self.p) };
    }

    /// Creates buffers for `input` and the first two outputs, installs `rt` as the callback target
    /// and starts streaming.
    pub fn start(&mut self, input: usize, frames: usize, mut rt: Box<Runtime>) -> Result<(), String> {
        let (ins, outs) = self.channels();
        if ins == 0 || outs == 0 {
            return Err("the driver has no inputs or outputs".into());
        }
        let input = input.min(ins - 1);
        let out_r = if outs > 1 { 1 } else { 0 };
        let mut infos = vec![
            BufferInfo { is_input: 1, channel_num: input as Long, buffers: [std::ptr::null_mut(); 2] },
            BufferInfo { is_input: 0, channel_num: 0, buffers: [std::ptr::null_mut(); 2] },
            BufferInfo { is_input: 0, channel_num: out_r, buffers: [std::ptr::null_mut(); 2] },
        ];
        if outs == 1 {
            infos.pop();
        }
        static CALLBACKS: Callbacks = Callbacks { buffer_switch, sample_rate_did_change, asio_message, buffer_switch_time_info };
        let err = unsafe { ((*self.vt()).create_buffers)(self.p, infos.as_mut_ptr(), infos.len() as Long, frames as Long, &CALLBACKS) };
        if err != ASE_OK {
            return Err(format!("the driver couldn't create {frames}-sample buffers (error {err})"));
        }
        rt.in_type = self.channel_info(input, true).sample_type;
        rt.out_type = self.channel_info(0, false).sample_type;
        for t in [rt.in_type, rt.out_type] {
            if bytes_per_sample(t).is_none() {
                unsafe { ((*self.vt()).dispose_buffers)(self.p) };
                return Err(format!("unsupported ASIO sample format {t}"));
            }
        }
        rt.infos = infos;
        rt.frames = frames;
        rt.driver = self.p;
        let r = unsafe { ((*self.vt()).output_ready)(self.p) };
        rt.post_output = r == ASE_OK;
        RUNTIME.store(Box::into_raw(rt), Ordering::Release);
        let err = unsafe { ((*self.vt()).start)(self.p) };
        if err != ASE_OK {
            self.stop();
            return Err(format!("the driver didn't start (error {err})"));
        }
        Ok(())
    }

    /// Stops streaming and drops the callback target.
    pub fn stop(&mut self) {
        unsafe {
            ((*self.vt()).stop)(self.p);
            ((*self.vt()).dispose_buffers)(self.p);
        }
        let rt = RUNTIME.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !rt.is_null() {
            drop(unsafe { Box::from_raw(rt) });
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            ((*self.vt()).release)(self.p);
            CoUninitialize();
        }
    }
}

/// What the callback needs: the engine and the buffer layout.
pub struct Runtime {
    pub process: Box<dyn FnMut(&mut [f32], &mut [f32]) + Send>,
    infos: Vec<BufferInfo>,
    frames: usize,
    in_type: Long,
    out_type: Long,
    driver: *mut Iasio,
    post_output: bool,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Runtime {
    pub fn new(process: Box<dyn FnMut(&mut [f32], &mut [f32]) + Send>, max_frames: usize) -> Box<Runtime> {
        Box::new(Runtime {
            process,
            infos: Vec::new(),
            frames: 0,
            in_type: 0,
            out_type: 0,
            driver: std::ptr::null_mut(),
            post_output: false,
            l: vec![0.0; max_frames],
            r: vec![0.0; max_frames],
        })
    }
}

static RUNTIME: AtomicPtr<Runtime> = AtomicPtr::new(std::ptr::null_mut());
/// The driver asked to be restarted (buffer size or rate changed in its control panel).
pub static RESET_REQUEST: AtomicBool = AtomicBool::new(false);

// ASIO sample types (little-endian)
const INT16: Long = 16;
const INT24: Long = 17;
const INT32: Long = 18;
const FLOAT32: Long = 19;
const FLOAT64: Long = 20;

fn bytes_per_sample(t: Long) -> Option<usize> {
    match t {
        INT16 => Some(2),
        INT24 => Some(3),
        INT32 | FLOAT32 => Some(4),
        FLOAT64 => Some(8),
        _ => None,
    }
}

unsafe fn read(buf: *const u8, t: Long, out: &mut [f32]) {
    for (i, o) in out.iter_mut().enumerate() {
        *o = match t {
            INT16 => i16::from_le_bytes([*buf.add(i * 2), *buf.add(i * 2 + 1)]) as f32 / 32768.0,
            INT24 => {
                let b = buf.add(i * 3);
                ((i32::from_le_bytes([0, *b, *b.add(1), *b.add(2)])) >> 8) as f32 / 8_388_608.0
            }
            INT32 => (buf as *const i32).add(i).read_unaligned() as f32 / 2_147_483_648.0,
            FLOAT32 => (buf as *const f32).add(i).read_unaligned(),
            _ => (buf as *const f64).add(i).read_unaligned() as f32,
        };
    }
}

unsafe fn write(buf: *mut u8, t: Long, src: &[f32]) {
    for (i, &v) in src.iter().enumerate() {
        let v = v.clamp(-1.0, 1.0);
        match t {
            INT16 => {
                let b = ((v * 32767.0) as i16).to_le_bytes();
                *buf.add(i * 2) = b[0];
                *buf.add(i * 2 + 1) = b[1];
            }
            INT24 => {
                let b = ((v * 8_388_607.0) as i32).to_le_bytes();
                let p = buf.add(i * 3);
                *p = b[0];
                *p.add(1) = b[1];
                *p.add(2) = b[2];
            }
            INT32 => (buf as *mut i32).add(i).write_unaligned((v as f64 * 2_147_483_647.0) as i32),
            FLOAT32 => (buf as *mut f32).add(i).write_unaligned(v),
            _ => (buf as *mut f64).add(i).write_unaligned(v as f64),
        }
    }
}

extern "system" fn buffer_switch(index: Long, _direct: AsioBool) {
    let rt = RUNTIME.load(Ordering::Acquire);
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    let n = rt.frames.min(rt.l.len());
    let idx = (index & 1) as usize;
    unsafe {
        read(rt.infos[0].buffers[idx] as *const u8, rt.in_type, &mut rt.l[..n]);
        (rt.process)(&mut rt.l[..n], &mut rt.r[..n]);
        write(rt.infos[1].buffers[idx] as *mut u8, rt.out_type, &rt.l[..n]);
        if let Some(r) = rt.infos.get(2) {
            write(r.buffers[idx] as *mut u8, rt.out_type, &rt.r[..n]);
        }
        if rt.post_output {
            ((*(*rt.driver).vtbl).output_ready)(rt.driver);
        }
    }
}

extern "system" fn buffer_switch_time_info(params: *mut c_void, index: Long, direct: AsioBool) -> *mut c_void {
    buffer_switch(index, direct);
    params
}

extern "system" fn sample_rate_did_change(_rate: f64) {
    RESET_REQUEST.store(true, Ordering::Relaxed);
}

extern "system" fn asio_message(selector: Long, value: Long, _msg: *mut c_void, _opt: *mut f64) -> Long {
    const SELECTOR_SUPPORTED: Long = 1;
    const ENGINE_VERSION: Long = 2;
    const RESET_REQ: Long = 3;
    const BUFFER_SIZE_CHANGE: Long = 4;
    const RESYNC: Long = 5;
    const LATENCIES_CHANGED: Long = 6;
    match selector {
        SELECTOR_SUPPORTED => matches!(value, ENGINE_VERSION | RESET_REQ | BUFFER_SIZE_CHANGE | RESYNC | LATENCIES_CHANGED) as Long,
        ENGINE_VERSION => 2,
        RESET_REQ | BUFFER_SIZE_CHANGE => {
            RESET_REQUEST.store(true, Ordering::Relaxed);
            1
        }
        RESYNC | LATENCIES_CHANGED => 1,
        _ => 0,
    }
}

const _: () = {
    let _ = ASE_SUCCESS;
};
