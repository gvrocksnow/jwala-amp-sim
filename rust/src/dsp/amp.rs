//! Preamp + tone stack + power amp, all at 4x oversampling. Oversampled-slot pedals
//! (drives/fuzz) run inside the same oversampled region so they don't alias either.
//! Channel 3 (ACOUSTIC) swaps the tube stages for the acoustic-guitar simulator.

use super::acoustic::AcousticSim;
use super::filters::{Biquad, OnePole, ToneStack};
use super::oversampler::{Oversampler, FACTOR};
use super::pedals::Stomp;
use super::state::AmpState;

fn db(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// One triode gain stage: coupling-cap high-pass, Miller-cap band-limiting, asymmetric
/// grid-conduction clipping with dynamic bias shift (the "blocking" that makes hard
/// picking bloom), plate low-pass, phase inversion.
#[derive(Default, Clone, Copy)]
struct TriodeStage {
    drive: f64,
    bias_shift: f64,
    couple: OnePole,
    miller: OnePole,
    plate: OnePole,
    dc: OnePole,
    env: f64,
    atk: f64,
    rel: f64,
}

const BIAS0: f64 = 0.18; // static operating point offset -> even harmonics

impl TriodeStage {
    fn configure(&mut self, fs: f64, hp_hz: f64, lp_hz: f64) {
        self.couple.set_low_pass(fs, hp_hz);
        self.miller.set_low_pass(fs, lp_hz);
        self.plate.set_low_pass(fs, 18000.0);
        self.dc.set_low_pass(fs, 8.0);
        self.atk = 1.0 - (-1.0 / (fs * 0.004)).exp();
        self.rel = 1.0 - (-1.0 / (fs * 0.12)).exp();
    }

    /// Soft, compressive grid-conduction side; harder cutoff side with more headroom.
    #[inline]
    fn shape(v: f64) -> f64 {
        if v >= 0.0 { v / (1.0 + v) } else { -1.6 * (-v / 1.6).tanh() }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let hp = self.couple.high_pass(x);
        let v = self.miller.low_pass(hp) * self.drive;
        let pos = v.max(0.0);
        self.env += (if pos > self.env { self.atk } else { self.rel }) * (pos - self.env);
        let mut y = Self::shape(v + BIAS0 - self.env * self.bias_shift) - Self::shape(BIAS0);
        y = self.plate.low_pass(y);
        y = self.dc.high_pass(y);
        -y
    }
}

pub struct AmpEngine {
    fs_os: f64,
    os: Oversampler,
    stages: [TriodeStage; 4],
    stage_count: usize,
    /// Lead only: how much of the first stage's output is blended past the later stages.
    blend: f64,
    /// Lead only: the bass cut before the stages, put back after them.
    lows: Biquad,
    tone: ToneStack,
    bright_cap: Biquad,
    presence: Biquad,
    resonance: Biquad,
    power_hp: Biquad,
    sag_env: f64,
    sag_atk: f64,
    sag_rel: f64,
    s_gain: f64,
    s_master: f64,
    smooth: f64,
    last_pres: f64,
    last_res: f64,
    last_ch: i32,
    last_bright: bool,
    acoustic: AcousticSim,
}

impl AmpEngine {
    pub fn new(sample_rate: f64, max_block: usize) -> Self {
        let fs_os = sample_rate * FACTOR as f64;
        let mut power_hp = Biquad::default();
        power_hp.high_pass(fs_os, 35.0, 0.7);
        AmpEngine {
            fs_os,
            os: Oversampler::new(),
            stages: [TriodeStage::default(); 4],
            stage_count: 0,
            blend: 0.0,
            lows: Biquad::default(),
            tone: ToneStack::new(fs_os),
            bright_cap: Biquad::default(),
            presence: Biquad::default(),
            resonance: Biquad::default(),
            power_hp,
            sag_env: 0.0,
            sag_atk: 1.0 - (-1.0 / (fs_os * 0.012)).exp(),
            sag_rel: 1.0 - (-1.0 / (fs_os * 0.18)).exp(),
            s_gain: 0.0,
            s_master: 0.0,
            smooth: 1.0 - (-1.0 / (sample_rate * 0.02)).exp(),
            last_pres: -1.0,
            last_res: -1.0,
            last_ch: -1,
            last_bright: false,
            acoustic: AcousticSim::new(sample_rate, max_block),
        }
    }

    pub fn os_rate(&self) -> f64 {
        self.fs_os
    }

    fn configure_channel(&mut self, ch: u8, bright: bool) {
        // (hp Hz, lp Hz, inter-stage gain, bias shift) per stage
        // Lead: the bass is cut before the first two stages so low notes don't intermodulate with
        // everything above them (tight, even note to note; a shelf after the stages puts it back),
        // and the Miller corners are low so the stages clip a darker signal: less fizz and hiss,
        // the treble comes from the harmonics instead.
        let cfg: &[(f64, f64, f64, f64)] = match ch {
            0 => &[(25.0, 16000.0, 1.0, 0.25), (40.0, 12000.0, 1.8, 0.3)],
            1 => &[(35.0, 14000.0, 1.0, 0.4), (110.0, 10000.0, 5.0, 0.5), (60.0, 8500.0, 2.5, 0.6)],
            _ => &[(440.0, 6000.0, 1.0, 0.52), (215.0, 4500.0, 9.0, 0.63), (60.0, 3750.0, 10.0, 0.63), (55.0, 3400.0, 2.5, 0.63)],
        };
        // a little of the first stage, unclipped, keeps the lead defined and alive under the sustain
        self.blend = if ch == 2 { db(4.3) } else { 0.0 };
        self.lows.low_shelf(self.fs_os, 300.0, if ch == 2 { 8.0 } else { 0.0 });
        self.stage_count = cfg.len();
        for (i, c) in cfg.iter().enumerate() {
            self.stages[i].configure(self.fs_os, c.0, c.1);
            self.stages[i].drive = c.2;
            self.stages[i].bias_shift = c.3;
        }
        let shelf = (if ch == 0 { 5.0 } else { 3.0 }) + if bright { 5.0 } else { 0.0 };
        self.bright_cap.high_shelf(self.fs_os, 2200.0, shelf);
    }

    /// The acoustic channel's stereo side signal for the last block (see `AcousticSim::side`).
    pub fn acoustic_side(&self, n: usize) -> &[f32] {
        self.acoustic.side(n)
    }

    /// Processes a mono block in place. `pedals` run inside the oversampled region,
    /// `pedal_on[i]` is each pedal's bypass state (their `update` has been called this block).
    pub fn process(&mut self, buf: &mut [f32], p: &AmpState, pedals: &mut [(usize, Stomp)], pedal_on: &[bool]) {
        if p.channel == 3 {
            // acoustic: drive pedals still run oversampled, then the body simulator at the base rate
            let mut ob = [0.0f64; FACTOR];
            for s in buf.iter_mut() {
                self.os.up(*s as f64, &mut ob);
                for o in ob.iter_mut() {
                    for ((_, e), on) in pedals.iter_mut().zip(pedal_on) {
                        *o = e.run(*o, *on);
                    }
                }
                *s = self.os.down(&ob) as f32;
            }
            self.acoustic.process(buf, p);
            return;
        }
        let ch = p.channel.min(2);
        if ch as i32 != self.last_ch || p.bright != self.last_bright {
            self.last_ch = ch as i32;
            self.last_bright = p.bright;
            self.configure_channel(ch, p.bright);
        }
        self.tone.set(p.treble as f64, p.middle as f64, p.bass as f64);
        if p.presence as f64 != self.last_pres || p.depth as f64 != self.last_res {
            self.last_pres = p.presence as f64;
            self.last_res = p.depth as f64;
            self.presence.high_shelf(self.fs_os, 3000.0, -3.0 + 11.0 * self.last_pres);
            self.resonance.peak(self.fs_os, 95.0, 0.9, -2.0 + 10.0 * self.last_res);
        }

        let g = p.gain as f64;
        let gain_db = match ch {
            0 => -10.0 + 30.0 * g,
            1 => -6.0 + 34.0 * g,
            _ => 34.0 * g,
        };
        let t_gain = db(gain_db) * 4.0; // DI level (~0.1-0.3 peak) -> grid volts scale
        let t_master = db(-16.0 + 26.0 * p.master as f64); // power amp drive
        let ch_makeup = if ch == 0 { 1.6 } else { 1.0 };
        const TONE_MAKEUP: f64 = 3.2;

        let mut ob = [0.0f64; FACTOR];
        for s in buf.iter_mut() {
            self.s_gain += self.smooth * (t_gain - self.s_gain);
            self.s_master += self.smooth * (t_master - self.s_master);
            self.os.up(*s as f64, &mut ob);
            for o in ob.iter_mut() {
                let mut v = *o;
                for ((_, e), on) in pedals.iter_mut().zip(pedal_on) {
                    v = e.run(v, *on);
                }
                v *= self.s_gain;
                let mut first = 0.0;
                for st in 0..self.stage_count {
                    v = self.stages[st].process(v);
                    if st == 0 {
                        v = self.bright_cap.process(v);
                        first = v;
                    }
                }
                v -= first * self.blend; // one stage is inverted against the four
                if ch == 2 {
                    v = self.lows.process(v);
                }
                v = self.tone.process(v * ch_makeup) * TONE_MAKEUP;

                // power amp: presence/depth, push-pull saturation with supply sag
                v = self.resonance.process(self.presence.process(v));
                v = self.power_hp.process(v) * self.s_master;
                let ceiling = 1.0 / (1.0 + 0.6 * self.sag_env);
                let y = ceiling * (v / ceiling).tanh();
                let ay = y.abs();
                self.sag_env += (if ay > self.sag_env { self.sag_atk } else { self.sag_rel }) * (ay - self.sag_env);
                *o = y;
            }
            *s = self.os.down(&ob) as f32;
        }
    }
}
