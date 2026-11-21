//! Limiti d'uso di Claude (sessione di 5 ore e settimana), presi dalla status line di Claude Code.
//!
//! Claude Code passa allo script `statusLine` un JSON con `rate_limits` (solo per gli
//! abbonamenti Pro/Max, dopo la prima risposta della sessione). `cyd-deck statusline`
//! lo salva in un file che il demone legge.

use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Una finestra di utilizzo: percentuale usata e istante di azzeramento (secondi Unix).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub used: f32,
    pub resets_at: i64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Limits {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// Quando Claude Code li ha riportati (secondi Unix).
    pub updated: i64,
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

pub fn path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/cyd-deck/claude-limits.json")
}

fn window(v: &Value) -> Option<Window> {
    Some(Window { used: v["used_percentage"].as_f64()? as f32, resets_at: v["resets_at"].as_i64()? })
}

pub fn parse(json: &Value, updated: i64) -> Limits {
    let r = &json["rate_limits"];
    Limits { five_hour: window(&r["five_hour"]), seven_day: window(&r["seven_day"]), updated }
}

/// Ultimi limiti salvati, o None se Claude Code non li ha mai riportati.
pub fn load() -> Option<Limits> {
    let v: Value = serde_json::from_slice(&std::fs::read(path()).ok()?).ok()?;
    let l = parse(&v, v["saved_at"].as_i64().unwrap_or(0));
    (l.five_hour.is_some() || l.seven_day.is_some()).then_some(l)
}

impl Window {
    /// Dopo l'azzeramento la finestra riparte da zero.
    pub fn used_now(&self, now: i64) -> f32 {
        if now >= self.resets_at { 0.0 } else { self.used }
    }
}

/// Tempo che manca, in breve: "2 h 13 min", "3 g 4 h", "45 min".
pub fn until(resets_at: i64, now: i64) -> String {
    let s = (resets_at - now).max(0);
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    match (d, h) {
        (0, 0) => tr!("{} min", m.max(1)),
        (0, _) => tr!("{} h {} min", h, m),
        _ => tr!("{} g {} h", d, h),
    }
}

/// `cyd-deck statusline`: comando per la status line di Claude Code. Salva i limiti (se ci sono)
/// e stampa una riga breve: modello e consumo.
pub fn statusline() -> anyhow::Result<()> {
    crate::i18n::set("auto");
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let mut v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let l = parse(&v, now());
    if l.five_hour.is_some() || l.seven_day.is_some() {
        // Solo i limiti: il resto del JSON (cartelle, sessione) non serve al deck.
        let saved = serde_json::json!({ "rate_limits": v["rate_limits"].take(), "saved_at": l.updated });
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&saved)?)?;
        std::fs::rename(&tmp, &p)?;
    }
    let mut parts = Vec::new();
    if let Some(m) = v["model"]["display_name"].as_str() {
        parts.push(m.to_string());
    }
    let t = now();
    if let Some(w) = l.five_hour {
        parts.push(format!("5h {:.0}% (−{})", w.used_now(t), until(w.resets_at, t)));
    }
    if let Some(w) = l.seven_day {
        parts.push(tr!("sett. {}%", format!("{:.0}", w.used_now(t))));
    }
    println!("{}", parts.join(" · "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rate_limits() {
        let v: Value = serde_json::from_str(
            r#"{"model": {"display_name": "Opus"}, "rate_limits": {
                "five_hour": {"used_percentage": 23.5, "resets_at": 1000},
                "seven_day": {"used_percentage": 41.2, "resets_at": 400000}}}"#,
        )
        .unwrap();
        let l = parse(&v, 5);
        assert_eq!(l.five_hour, Some(Window { used: 23.5, resets_at: 1000 }));
        assert_eq!(l.seven_day.unwrap().used, 41.2);
        assert_eq!((l.five_hour.unwrap().used_now(999), l.five_hour.unwrap().used_now(1000)), (23.5, 0.0));
        // Senza abbonamento (o prima della prima risposta) non c'è niente.
        assert_eq!(parse(&serde_json::json!({}), 0), Limits { updated: 0, ..Limits::default() });
        assert_eq!((until(1000, 0), until(8000, 0), until(300000, 0)), ("16 min".to_string(), "2 h 13 min".to_string(), "3 g 11 h".to_string()));
    }
}
