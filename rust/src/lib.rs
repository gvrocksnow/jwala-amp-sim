//! Jwala: amp & effects rig as a VST3/CLAP plugin and standalone app.

pub mod dsp;
pub mod params;
pub mod plugin;
pub mod presets;
pub mod render;
pub mod spec;
pub mod standalone;
mod ui;

use std::sync::atomic::AtomicBool;

pub use plugin::Jwala;

/// Set by the standalone binary before it starts; the editor then saves/restores the last state itself.
pub static STANDALONE: AtomicBool = AtomicBool::new(false);
/// Standalone started with `--no-audio`: don't open an audio device at launch.
pub static NO_AUDIO: AtomicBool = AtomicBool::new(false);

nih_plug::nih_export_clap!(Jwala);
nih_plug::nih_export_vst3!(Jwala);
