//! Standalone Jwala, plus dev aids: the offline renderer (`--render`) and an audio-device
//! check (`--audio-test`).

// a GUI app: no console window (the command-line modes attach to the calling terminal instead)
#![windows_subsystem = "windows"]

use std::sync::atomic::Ordering;

#[cfg(windows)]
mod win {
    #[link(name = "user32")]
    extern "system" {
        pub fn SetProcessDpiAwarenessContext(value: isize) -> i32;
        pub fn GetDpiForSystem() -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        pub fn AttachConsole(pid: u32) -> i32;
    }
    /// ATTACH_PARENT_PROCESS
    pub const PARENT: u32 = u32::MAX;
    /// DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
    pub const PER_MONITOR_V2: isize = -4;
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag || a.starts_with(&format!("{flag}=")))
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    // command-line modes print to the terminal they were started from
    #[cfg(windows)]
    if args.get(1).is_some_and(|a| a == "--render" || a == "--audio-test") {
        unsafe { win::AttachConsole(win::PARENT) };
    }
    if args.len() >= 4 && args[1] == "--render" {
        if let Err(e) = jwala_plugin::render::run(&args[2..]) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() >= 3 && args[1] == "--audio-test" {
        std::process::exit(audio_test(&args[2..]));
    }

    // the app runs its own audio host (ASIO / WASAPI / DI file, chosen in ⚙ Settings);
    // nih-plug's wrapper only provides the window and parameters, on its silent dummy backend
    if has(&args, "--no-audio") {
        jwala_plugin::NO_AUDIO.store(true, Ordering::Relaxed);
        args.retain(|a| a != "--no-audio");
    }
    if !has(&args, "--backend") {
        args.extend(["--backend".into(), "dummy".into()]);
    }
    #[cfg(windows)]
    unsafe {
        win::SetProcessDpiAwarenessContext(win::PER_MONITOR_V2);
        if !has(&args, "--dpi-scale") {
            let scale = win::GetDpiForSystem() as f32 / 96.0;
            args.extend(["--dpi-scale".into(), format!("{scale}")]);
        }
    }

    jwala_plugin::STANDALONE.store(true, Ordering::Relaxed);
    nih_plug::wrapper::standalone::nih_export_standalone_with_args::<jwala_plugin::Jwala, _>(args);
}

/// `--audio-test asio "<driver>" [input]` / `--audio-test wasapi` / `--audio-test list`:
/// starts the device with the output muted for two seconds and reports what happened.
fn audio_test(args: &[String]) -> i32 {
    use jwala_plugin::standalone::{devices, AudioHost, AudioSettings, Driver};
    let d = devices();
    if args[0] == "list" {
        println!("ASIO drivers: {:?}", d.asio);
        println!("WASAPI inputs: {:?}", d.inputs);
        println!("WASAPI outputs: {:?}", d.outputs);
        return 0;
    }
    let plugin = jwala_plugin::Jwala::default();
    let (params, shared) = (plugin.params.clone(), plugin.shared.clone());
    // mute the output the way the tuner does
    shared.tuner.active.store(true, Ordering::Relaxed);
    shared.tuner.mute_output.store(true, Ordering::Relaxed);
    let s = match args[0].as_str() {
        "asio" => AudioSettings { driver: Driver::Asio, asio_driver: args.get(1).cloned().unwrap_or_default(), input_channel: args.get(2).and_then(|v| v.parse().ok()).unwrap_or(0), ..Default::default() },
        _ => AudioSettings { driver: Driver::Wasapi, ..Default::default() },
    };
    let mut host = AudioHost::default();
    if s.driver == Driver::Asio {
        println!("input channels: {:?}", host.asio_inputs(&s.asio_driver));
    }
    match host.start(&s, params, shared.clone()) {
        Ok(()) => {
            println!("started: {}", host.status());
            std::thread::sleep(std::time::Duration::from_secs(2));
            println!(
                "after 2 s: running={} input peak={:.4} cpu={:.1}%  (output muted)",
                host.running(),
                shared.meter_in.load(Ordering::Relaxed),
                shared.cpu.load(Ordering::Relaxed) * 100.0
            );
            host.stop();
            println!("stopped");
            0
        }
        Err(e) => {
            println!("failed: {e}");
            1
        }
    }
}
