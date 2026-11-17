//! Finding the UI's typefaces on each platform. The UI was designed around the Windows fonts the C#
//! version used (Segoe UI, Segoe Script, Arial); macOS and Linux get their closest system faces:
//!
//! - Windows: `%WINDIR%\Fonts` (unchanged from before)
//! - macOS: Helvetica Neue for the Segoe UI weights, Bradley Hand for the script logo, Arial Bold,
//!   Apple Symbols; faces inside `.ttc` collections are picked by PostScript name
//! - Linux: whatever fontconfig (`fc-match`) picks for each role
//!
//! Each role lists candidates in order of preference; the first file that exists and parses wins.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Face {
    Regular,
    Bold,
    Semibold,
    Light,
    /// The handwritten logo face (Segoe Script Bold on Windows).
    Script,
    ArialBold,
    /// Fallback for symbols (◀ ▶ ♩ ♭ ♯ ⚙ …).
    Symbol,
}

/// Which face of a font file: a collection index, or (for `.ttc` collections whose order isn't
/// fixed) the face's PostScript name.
#[allow(dead_code)]
enum Pick {
    Index(u32),
    Named(&'static str),
}

/// The best available font for `face`, if any.
pub(crate) fn load(face: Face) -> Option<fontdue::Font> {
    candidates(face).into_iter().find_map(|(path, pick)| {
        let bytes = std::fs::read(&path).ok()?;
        let index = match pick {
            Pick::Index(i) => i,
            Pick::Named(name) => index_of(&bytes, name)?,
        };
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings { collection_index: index, ..Default::default() }).ok()
    })
}

/// The index of the face with PostScript name `name` in a font file or collection.
fn index_of(bytes: &[u8], name: &str) -> Option<u32> {
    let n = ttf_parser::fonts_in_collection(bytes).unwrap_or(1);
    (0..n).find(|&i| ttf_parser::Face::parse(bytes, i).is_ok_and(|f| postscript_name(&f).as_deref() == Some(name)))
}

fn postscript_name(face: &ttf_parser::Face) -> Option<String> {
    let names = face.names();
    (0..names.len()).filter_map(|i| names.get(i)).filter(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME).find_map(|n| {
        if n.is_unicode() {
            // UTF-16BE
            let units: Vec<u16> = n.name.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16(&units).ok()
        } else {
            // Mac Roman; PostScript names are plain ASCII
            n.name.is_ascii().then(|| String::from_utf8_lossy(n.name).into_owned())
        }
    })
}

#[cfg(windows)]
fn candidates(face: Face) -> Vec<(PathBuf, Pick)> {
    let dir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
    let file = match face {
        Face::Regular => "segoeui.ttf",
        Face::Bold => "segoeuib.ttf",
        Face::Semibold => "seguisb.ttf",
        Face::Light => "segoeuil.ttf",
        Face::Script => "segoescb.ttf",
        Face::ArialBold => "arialbd.ttf",
        Face::Symbol => "seguisym.ttf",
    };
    vec![(dir.join(file), Pick::Index(0))]
}

#[cfg(target_os = "macos")]
fn candidates(face: Face) -> Vec<(PathBuf, Pick)> {
    const NEUE: &str = "/System/Library/Fonts/HelveticaNeue.ttc";
    const HELVETICA: &str = "/System/Library/Fonts/Helvetica.ttc";
    const SUPP: &str = "/System/Library/Fonts/Supplemental";
    let p = |s: &str| PathBuf::from(s);
    let supp = |f: &str| PathBuf::from(SUPP).join(f);
    match face {
        Face::Regular => vec![(p(NEUE), Pick::Named("HelveticaNeue")), (p(HELVETICA), Pick::Named("Helvetica")), (supp("Arial.ttf"), Pick::Index(0))],
        Face::Bold => vec![(p(NEUE), Pick::Named("HelveticaNeue-Bold")), (p(HELVETICA), Pick::Named("Helvetica-Bold")), (supp("Arial Bold.ttf"), Pick::Index(0))],
        Face::Semibold => vec![(p(NEUE), Pick::Named("HelveticaNeue-Medium")), (p(NEUE), Pick::Named("HelveticaNeue-Bold")), (p(HELVETICA), Pick::Named("Helvetica-Bold"))],
        Face::Light => vec![(p(NEUE), Pick::Named("HelveticaNeue-Light")), (p(HELVETICA), Pick::Named("Helvetica-Light")), (p(NEUE), Pick::Named("HelveticaNeue"))],
        Face::Script => vec![(supp("Bradley Hand Bold.ttf"), Pick::Index(0)), (supp("SnellRoundhand.ttc"), Pick::Named("SnellRoundhand-Bold")), (supp("SnellRoundhand.ttc"), Pick::Index(0))],
        Face::ArialBold => vec![(supp("Arial Bold.ttf"), Pick::Index(0)), (p(HELVETICA), Pick::Named("Helvetica-Bold"))],
        Face::Symbol => vec![(p("/System/Library/Fonts/Apple Symbols.ttf"), Pick::Index(0)), (supp("Arial Unicode.ttf"), Pick::Index(0))],
    }
}

#[cfg(target_os = "linux")]
fn candidates(face: Face) -> Vec<(PathBuf, Pick)> {
    let patterns: &[&str] = match face {
        Face::Regular => &["sans-serif"],
        Face::Bold => &["sans-serif:bold"],
        Face::Semibold => &["sans-serif:semibold", "sans-serif:bold"],
        Face::Light => &["sans-serif:light"],
        Face::Script => &["cursive:bold"],
        // usually Liberation Sans Bold, Arial's metric-compatible stand-in
        Face::ArialBold => &["Arial:bold"],
        Face::Symbol => &["DejaVu Sans", "Noto Sans Symbols2", "Noto Sans Symbols"],
    };
    patterns.iter().filter_map(|p| fc_match(p)).collect()
}

/// The file (and face index) fontconfig picks for `pattern`.
#[cfg(target_os = "linux")]
fn fc_match(pattern: &str) -> Option<(PathBuf, Pick)> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}\n%{index}", pattern]).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    let mut lines = s.lines();
    let file = lines.next()?.trim();
    let index = lines.next().and_then(|i| i.trim().parse().ok()).unwrap_or(0);
    (!file.is_empty()).then(|| (PathBuf::from(file), Pick::Index(index)))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn candidates(_face: Face) -> Vec<(PathBuf, Pick)> {
    Vec::new()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The by-name lookup macOS relies on, checked against a Windows collection with two faces.
    #[test]
    fn finds_faces_in_a_collection_by_postscript_name() {
        let dir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("Fonts");
        let Ok(bytes) = std::fs::read(dir.join("cambria.ttc")) else { return };
        assert_eq!(index_of(&bytes, "Cambria"), Some(0));
        assert_eq!(index_of(&bytes, "CambriaMath"), Some(1));
        assert_eq!(index_of(&bytes, "NoSuchFace"), None);
    }

    #[test]
    fn windows_faces_load() {
        for face in [Face::Regular, Face::Bold, Face::Semibold, Face::Light, Face::Script, Face::ArialBold, Face::Symbol] {
            assert!(load(face).is_some(), "{face:?}");
        }
    }
}
