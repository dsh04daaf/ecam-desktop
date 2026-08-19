//! Errores del core. La UI nunca ve un `Box<dyn Error>` pelón: cada fallo llega
//! ya clasificado, porque la acción que toca no es la misma en cada caso.

use std::fmt;

/// Qué hacer con el fallo. Es la diferencia que el bot aprendió a golpes:
/// un track no disponible se reporta y se sigue, pero una sesión FairPlay muerta
/// obliga a **relanzar el wrapper** — reintentar el track no arregla nada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FailKind {
    /// Problema de disponibilidad (región, no publicado, sin suscripción).
    Unavailable,
    /// Fallo transitorio: red, timeout. Se puede reintentar.
    Transient,
    /// La sesión del wrapper está muerta o corrupta. Relanzar el wrapper.
    WrapperDead,
    /// Error real de la descarga o del archivo.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TrackError {
    pub reason: String,
    pub kind: FailKind,
}

impl TrackError {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self { reason: reason.into(), kind: FailKind::Unavailable }
    }
    pub fn transient(reason: impl Into<String>) -> Self {
        Self { reason: reason.into(), kind: FailKind::Transient }
    }
    pub fn wrapper_dead(reason: impl Into<String>) -> Self {
        Self { reason: reason.into(), kind: FailKind::WrapperDead }
    }
    pub fn failed(reason: impl Into<String>) -> Self {
        Self { reason: reason.into(), kind: FailKind::Failed }
    }
}

impl fmt::Display for TrackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}
impl std::error::Error for TrackError {}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Api(String),
    #[error(transparent)]
    Track(#[from] TrackError),
    /// El descifrado devolvió basura sin lanzar error de red. Una sesión FairPlay
    /// caducada NO falla: entrega ruido. Ver `mp4::frag::validate_sample`.
    #[error("sesión FairPlay corrupta: {0}")]
    DecryptionCorrupted(String),
    #[error("el wrapper no responde en {0} — ¿está encendido?")]
    WrapperUnreachable(String),
    #[error("no encontrado")]
    NotFound,
    #[error("se necesita media-user-token (suscripción) para esto")]
    NeedsUserToken,
    #[error("MP4 inválido: {0}")]
    Mp4(String),
    #[error("error de E/S: {0}")]
    Io(#[from] std::io::Error),
    #[error("config inválida: {0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() || e.is_connect() {
            Error::Track(TrackError::transient(clean(e.to_string())))
        } else {
            Error::Api(clean(e.to_string()))
        }
    }
}

/// Quita URLs de un mensaje antes de enseñarlo: llevan tokens firmados.
pub fn clean(msg: impl AsRef<str>) -> String {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"https?://\S+").unwrap());
    let out = RE.replace_all(msg.as_ref(), "[url]").trim().to_string();
    if out.is_empty() { "error".into() } else { out }
}
