//! Historial de descargas en disco.
//!
//! La lista de Descargas vivía solo en memoria: cerrabas la app y desaparecía
//! todo, incluido qué falló y por qué. Mismo planteamiento que en ECBP.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Se guardan las últimas N: sin tope el archivo crece para siempre.
const MAX_ENTRIES: usize = 300;

/// Una pista que no salió, con su motivo. Es lo que permite responder
/// "¿por qué no bajó esta?" una semana después.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Failure {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// Segundos desde el epoch: la UI decide cómo mostrarlo según el idioma.
    pub at: i64,
    pub name: String,
    pub kind: String,
    pub quality: String,
    pub ok: usize,
    pub skipped: usize,
    /// Carpeta donde quedó, para poder abrirla desde la app.
    pub folder: String,
    pub failed: Vec<Failure>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub seconds: f32,
}

fn path() -> PathBuf {
    crate::config::Config::config_dir().join("history.json")
}

pub fn load() -> Vec<Entry> {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(all: &[Entry]) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(all) {
        let _ = std::fs::write(p, text);
    }
}

pub fn append(entry: Entry) {
    let mut all = load();
    all.insert(0, entry);
    all.truncate(MAX_ENTRIES);
    save(&all);
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn clear() {
    let _ = std::fs::remove_file(path());
}

/// Quita una entrada suelta: limpiar lo ya revisado sin perder el resto.
pub fn remove(id: &str) {
    let mut all = load();
    all.retain(|e| e.id != id);
    save(&all);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_entrada_sin_fallos_se_serializa_y_vuelve() {
        let e = Entry {
            id: "1".into(), at: now(), name: "Discovery".into(), kind: "album".into(),
            quality: "ALAC".into(), ok: 14, skipped: 0, folder: "/musica".into(),
            failed: vec![], cancelled: false, seconds: 42.0,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ok, 14);
        assert_eq!(back.name, "Discovery");
    }

    #[test]
    fn una_entrada_vieja_sin_campos_nuevos_sigue_leyendose() {
        // Las entradas guardadas antes de añadir `cancelled` y `seconds` no
        // pueden romper el historial de nadie.
        let viejo = r#"{"id":"1","at":0,"name":"x","kind":"album","quality":"ALAC",
                        "ok":1,"skipped":0,"folder":"/m","failed":[]}"#;
        let e: Entry = serde_json::from_str(viejo).unwrap();
        assert!(!e.cancelled);
        assert_eq!(e.seconds, 0.0);
    }
}
