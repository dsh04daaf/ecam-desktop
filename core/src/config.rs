//! Config del core. Los defaults NO son gustos: son las decisiones que ya venían
//! del bot (ver INVENTARIO_CORE.md sección C) y cambiarlas cambia lo que se baja.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Calidad de audio pedida. El nombre viaja tal cual a la selección de variante HLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Alac,
    Aac,
    Atmos,
    Binaural,
}

impl Quality {
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Alac => "alac",
            Quality::Aac => "aac",
            Quality::Atmos => "atmos",
            Quality::Binaural => "binaural",
        }
    }
    /// Cómo se muestra en carpetas y etiquetas.
    pub fn display(self) -> &'static str {
        match self {
            Quality::Alac => "ALAC",
            Quality::Aac => "AAC",
            Quality::Atmos => "Atmos",
            Quality::Binaural => "Binaural",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// Debe coincidir con el país de la cuenta o casi todas las letras fallan.
    pub storefront: String,
    pub language: String,
    /// Sin esto no hay letras ni AAC-LC. No es opcional en la práctica.
    pub media_user_token: String,

    /// Techos, no selectores: se toma la mejor variante que quepa debajo.
    pub alac_max: u32,
    pub atmos_max: u32,
    pub aac_type: String,
    pub mv_max: u32,
    pub mv_audio_type: String,

    /// `host:puerto` del wrapper. En Windows apunta al WSL vía localhost.
    pub decrypt_port: String,

    pub output_dir: PathBuf,
    pub cover_size: String,
    pub save_cover: bool,
    pub save_animated_artwork: bool,
    pub save_lrc: bool,
    pub embed_lrc: bool,

    pub album_folder_format: String,
    pub playlist_folder_format: String,
    pub artist_folder_format: String,
    pub song_file_format: String,
    pub explicit_choice: String,
    pub clean_choice: String,

    /// Ruta al ffmpeg. Solo hace falta para el artwork animado.
    pub ffmpeg_path: PathBuf,
    /// Llave de dispositivo Widevine, en PEM y **fuera del binario** (solo MV).
    /// Igual que en ECBP: una credencial dentro de un ejecutable se saca con
    /// `strings`, así que aquí se lee de disco o no hay MV.
    pub widevine_device_key: Option<PathBuf>,
    /// Blob del ClientId de Widevine (base64), por el mismo motivo.
    pub widevine_client_id: Option<PathBuf>,

    /// De dónde se cargó. No se serializa: es para poder reescribir el archivo
    /// cuando el core detecta algo que el usuario no tuvo que teclear.
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // "auto" = preguntarle a la cuenta. Un código concreto se respeta.
            storefront: "auto".into(),
            language: "en-GB".into(),
            media_user_token: String::new(),
            alac_max: 192_000,
            atmos_max: 2768,
            aac_type: "aac".into(),
            mv_max: 2160,
            mv_audio_type: "atmos".into(),
            decrypt_port: "127.0.0.1:10020".into(),
            output_dir: default_output_dir(),
            cover_size: "1200x1200".into(),
            save_cover: true,
            save_animated_artwork: false,
            save_lrc: true,
            embed_lrc: true,
            album_folder_format: "{AlbumName}".into(),
            playlist_folder_format: "{PlaylistName}".into(),
            artist_folder_format: "{UrlArtistName}".into(),
            song_file_format: "{SongNumer}. {SongName}".into(),
            explicit_choice: "[E]".into(),
            clean_choice: "[C]".into(),
            ffmpeg_path: "ffmpeg".into(),
            widevine_device_key: None,
            widevine_client_id: None,
            source_path: None,
        }
    }
}

fn default_output_dir() -> PathBuf {
    dirs::audio_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ECAM")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("no se pudo leer {}: {e}", path.display())))?;
        let mut cfg: Config = serde_yaml::from_str(&text)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        cfg.source_path = Some(path.to_path_buf());
        Ok(cfg)
    }

    /// Carga el config del sitio de siempre, creándolo con los defaults si es la
    /// primera vez. Así el usuario nunca tiene que escribir un YAML a mano.
    pub fn load_or_create() -> Result<Self> {
        let path = Self::default_path();
        if path.exists() {
            return Self::load(&path);
        }
        let mut cfg = Self::default();
        cfg.source_path = Some(path.clone());
        cfg.save(&path)?;
        Ok(cfg)
    }

    /// Reescribe el archivo del que salió, si sabe cuál es.
    pub fn persist(&self) -> Result<()> {
        match &self.source_path {
            Some(p) => self.save(p),
            None => Ok(()),
        }
    }

    /// Guarda el config **de forma atómica**.
    ///
    /// Escribir encima del archivo bueno con `write` deja una ventana en la que,
    /// si la app se cierra o se cruzan dos guardados, el config queda a medias y
    /// al arrancar no se puede leer. Se escribe a un temporal en la MISMA
    /// carpeta (para que el rename no cruce discos) y se renombra encima, que en
    /// los tres sistemas es atómico.
    ///
    /// El bot nunca tuvo este problema porque solo LEE su config; esta app lo
    /// escribe, así que le toca resolverlo.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_yaml::to_string(self).map_err(|e| Error::Config(e.to_string()))?;

        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        {
            use std::io::Write;
            tmp.write_all(text.as_bytes())?;
            tmp.flush()?;
            // Al disco de verdad antes de renombrar: si no, un corte de luz
            // puede dejar el archivo nuevo vacío.
            tmp.as_file().sync_all()?;
        }
        tmp.persist(path).map_err(|e| Error::Io(e.error))?;
        Ok(())
    }

    /// Aplica encima los ajustes que vengan, dejando intacto lo que no venga.
    ///
    /// La ventana manda un objeto con los ajustes que conoce. Si se sustituyera
    /// el config entero por eso, cualquier ajuste que la ventana no pinte
    /// (porque es nuevo, o porque falló al leerlo) volvería a su valor por
    /// defecto sin que nadie lo pida. Mezclando, eso no puede pasar.
    pub fn merge_patch(&self, patch: &serde_json::Value) -> Result<Self> {
        let mut base = serde_json::to_value(self).map_err(|e| Error::Config(e.to_string()))?;
        let (Some(base_map), Some(patch_map)) = (base.as_object_mut(), patch.as_object()) else {
            return Err(Error::Config("los ajustes no son un objeto".into()));
        };
        for (k, v) in patch_map {
            // Solo se aceptan claves que el config ya tiene: una clave inventada
            // no puede colarse ni tirar el resto al fallar la lectura.
            if base_map.contains_key(k) && !v.is_null() {
                base_map.insert(k.clone(), v.clone());
            }
        }
        let mut merged: Config = serde_json::from_value(base).map_err(|e| Error::Config(e.to_string()))?;
        merged.source_path = self.source_path.clone();
        Ok(merged)
    }

    /// Directorio de config por usuario: `%APPDATA%\ECAM` en Windows, XDG en el resto.
    pub fn config_dir() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ECAM")
    }

    pub fn default_path() -> PathBuf {
        Self::config_dir().join("config.yaml")
    }

    /// Un `media-user-token` de menos de 20 caracteres no es un token: es basura
    /// pegada a medias. Se comprueba antes de pedir letras, como en el original.
    pub fn has_user_token(&self) -> bool {
        self.media_user_token.trim().len() >= 20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardar_no_deja_el_config_a_medias() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let cfg = Config::default();
        cfg.save(&path).unwrap();

        // Un segundo guardado no puede dejar restos ni truncar el bueno.
        let mut otro = Config::default();
        otro.language = "ru".into();
        otro.save(&path).unwrap();

        let leido = Config::load(&path).unwrap();
        assert_eq!(leido.language, "ru");
        let sueltos: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten()
            .filter(|e| e.file_name() != "config.yaml").collect();
        assert!(sueltos.is_empty(), "no deben quedar temporales tirados");
    }

    #[test]
    fn un_ajuste_que_no_viene_en_el_parche_no_se_pierde() {
        let mut cfg = Config::default();
        cfg.language = "es-MX".into();
        cfg.alac_max = 96000;

        // La ventana solo manda el idioma.
        let patch = serde_json::json!({ "language": "ru" });
        let nuevo = cfg.merge_patch(&patch).unwrap();

        assert_eq!(nuevo.language, "ru");
        assert_eq!(nuevo.alac_max, 96000, "lo que no vino en el parche sigue igual");
    }

    #[test]
    fn una_clave_inventada_no_rompe_el_guardado() {
        let cfg = Config::default();
        let patch = serde_json::json!({ "no-existe": 1, "language": "en-GB" });
        let nuevo = cfg.merge_patch(&patch).unwrap();
        assert_eq!(nuevo.language, "en-GB");
    }
}
