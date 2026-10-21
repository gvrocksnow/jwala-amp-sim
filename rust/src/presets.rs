//! Factory presets and user presets. User presets are JSON key→value maps in
//! `%AppData%\Jwala\presets\`, the same files the C# version wrote.

use std::collections::HashMap;
use std::path::PathBuf;

pub type State = HashMap<String, f32>;

/// Factory presets: partial states; unspecified values use defaults, effects off.
pub const FACTORY: [(&str, &str); 12] = [
    ("Init", ""),
    ("Glass Clean", "amp.CHANNEL=0 amp.GAIN=0.45 amp.BASS=0.55 amp.MIDDLE=0.45 amp.TREBLE=0.65 amp.BRIGHT=1 comp.ON=1 comp.SUSTAIN=0.45 chorus.ON=1 chorus.MIX=0.35 delay.ON=1 delay.TIME=0.62 delay.MIX=0.22 reverb.ON=1 reverb.MIX=0.3 cab.MODEL=1"),
    ("Ambient Shimmer", "amp.CHANNEL=0 amp.GAIN=0.4 amp.TREBLE=0.6 comp.ON=1 delay.ON=1 delay.TIME=0.72 delay.FEEDBACK=0.55 delay.MIX=0.35 delay.MOD=0.45 delay.SPREAD=1 reverb.ON=1 reverb.DECAY=0.85 reverb.SIZE=0.9 reverb.SHIMMER=0.45 reverb.MIX=0.55 reverb.PREDELAY=0.3 cab.MODEL=1"),
    ("Classic Crunch", "doubler.ON=1 doubler.AMOUNT=0.4 amp.CHANNEL=1 amp.GAIN=0.55 amp.MIDDLE=0.7 amp.TREBLE=0.6 amp.PRESENCE=0.55 reverb.ON=1 reverb.DECAY=0.3 reverb.MIX=0.18 cab.MODEL=0"),
    ("Blues Lead", "amp.CHANNEL=1 amp.GAIN=0.6 amp.MIDDLE=0.7 od.ON=1 od.DRIVE=0.5 od.LEVEL=0.55 delay.ON=1 delay.TIME=0.5 delay.MIX=0.18 delay.FEEDBACK=0.25 reverb.ON=1 reverb.MIX=0.22 cab.MODEL=2"),
    ("Modern Metal", "doubler.ON=1 amp.CHANNEL=2 amp.GAIN=0.55 amp.BASS=0.55 amp.MIDDLE=0.45 amp.TREBLE=0.62 amp.DEPTH=0.6 global.GATE=0.45 od.ON=1 od.DRIVE=0 od.LEVEL=0.85 od.TONE=0.6 cab.MODEL=0 cab.MIC POS=0.25"),
    ("Soaring Solo", "amp.CHANNEL=2 amp.GAIN=0.7 amp.MIDDLE=0.7 amp.PRESENCE=0.55 od.ON=1 od.DRIVE=0.2 od.LEVEL=0.75 delay.ON=1 delay.TIME=0.66 delay.FEEDBACK=0.42 delay.MIX=0.3 delay.DUCK=0.6 delay.SPREAD=0.8 reverb.ON=1 reverb.DECAY=0.55 reverb.MIX=0.28"),
    ("Fuzz Wall", "amp.CHANNEL=1 amp.GAIN=0.4 fuzz.ON=1 fuzz.SUSTAIN=0.75 fuzz.TONE=0.45 phaser.ON=1 phaser.MIX=0.4 reverb.ON=1 reverb.MIX=0.25 cab.MODEL=0"),
    ("Rodent Grunge", "doubler.ON=1 amp.CHANNEL=1 amp.GAIN=0.45 dist.ON=1 dist.DIST=0.65 dist.FILTER=0.4 chorus.ON=1 chorus.RATE=0.25 chorus.MIX=0.4 cab.MODEL=0"),
    ("Acoustic 1", "amp.CHANNEL=3 amp.GAIN=0.56 amp.BASS=0 amp.MIDDLE=0 amp.TREBLE=0.73 amp.PRESENCE=0.45 amp.DEPTH=0 amp.MASTER=0.5 cab.ON=0 comp.ON=1 comp.SUSTAIN=0.25 comp.ATTACK=0.6 comp.LEVEL=0.35 reverb.ON=1 reverb.DECAY=0.2 reverb.SIZE=0.5 reverb.PREDELAY=0.05 reverb.MOD=0.2 reverb.MIX=0.32"),
    ("Acoustic 2", "amp.CHANNEL=3 amp.GAIN=0.85 amp.BASS=0.2 amp.MIDDLE=0.87 amp.TREBLE=1 amp.PRESENCE=0.36 amp.DEPTH=0 amp.MASTER=0.5 cab.ON=0 comp.ON=1 comp.SUSTAIN=0.25 comp.ATTACK=0.6 comp.LEVEL=0.35 reverb.ON=1 reverb.DECAY=0.35 reverb.SIZE=0.5 reverb.PREDELAY=0.05 reverb.MOD=0.2 reverb.MIX=0.35"),
    ("Acoustic 3", "amp.CHANNEL=3 amp.GAIN=0.5 amp.BASS=0.5 amp.MIDDLE=0.5 amp.TREBLE=0.5 amp.PRESENCE=0.4 amp.DEPTH=0.5 amp.MASTER=0.5 amp.DECAY=1 cab.ON=0 comp.ON=1 comp.SUSTAIN=0.25 comp.ATTACK=0.6 comp.LEVEL=0.35 reverb.ON=1 reverb.DECAY=0.35 reverb.SIZE=0.5 reverb.PREDELAY=0.05 reverb.MOD=0.2 reverb.MIX=0.15"),
];

/// Parses "key=value key=value"; keys may contain spaces ("cab.MIC POS"), so split on '=' boundaries.
pub fn parse_spec(spec: &str) -> State {
    let mut d = State::new();
    if spec.trim().is_empty() {
        return d;
    }
    let parts: Vec<&str> = spec.split('=').collect();
    let mut key = parts[0].to_string();
    for (i, seg) in parts.iter().enumerate().skip(1) {
        let sp = if i < parts.len() - 1 { seg.find(' ') } else { None };
        let val = match sp {
            Some(sp) => &seg[..sp],
            None => seg,
        };
        if let Ok(v) = val.trim().parse::<f32>() {
            d.insert(key.trim().to_string(), v);
        }
        if let Some(sp) = sp {
            key = seg[sp + 1..].to_string();
        }
    }
    d
}

pub fn factory(name: &str) -> Option<State> {
    FACTORY.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, s)| parse_spec(s))
}

pub fn is_factory(name: &str) -> bool {
    FACTORY.iter().any(|(n, _)| *n == name)
}

/// `%AppData%\Jwala`. On first use, a folder left by the old name (`TubeForge`) is moved
/// here, and its settings file renamed, so user presets and the last state carry over.
pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Jwala");
    static MIGRATE: std::sync::Once = std::sync::Once::new();
    MIGRATE.call_once(|| {
        let old = base.join("TubeForge");
        if !dir.exists() && old.is_dir() && std::fs::rename(&old, &dir).is_ok() {
            let _ = std::fs::rename(dir.join("tubeforge-rs.json"), dir.join("jwala-rs.json"));
        }
    });
    dir
}

fn user_dir() -> PathBuf {
    app_dir().join("presets")
}

pub fn user_names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(user_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn load_user(name: &str) -> Option<State> {
    let text = std::fs::read_to_string(user_dir().join(format!("{name}.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Strips characters that can't be in a file name.
pub fn safe_name(n: &str) -> String {
    n.chars().filter(|c| !r#"<>:"/\|?*"#.contains(*c) && !c.is_control()).collect::<String>().trim().to_string()
}

pub fn save_user(name: &str, st: &State) -> std::io::Result<()> {
    std::fs::create_dir_all(user_dir())?;
    let mut sorted: Vec<(&String, &f32)> = st.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let map: serde_json::Map<String, serde_json::Value> = sorted.into_iter().map(|(k, v)| (k.clone(), serde_json::json!(*v))).collect();
    std::fs::write(user_dir().join(format!("{}.json", safe_name(name))), serde_json::to_string_pretty(&map)?)
}

pub fn delete_user(name: &str) {
    let _ = std::fs::remove_file(user_dir().join(format!("{name}.json")));
}

/// All preset names in browse order: factory first, then user.
pub fn all_names() -> Vec<String> {
    FACTORY.iter().map(|(n, _)| n.to_string()).chain(user_names()).collect()
}

/// Factory or user preset by name.
pub fn get(name: &str) -> Option<State> {
    if is_factory(name) { factory(name) } else { load_user(name) }
}
