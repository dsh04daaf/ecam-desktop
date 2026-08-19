//! Cancelación de una descarga en curso.
//!
//! Existía el botón y el comando, pero el core no sabía cancelar: la bandera se
//! guardaba y no la leía nadie. Un mando que no hace nada es peor que no
//! tenerlo, porque el usuario cree que paró algo que sigue corriendo.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Default, Debug)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    /// Atajo para cortar en medio de un bucle.
    pub fn check(&self) -> crate::error::Result<()> {
        if self.is_cancelled() {
            return Err(crate::error::Error::Cancelled);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn una_copia_cancela_a_la_otra() {
        let a = Cancel::new();
        let b = a.clone();
        assert!(a.check().is_ok());
        b.cancel();
        assert!(a.is_cancelled(), "las copias comparten la misma bandera");
        assert!(matches!(a.check(), Err(crate::error::Error::Cancelled)));
    }
}
