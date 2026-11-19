//! The UI's typefaces, embedded in the binary so text looks the same on every platform. The UI was
//! designed around the Windows fonts the C# version used, which can't be redistributed; these are
//! their open-licensed stand-ins (`assets/fonts/`, licences alongside):
//!
//! - Segoe UI → Selawik (SIL OFL): Microsoft's metric-compatible substitute, same advance widths
//! - Arial Bold → Arimo Bold (SIL OFL): metric-compatible with Arial
//! - Segoe UI Symbol → DejaVu Sans (Bitstream Vera licence): the symbol fallback
//!
//! The Segoe Script logo is a pre-rendered image instead (see `crate::logo`).

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Face {
    Regular,
    Bold,
    Semibold,
    Light,
    ArialBold,
    /// Fallback for symbols (◀ ▶ ♩ ♭ ♯ ⚙ …).
    Symbol,
}

macro_rules! font {
    ($file:literal) => {
        include_bytes!(concat!("../../assets/fonts/", $file)) as &'static [u8]
    };
}

fn bytes(face: Face) -> &'static [u8] {
    match face {
        Face::Regular => font!("Selawik-Regular.ttf"),
        Face::Bold => font!("Selawik-Bold.ttf"),
        Face::Semibold => font!("Selawik-Semibold.ttf"),
        Face::Light => font!("Selawik-Light.ttf"),
        Face::ArialBold => font!("Arimo-Bold.ttf"),
        Face::Symbol => font!("DejaVuSans.ttf"),
    }
}

/// The font for `face`.
pub(crate) fn load(face: Face) -> Option<fontdue::Font> {
    fontdue::Font::from_bytes(bytes(face), fontdue::FontSettings::default()).ok()
}

/// Line metrics (ascent, descent; per em) to use instead of the font's own, if any. Selawik has
/// Segoe UI's widths and cap height but a shorter line (1.20 em against 1.33 em), so it gets
/// Segoe UI's: sizes fitted to a line height (the pedal names) and vertical centring then match.
pub(crate) fn line_metrics_em(face: Face) -> Option<(f32, f32)> {
    match face {
        Face::Regular | Face::Bold | Face::Semibold | Face::Light => Some((2210.0 / 2048.0, -514.0 / 2048.0)),
        Face::ArialBold | Face::Symbol => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Face; 6] = [Face::Regular, Face::Bold, Face::Semibold, Face::Light, Face::ArialBold, Face::Symbol];

    #[test]
    fn embedded_faces_load() {
        for face in ALL {
            assert!(load(face).is_some(), "{face:?}");
        }
    }

    /// Every non-ASCII character the UI draws has a glyph in the text face or the symbol fallback.
    #[test]
    fn symbols_are_covered() {
        let (text, symbol) = (load(Face::Regular).unwrap(), load(Face::Symbol).unwrap());
        for c in "…—·¢◀▶▾⚙♩→✓♭♯".chars() {
            assert!(text.lookup_glyph_index(c) != 0 || symbol.lookup_glyph_index(c) != 0, "{c}");
        }
    }
}
