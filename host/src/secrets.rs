//! Segreti (password del Wi-Fi, token di Home Assistant) nel portachiavi di sistema (Secret Service).
//! Non finiscono mai nella config. Con il portachiavi dedicato "cyd-deck", che ha una password sua,
//! restano chiusi finché non lo sblocchi dalla finestra che compare al primo uso.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use secret_service::EncryptionType;
use secret_service::blocking::{Collection, SecretService};

pub const WIFI_PASSWORD: &str = "wifi-password";
pub const HA_TOKEN: &str = "ha-token";
pub const BAMBU_CODE: &str = "bambu-code";

/// Nome del portachiavi dedicato.
const COLLECTION: &str = "cyd-deck";
/// Dopo una finestra di sblocco chiusa, quelle automatiche aspettano tanto così.
const RETRY_AFTER: Duration = Duration::from_secs(600);

/// Se si può chiedere la password del portachiavi quando è chiuso.
#[derive(Clone, Copy, PartialEq)]
pub enum Prompt {
    /// Mai: se è chiuso il segreto risulta non disponibile.
    Never,
    /// Sì, ma non più di una finestra alla volta e non subito dopo che l'hai chiusa.
    Auto,
    /// Sì: l'hai chiesto tu (un pulsante nell'editor).
    Always,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    /// Niente portachiavi dedicato: i segreti stanno in quello di login.
    Missing,
    Locked,
    Unlocked,
}

/// Finestra di sblocco in corso e quando è stata chiusa senza sbloccare.
static PROMPTING: Mutex<()> = Mutex::new(());
static DISMISSED: Mutex<Option<Instant>> = Mutex::new(None);

fn connect() -> Result<SecretService<'static>> {
    SecretService::connect(EncryptionType::Dh).map_err(|e| anyhow!("{}", tr!("portachiavi non disponibile: {}", e)))
}

fn find<'a>(ss: &'a SecretService<'a>) -> Result<Option<Collection<'a>>> {
    for c in ss.get_all_collections()? {
        if c.get_label()? == COLLECTION {
            return Ok(Some(c));
        }
    }
    Ok(None)
}

fn attrs(key: &str) -> HashMap<&str, &str> {
    HashMap::from([("service", "cyd-deck"), ("key", key)])
}

pub fn status() -> Result<Status> {
    let ss = connect()?;
    Ok(match find(&ss)? {
        None => Status::Missing,
        Some(c) if c.is_locked()? => Status::Locked,
        Some(_) => Status::Unlocked,
    })
}

/// Apre il portachiavi dedicato se è chiuso, secondo `prompt`.
fn open(c: &Collection, prompt: Prompt) -> Result<()> {
    if !c.is_locked()? {
        return Ok(());
    }
    let recently = DISMISSED.lock().unwrap().is_some_and(|t| t.elapsed() < RETRY_AFTER);
    let _guard = match prompt {
        Prompt::Never => bail!("{}", tr!("portachiavi cyd-deck chiuso")),
        Prompt::Auto if recently => bail!("{}", tr!("portachiavi cyd-deck chiuso")),
        Prompt::Auto => PROMPTING.try_lock().map_err(|_| anyhow!("{}", tr!("portachiavi cyd-deck: in attesa della password")))?,
        Prompt::Always => PROMPTING.lock().unwrap(),
    };
    // Un altro thread potrebbe averlo appena aperto.
    if !c.is_locked()? {
        return Ok(());
    }
    if c.unlock().is_err() || c.is_locked()? {
        *DISMISSED.lock().unwrap() = Some(Instant::now());
        bail!("{}", tr!("portachiavi cyd-deck non sbloccato"));
    }
    *DISMISSED.lock().unwrap() = None;
    Ok(())
}

/// Valore salvato: Ok(None) se non c'è, Err se il portachiavi è chiuso o non risponde.
pub fn get(key: &str, prompt: Prompt) -> Result<Option<String>> {
    let ss = connect()?;
    let secret = match find(&ss)? {
        Some(c) => {
            open(&c, prompt)?;
            c.search_items(attrs(key))?.first().map(|i| i.get_secret()).transpose()?
        }
        // Senza portachiavi dedicato: quello di login, già aperto.
        None => ss.search_items(attrs(key))?.unlocked.first().map(|i| i.get_secret()).transpose()?,
    };
    let Some(secret) = secret else { return Ok(None) };
    let v = String::from_utf8(secret)?;
    Ok((!v.is_empty()).then_some(v))
}

/// C'è un valore salvato? Non apre il portachiavi e non legge il segreto.
pub fn has(key: &str) -> bool {
    connect().and_then(|ss| Ok(ss.search_items(attrs(key)).map(|r| !r.unlocked.is_empty() || !r.locked.is_empty())?)).unwrap_or(false)
}

/// Salva il valore (vuoto = cancella la voce); se serve chiede di aprire il portachiavi.
pub fn set(key: &str, value: &str) -> Result<()> {
    let ss = connect()?;
    let c = match find(&ss)? {
        Some(c) => {
            open(&c, Prompt::Always)?;
            c
        }
        None => ss.get_default_collection()?,
    };
    for item in c.search_items(attrs(key))? {
        item.delete()?;
    }
    if !value.is_empty() {
        c.create_item(&format!("cyd-deck: {key}"), attrs(key), value.as_bytes(), true, "text/plain")?;
    }
    Ok(())
}

/// Crea il portachiavi dedicato (la finestra chiede la nuova password) e ci sposta i segreti.
pub fn create() -> Result<()> {
    let ss = connect()?;
    if find(&ss)?.is_some() {
        bail!("{}", tr!("il portachiavi cyd-deck esiste già"));
    }
    let c = ss.create_collection(COLLECTION, "").map_err(|e| anyhow!("{}", tr!("portachiavi non creato: {}", e)))?;
    for key in [WIFI_PASSWORD, HA_TOKEN, BAMBU_CODE] {
        let old = ss.search_items(attrs(key))?;
        for item in old.unlocked {
            if item.item_path.as_str().starts_with(c.collection_path.as_str()) {
                continue;
            }
            c.create_item(&format!("cyd-deck: {key}"), attrs(key), &item.get_secret()?, true, "text/plain")?;
            item.delete()?;
        }
    }
    Ok(())
}

/// Sblocca il portachiavi dedicato chiedendo la password.
pub fn unlock() -> Result<()> {
    let ss = connect()?;
    let c = find(&ss)?.ok_or_else(|| anyhow!("{}", tr!("il portachiavi cyd-deck non esiste")))?;
    open(&c, Prompt::Always)
}

pub fn lock() -> Result<()> {
    let ss = connect()?;
    let c = find(&ss)?.ok_or_else(|| anyhow!("{}", tr!("il portachiavi cyd-deck non esiste")))?;
    c.lock()?;
    Ok(())
}
