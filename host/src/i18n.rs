//! Lingua dell'interfaccia. I testi nel codice sono in italiano; `lang/en.tsv` li traduce in
//! inglese. `t("Salva")` restituisce la traduzione (o il testo originale se manca),
//! `tr!("Pagina {}", n)` fa lo stesso con dei valori da inserire al posto di `{}`.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);
static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();

const EN_TSV: &str = include_str!("../lang/en.tsv");

/// Lingue disponibili: (codice nella config, nome da mostrare).
pub const LANGUAGES: &[(&str, &str)] = &[("auto", "Automatica / Automatic"), ("it", "Italiano"), ("en", "English")];

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\t", "\t")
}

fn table() -> &'static HashMap<String, String> {
    TABLE.get_or_init(|| {
        EN_TSV
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('\t'))
            .map(|(it, en)| (unescape(it), unescape(en)))
            .collect()
    })
}

/// La lingua del sistema è l'italiano? (LC_ALL, LC_MESSAGES, LANG come fa gettext)
fn system_is_italian() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .is_some_and(|l| l.starts_with("it"))
}

/// Imposta la lingua dalla config: "it", "en" o "auto" (quella del sistema; inglese se non è l'italiano).
pub fn set(lang: &str) {
    let en = match lang {
        "it" => false,
        "en" => true,
        _ => !system_is_italian(),
    };
    ENGLISH.store(en, Ordering::Relaxed);
}

#[cfg(test)]
thread_local! {
    /// Nei test la lingua vale solo per il thread che la imposta: i test girano in parallelo.
    pub static TEST_ENGLISH: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

pub fn english() -> bool {
    #[cfg(test)]
    if let Some(en) = TEST_ENGLISH.with(std::cell::Cell::get) {
        return en;
    }
    ENGLISH.load(Ordering::Relaxed)
}

/// Testo tradotto nella lingua attuale.
pub fn t(s: &str) -> &str {
    if !english() {
        return s;
    }
    table().get(s).map_or(s, String::as_str)
}

/// Sostituisce, in ordine, ogni `{}` del modello con un valore.
pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut args = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        out.push_str(args.next().map_or("", String::as_str));
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// `tr!("testo")` = `t("testo")`; `tr!("Pagina {}", n)` = testo tradotto con i valori inseriti.
#[macro_export]
macro_rules! tr {
    ($s:literal) => {
        $crate::i18n::t($s)
    };
    ($s:literal, $($a:expr),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::t($s), &[$(format!("{}", $a)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Testi tra virgolette dopo `t(` o `tr!(` in tutti i sorgenti.
    fn used_strings() -> Vec<(String, String)> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_string_lossy().to_string();
            if file == "i18n.rs" {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            for start in ["t(\"", "tr!(\""] {
                let mut rest = src.as_str();
                while let Some(i) = rest.find(start) {
                    // Solo `t(` e `tr!(` a sé, non `get(` o `include_str!(`.
                    let before = rest[..i].chars().last();
                    rest = &rest[i + start.len()..];
                    if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                        continue;
                    }
                    let mut s = String::new();
                    let mut chars = rest.chars();
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => break,
                            '\\' => match chars.next() {
                                Some('n') => s.push('\n'),
                                // `\` a fine riga: Rust salta l'a capo e gli spazi che seguono.
                                Some('\n') => {
                                    while chars.clone().next().is_some_and(char::is_whitespace) {
                                        chars.next();
                                    }
                                }
                                Some(o) => s.push(o),
                                None => {}
                            },
                            c => s.push(c),
                        }
                    }
                    out.push((file.clone(), s));
                }
            }
        }
        out
    }

    #[test]
    fn every_text_has_an_english_translation() {
        let missing: Vec<String> = used_strings().into_iter().filter(|(_, s)| !table().contains_key(s)).map(|(f, s)| format!("{f}: {s}")).collect();
        assert!(missing.is_empty(), "{} testi senza traduzione:\n{}", missing.len(), missing.join("\n"));
    }

    /// Le pagine d'esempio della guida rapida vengono tradotte: nomi ed etichette.
    #[test]
    fn sample_config_is_translated() {
        let cfg: crate::config::Config = toml::from_str(crate::config::DEFAULT_CONFIG).unwrap();
        let missing: Vec<&str> = cfg
            .pages
            .iter()
            .flat_map(|p| std::iter::once(p.name.as_str()).chain(p.keys.iter().filter_map(|k| k.label.as_deref())))
            .filter(|s| !s.is_empty() && !table().contains_key(*s))
            .collect();
        assert!(missing.is_empty(), "senza traduzione: {missing:?}");
    }

    #[test]
    fn placeholders_match() {
        for (it, en) in table() {
            assert_eq!(it.matches("{}").count(), en.matches("{}").count(), "segnaposto diversi: {it} → {en}");
        }
    }

    #[test]
    fn fills_in_order() {
        assert_eq!(fill("{} di {}", &["1".into(), "3".into()]), "1 di 3");
        assert_eq!(fill("niente", &[]), "niente");
    }
}
