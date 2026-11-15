# Changelog

All notable changes to Jwala are recorded here, newest first.

## [0.3.0] - 2026-11-15

### Added
- **LEAD II channel, phase 4:** two new factory presets, *Lead II Rhythm* and *Lead II Solo* (14 factory presets in total).
- **TAPE ECHO:** new **DIFFUSE** knob, which blooms the repeats into a wash. Wide pedals can now have up to 8 knobs, laid out 4 per row.
- Before/after comparison renders in `samples/reverb_compare/` and `samples/cab_compare/`.

### Changed: high-quality DSP pass (Rust)
- **CATHEDRAL reverb / shimmer:** rebuilt as a lush stereo hall. It has per-channel pre-delay and input diffusion, and a 16-line Hadamard FDN with allpass diffusers, high damping and a separate low-band decay per line. Line lengths wander slowly. Shimmer is now a phase-aligned octave-up grain shifter fed back into the tank, so it holds a steady octave without warble, and the send scales with decay.
- **TAPE ECHO:** glide, wow, flutter and random speed drift. Repeats run through a tape head model, so they darken and saturate from the first repeat on.
- **RODENT distortion:** now two-stage soft clipping.
- **DOUBLER:** two synthetic takes (one a few cents sharp, one flat) that get retimed note by note, each with its own wandering level and tone. Each take is phase-scrambled so the mono sum doesn't comb, and the image stays centred.
- **SQUEEZE compressor:** smooth sustainer that reads levels from a held peak of a low-cut sidechain, with timing done in dB, so held low notes no longer distort. Release depends on the material (fast after a pick transient, slow after held passages). It has a 12 dB soft knee and keeps a little more of the top end.
- **GREEN DRIVE overdrive:** circuit-modelled mid-hump with a diode feedback clipper and an active tone control.
- **MUFFLER fuzz:** circuit-modelled, with two diode-feedback clipping stages and a scooped passive tone stack.
- **TRI CHORUS:** three voices per side on LFOs spaced 120° apart (one slow, one fast), with per-voice drift, rate-compensated detune and a band-limited wet path.
- **PHASE 6 phaser:** six slightly staggered allpass stages with an exponential sweep that glides per sample. L/R run in quadrature with a slow wander, the resonance loop is band-limited and soft-saturating, and the level stays matched at any mix.
- **Graphic EQ:** nine analog-matched bells (63 Hz–16 kHz). Band gains are solved from the sliders (Välimäki & Liski cascade EQ fit), so the curve follows the sliders to within a fraction of a dB. Slider moves are smoothed and on/off crossfades. The UI curve uses the same response.
- **Cab sim:** zero-latency partitioned convolver (direct head + FFT partitions, one shared input FFT). Built-in cabs are redesigned in the frequency domain with mic-position-dependent cone breakup and a minimum-phase response, plus cabinet reflections and a diffuse tail. Each cab is two speakers × four mic positions blended with a ribbon mic, and the mic position crossfades smoothly.

### Changed: C# version
- Renamed `old-csharp-legacy/` to `legacy-csharp/`.
- Synced the C# version with the Rust version: LEAD II channel, power amp, the high-quality pedals (split into `PedalsPre.cs` / `PedalsPost.cs`), EQ and cab convolver. Renders from the two versions match to float rounding.

### Fixed
- Blurry graphics in the Rust UI. Downscaled assets now use a Lanczos mip chain with a negative LOD bias, which matches the C# version's GDI+ HighQualityBicubic look.

### Version
- Bumped to 0.3.0 in `rust/Cargo.toml` and `legacy-csharp/src/Jwala.csproj`.


## [0.2.0] - 2026-10-22
- Updated equalizer UI


## [0.1.0] - 2026-10-21
- First Release