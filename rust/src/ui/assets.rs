//! Images shared with the legacy C# app (`assets/` at the repo root), embedded in the binary and decoded once per process
//! into premultiplied RGBA mip chains ready for upload.

use std::sync::OnceLock;

macro_rules! assets {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../../../assets/", $name)) as &[u8])),*]
    };
}

static FILES: &[(&str, &[u8])] = assets![
    "backdrop.jpg", "faceplate.jpg", "grille.jpg", "tolex.jpg",
    "jewel.png", "knob.png", "pedal_knob.png",
    "amp_head.png", "amp_cab.png", "amp_knob.png", "amp_led_green.png", "amp_led_red.png", "amp_led_off.png",
    "cab_412.png", "cab_212.png", "cab_112.png", "cab_mic.png",
    "eq_panel.png",
    "led_glow.png", "port_left.png", "port_right.png",
    "pastel_comp.png", "pastel_od.png", "pastel_dist.png", "pastel_fuzz.png", "pastel_doubler.png",
    "pastel_chorus.png", "pastel_phaser.png", "pastel_delay.png", "pastel_reverb.png",
    "knob_comp.png", "knob_od.png", "knob_dist.png", "knob_fuzz.png", "knob_doubler.png",
    "knob_chorus.png", "knob_phaser.png", "knob_delay.png", "knob_reverb.png",
    "footswitch_comp.png", "footswitch_od.png", "footswitch_dist.png", "footswitch_fuzz.png", "footswitch_doubler.png",
    "footswitch_chorus.png", "footswitch_phaser.png", "footswitch_delay.png", "footswitch_reverb.png",
    "led_off_comp.png", "led_off_od.png", "led_off_dist.png", "led_off_fuzz.png", "led_off_doubler.png",
    "led_off_chorus.png", "led_off_phaser.png", "led_off_delay.png", "led_off_reverb.png",
];

/// One decoded image: premultiplied RGBA8 mip levels, largest first.
pub struct Decoded {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub mips: Vec<Vec<u8>>,
}

fn decode(name: &'static str, bytes: &[u8]) -> Decoded {
    let img = image::load_from_memory(bytes).map(|i| i.to_rgba8()).unwrap_or_else(|_| image::RgbaImage::new(1, 1));
    let (w, h) = img.dimensions();
    let mut level = img;
    // premultiply so filtering never bleeds colour out of transparent pixels
    for p in level.pixels_mut() {
        let a = p[3] as u32;
        for c in 0..3 {
            p[c] = ((p[c] as u32 * a + 127) / 255) as u8;
        }
    }
    let mut mips = Vec::new();
    loop {
        let (lw, lh) = level.dimensions();
        mips.push(level.as_raw().clone());
        if lw == 1 && lh == 1 {
            break;
        }
        level = image::imageops::resize(&level, (lw / 2).max(1), (lh / 2).max(1), image::imageops::FilterType::Triangle);
    }
    Decoded { name, width: w, height: h, mips }
}

/// All images, decoded in parallel on first use.
pub fn images() -> &'static [Decoded] {
    static CACHE: OnceLock<Vec<Decoded>> = OnceLock::new();
    CACHE.get_or_init(|| {
        std::thread::scope(|s| {
            let handles: Vec<_> = FILES.iter().map(|&(n, b)| s.spawn(move || decode(n, b))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    })
}
