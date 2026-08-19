//! Nombres de archivo y carpeta. Las plantillas son las del bot y los
//! marcadores se llaman igual: cambiarlos rompería configs existentes.

use crate::config::{Config, Quality};
use once_cell::sync::Lazy;
use regex::Regex;

/// Caracteres que ningún sistema de archivos acepta. Se sustituyen, no se
/// borran: quitarlos pega palabras que iban separadas.
static FORBIDDEN: Lazy<Regex> = Lazy::new(|| Regex::new(r#"[/\\<>:"|?*]"#).unwrap());

/// Limpia y **recorta por bytes, no por caracteres**: el límite de los sistemas
/// de archivos es en bytes y un nombre en japonés se pasa mucho antes de lo que
/// parece contando letras.
pub fn sanitize(s: &str, max_bytes: usize) -> String {
    let cleaned = FORBIDDEN.replace_all(s.trim(), "_").to_string();
    if cleaned.len() <= max_bytes {
        return cleaned;
    }
    let mut end = max_bytes;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].trim_end().to_string()
}

pub fn apply(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// La etiqueta de contenido que va en el nombre.
pub fn rating_tag(cfg: &Config, content_rating: &str) -> String {
    match content_rating {
        "explicit" => cfg.explicit_choice.clone(),
        "clean" => cfg.clean_choice.clone(),
        _ => String::new(),
    }
}

/// Nombre del archivo de un track.
///
/// `disc_override` existe por las playlists: sus tracks vienen de álbumes
/// distintos, así que el disco real de cada uno produce nombres desconcertantes
/// (1.01, 2.06, 3.09…). Se normaliza a 1 **solo en el nombre**; la etiqueta
/// embebida conserva el disco real del álbum de origen.
#[allow(clippy::too_many_arguments)]
pub fn track_filename(
    cfg: &Config,
    song_id: &str,
    name: &str,
    track_num: u32,
    disc_num: u32,
    disc_override: Option<u32>,
    track_total: u32,
    quality: Quality,
    content_rating: &str,
) -> String {
    let pad = std::cmp::max(2, track_total.to_string().len());
    let numbered = format!("{:0pad$}", track_num, pad = pad);
    let codec = quality.display().to_string();
    let vars = [
        ("SongId", song_id.to_string()),
        // El original escribe "SongNumer" (sin la 'b'). Está así en los config.yaml
        // de la gente, así que se aceptan las dos formas y no se rompe nada.
        ("SongNumer", numbered.clone()),
        ("SongNumber", numbered.clone()),
        ("SongName", name.to_string()),
        ("DiscNumber", disc_override.unwrap_or(disc_num).to_string()),
        ("TrackNumber", numbered),
        ("Quality", codec.clone()),
        ("Codec", codec),
        ("Tag", rating_tag(cfg, content_rating)),
    ];
    format!("{}.m4a", sanitize(&apply(&cfg.song_file_format, &vars), 200))
}

pub fn album_folder(cfg: &Config, album: &serde_json::Value, quality: Quality) -> String {
    let a = &album["attributes"];
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let release = s("releaseDate");
    let vars = [
        ("AlbumId", album["id"].as_str().unwrap_or("").to_string()),
        ("AlbumName", s("name")),
        ("ArtistName", s("artistName")),
        ("ReleaseDate", release.clone()),
        ("ReleaseYear", release.chars().take(4).collect()),
        ("UPC", s("upc")),
        ("Copyright", s("copyright")),
        ("RecordLabel", s("recordLabel")),
        ("Quality", quality.display().to_string()),
        ("Codec", quality.display().to_string()),
        ("Tag", rating_tag(cfg, a["contentRating"].as_str().unwrap_or(""))),
    ];
    sanitize(&apply(&cfg.album_folder_format, &vars), 200)
}

pub fn playlist_folder(cfg: &Config, id: &str, name: &str, quality: Quality) -> String {
    let vars = [
        ("PlaylistId", id.to_string()),
        ("PlaylistName", name.to_string()),
        ("ArtistName", String::new()),
        ("Quality", quality.display().to_string()),
        ("Codec", quality.display().to_string()),
        ("Tag", String::new()),
    ];
    sanitize(&apply(&cfg.playlist_folder_format, &vars), 200)
}

/// Carpeta del artista. Si la plantilla está vacía, **no se crea carpeta** —
/// es la forma documentada de desactivarla.
pub fn artist_folder(cfg: &Config, id: &str, name: &str) -> Option<String> {
    if cfg.artist_folder_format.trim().is_empty() {
        return None;
    }
    let vars = [
        ("ArtistId", id.to_string()),
        ("ArtistName", name.to_string()),
        ("UrlArtistName", name.to_string()),
    ];
    Some(sanitize(&apply(&cfg.artist_folder_format, &vars), 200))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_caracteres_prohibidos_se_sustituyen() {
        assert_eq!(sanitize("AC/DC: Live?", 200), "AC_DC_ Live_");
    }

    #[test]
    fn el_recorte_es_por_bytes_y_no_parte_un_caracter() {
        let s = "ハローワールド"; // 3 bytes por carácter
        let cut = sanitize(s, 7);
        assert!(cut.len() <= 7);
        assert_eq!(cut, "ハロ", "no debe quedar medio carácter");
    }

    #[test]
    fn el_numero_se_rellena_segun_el_total_del_album() {
        let cfg = Config::default();
        let f = track_filename(&cfg, "1", "Tema", 7, 1, None, 120, Quality::Alac, "");
        assert_eq!(f, "007. Tema.m4a", "120 tracks → tres dígitos");
        let f = track_filename(&cfg, "1", "Tema", 7, 1, None, 9, Quality::Alac, "");
        assert_eq!(f, "07. Tema.m4a", "mínimo dos dígitos siempre");
    }

    #[test]
    fn en_playlist_el_disco_del_nombre_se_normaliza() {
        let mut cfg = Config::default();
        cfg.song_file_format = "{DiscNumber}.{TrackNumber} {SongName}".into();
        let f = track_filename(&cfg, "1", "Tema", 6, 3, Some(1), 20, Quality::Alac, "");
        assert_eq!(f, "1.06 Tema.m4a");
    }

    #[test]
    fn la_etiqueta_de_explicito_sale_del_config() {
        let cfg = Config::default();
        assert_eq!(rating_tag(&cfg, "explicit"), "[E]");
        assert_eq!(rating_tag(&cfg, "clean"), "[C]");
        assert_eq!(rating_tag(&cfg, ""), "");
    }

    #[test]
    fn sin_plantilla_de_artista_no_hay_carpeta() {
        let mut cfg = Config::default();
        cfg.artist_folder_format = "".into();
        assert!(artist_folder(&cfg, "1", "Alguien").is_none());
    }
}
