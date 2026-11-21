use std::collections::HashMap;

use rustybuzz::ttf_parser::{self, GlyphId, gsub::SubstitutionSubtable, opentype_layout::Coverage};

/// Elenca i nomi delle icone leggendo le ligature del font (es. "volume_up").
pub fn ligature_names(data: &[u8]) -> Vec<String> {
    let Ok(face) = ttf_parser::Face::parse(data, 0) else { return Vec::new() };
    let chars: HashMap<u16, char> = "abcdefghijklmnopqrstuvwxyz0123456789_"
        .chars()
        .filter_map(|c| face.glyph_index(c).map(|g| (g.0, c)))
        .collect();
    let Some(gsub) = face.tables().gsub else { return Vec::new() };

    let mut names = Vec::new();
    for lookup in gsub.lookups {
        for sub in lookup.subtables.into_iter::<SubstitutionSubtable>() {
            let SubstitutionSubtable::Ligature(lig) = sub else { continue };
            // Glifo iniziale per ogni indice di copertura.
            let firsts: Vec<GlyphId> = match lig.coverage {
                Coverage::Format1 { glyphs } => glyphs.into_iter().collect(),
                Coverage::Format2 { records } => {
                    let mut v = Vec::new();
                    for r in records {
                        for g in r.start.0..=r.end.0 {
                            let idx = (r.value + g - r.start.0) as usize;
                            if v.len() <= idx {
                                v.resize(idx + 1, GlyphId(0));
                            }
                            v[idx] = GlyphId(g);
                        }
                    }
                    v
                }
            };
            for (i, set) in lig.ligature_sets.into_iter().enumerate() {
                let Some(&first) = firsts.get(i) else { continue };
                let Some(&c0) = chars.get(&first.0) else { continue };
                for l in set {
                    let mut name = String::from(c0);
                    if l.components.into_iter().all(|g| chars.get(&g.0).map(|&c| name.push(c)).is_some()) {
                        names.push(name);
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}
