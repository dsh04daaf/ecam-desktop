//! ECAM Core — motor de descarga de Apple Music.
//!
//! Port a Rust de `apple-music-downloader`. Igual que el core de ECBP, **no**
//! depende de Tauri: así se compila y se prueba aquí, en Linux, y la carcasa de
//! Windows queda delgada.
//!
//! El comportamiento no se "moderniza": cada decisión rara que venía del bot está
//! documentada en `docs/INVENTARIO_CORE.md` con el motivo. Lo único que cambia a
//! propósito es el uso de memoria (ver `mp4::assemble`).

pub mod amp;
pub mod artwork;
pub mod cancel;
pub mod collection;
pub mod config;
pub mod error;
pub mod history;
pub mod hls;
pub mod lyrics;
pub mod mp4;
pub mod mv;
pub mod naming;
pub mod preview;
pub mod recovery;
pub mod runtime;
pub mod tags;
pub mod track;
pub mod wrapper;

pub use config::{Config, Quality};
pub use error::{Error, Result};
