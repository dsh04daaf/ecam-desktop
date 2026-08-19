//! Qué hacer cuando algo falla. Es el mapa de errores que el bot aprendió a
//! golpes, portado tal cual.
//!
//! Lo importante: **una sesión FairPlay caída NO es un fallo de la descarga**.
//! El wrapper sigue autenticado (las DBs de cuenta están intactas), solo se le
//! murió el contexto de FairPlay. Se relanza, se reintenta, y el usuario ni se
//! entera. Antes, en la app, eso tumbaba el track y lo reportaba como error.

use crate::error::{Error, FailKind};
use std::time::Duration;

/// Qué hacer con un fallo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Reintentar tal cual, esperando un poco.
    Retry,
    /// Relanzar el wrapper y reintentar. El wrapper conserva la sesión: NO pide
    /// 2FA otra vez. (En el bot esto es `docker restart`, nunca `rm + run`,
    /// justo por eso.)
    RestartWrapperAndRetry,
    /// No tiene arreglo reintentando: se reporta y se sigue con lo siguiente.
    GiveUp,
}

/// Cuántas veces se reintenta una misma pista antes de rendirse.
pub const MAX_ATTEMPTS: u32 = 3;

/// Espera antes del siguiente intento, creciendo un poco cada vez.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempt.min(3)))
}

/// Errores del wrapper que significan "sesión FairPlay muerta, relánzalo".
///
/// Sale de `WRAPPER_FATAL_ERRORS` del bot más el -42786, que es el que aparece
/// cuando la licencia caduca a media descarga.
pub const SESSION_DEAD_MARKERS: &[&str] = &[
    "-42786",
    "Invalid CKC",
    "catched an exception",
    "Error connecting to device",
    "Error reading response from device",
    "Error writing length to device",
];

pub fn looks_session_dead(text: &str) -> bool {
    SESSION_DEAD_MARKERS.iter().any(|m| text.contains(m))
}

pub fn classify(err: &Error) -> Action {
    match err {
        // El descifrado devolvió basura: la sesión está viva pero corrupta.
        // Reintentar con la misma sesión da la misma basura.
        Error::DecryptionCorrupted(_) => Action::RestartWrapperAndRetry,

        // El wrapper no contesta o cortó: mismo caso.
        Error::WrapperUnreachable(_) => Action::RestartWrapperAndRetry,

        Error::Track(t) => match t.kind {
            FailKind::WrapperDead => Action::RestartWrapperAndRetry,
            // Red: se reintenta sin tocar el wrapper.
            FailKind::Transient => Action::Retry,
            // Territorio, sin stream, sin suscripción: reintentar no cambia nada.
            FailKind::Unavailable => Action::GiveUp,
            FailKind::Failed => {
                if looks_session_dead(&t.reason) {
                    Action::RestartWrapperAndRetry
                } else {
                    Action::GiveUp
                }
            }
        },

        // Cancelado por el usuario: no se reintenta nada.
        Error::Cancelled => Action::GiveUp,

        // Lo demás (metadata, config, disco) no se arregla insistiendo.
        _ => Action::GiveUp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TrackError;

    #[test]
    fn una_sesion_muerta_relanza_el_wrapper_en_vez_de_fallar() {
        let e = Error::Track(TrackError::wrapper_dead("el wrapper cortó la conexión"));
        assert_eq!(classify(&e), Action::RestartWrapperAndRetry);
        assert_eq!(classify(&Error::DecryptionCorrupted("alac".into())), Action::RestartWrapperAndRetry);
        assert_eq!(classify(&Error::WrapperUnreachable("127.0.0.1:10020".into())), Action::RestartWrapperAndRetry);
    }

    #[test]
    fn el_42786_se_reconoce_venga_donde_venga() {
        assert!(looks_session_dead("[!] auth error: code=-42786"));
        assert!(looks_session_dead("KDCanProcessCKC Invalid CKC"));
        assert!(!looks_session_dead("[!] listening 0.0.0.0:10020"));
        let e = Error::Track(TrackError::failed("falló algo con -42786 dentro"));
        assert_eq!(classify(&e), Action::RestartWrapperAndRetry);
    }

    #[test]
    fn lo_que_no_tiene_arreglo_no_se_reintenta() {
        // Territorio o sin stream: insistir solo hace perder el tiempo, y el bot
        // aprendió eso a base de reintentar seis veces para nada.
        assert_eq!(classify(&Error::Track(TrackError::unavailable("Territory restricted"))), Action::GiveUp);
        assert_eq!(classify(&Error::Cancelled), Action::GiveUp);
        assert_eq!(classify(&Error::NotFound), Action::GiveUp);
    }

    #[test]
    fn un_fallo_de_red_se_reintenta_sin_tocar_el_wrapper() {
        assert_eq!(classify(&Error::Track(TrackError::transient("timeout"))), Action::Retry);
    }

    #[test]
    fn la_espera_crece_pero_no_sin_limite() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(4));
        assert_eq!(backoff(9), Duration::from_secs(8), "el tope evita esperas absurdas");
    }
}
