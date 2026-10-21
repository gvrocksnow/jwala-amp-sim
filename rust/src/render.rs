//! Offline render for testing: `jwala --render in.wav out.wav ["Preset Name"] [key=value ...] [ir=user_ir.wav]`.
//! Renders the input followed by 4 s of silence (so delay/reverb tails are audible) to 32-bit float stereo.

use crate::dsp::rig::Rig;
use crate::dsp::state::RigState;
use crate::presets;

pub fn run(args: &[String]) -> Result<(), String> {
    let (inp, outp) = (&args[0], &args[1]);
    let mut state = presets::State::new();
    let mut ir_path = None;
    if let Some(name) = args.get(2) {
        state = presets::factory(name).ok_or_else(|| format!("unknown preset {name}"))?;
        // presets leave the global input/gate/output alone (as in the app); overrides may still set them
        state.retain(|k, _| !k.starts_with("global."));
        for kv in &args[3..] {
            // ir=<file.wav> is the user IR for cab.MODEL=3
            if let Some(p) = kv.strip_prefix("ir=") {
                ir_path = Some(p.to_string());
                continue;
            }
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("bad override {kv}"))?;
            state.insert(k.to_string(), v.parse().map_err(|_| format!("bad value in {kv}"))?);
        }
    }
    let st = RigState::from_map(&state);

    let (mono, fs) = read_mono(inp)?;

    let mut rig = Rig::new(fs as f64, &st, 256);
    let user = match &ir_path {
        Some(p) => {
            let (data, rate) = read_mono(p)?;
            Some((data, rate))
        }
        None => None,
    };
    let cab = crate::dsp::convolver::build_cab(fs as f64, st.cab.model as usize, user.as_ref().map(|(d, r)| (&d[..], *r)));
    rig.swap_cab(cab);
    let out_spec = hound::WavSpec { channels: 2, sample_rate: fs, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(outp, out_spec).map_err(|e| e.to_string())?;
    let total = mono.len() + fs as usize * 4;
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let t0 = std::time::Instant::now();
    let mut pos = 0;
    while pos < total {
        let n = (total - pos).min(256);
        let mut block = [0.0f32; 256];
        for (i, b) in block.iter_mut().enumerate().take(n) {
            *b = mono.get(pos + i).copied().unwrap_or(0.0);
        }
        rig.process(&block[..n], &mut l[..n], &mut r[..n], &st, false);
        for i in 0..n {
            w.write_sample(l[i]).map_err(|e| e.to_string())?;
            w.write_sample(r[i]).map_err(|e| e.to_string())?;
        }
        pos += n;
    }
    w.finalize().map_err(|e| e.to_string())?;
    let secs = total as f64 / fs as f64;
    println!("rendered {:.2}s audio in {:.2}s ({:.0}x realtime)", secs, t0.elapsed().as_secs_f64(), secs / t0.elapsed().as_secs_f64());
    Ok(())
}

/// A .wav file's first channel and its sample rate.
fn read_mono(path: &str) -> Result<(Vec<f32>, u32), String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    Ok((samples.chunks(ch).map(|f| f[0]).collect(), spec.sample_rate))
}
