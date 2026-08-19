//! Etiquetas del .m4a. El mapa es el mismo que usaba el bot: si cambia, las
//! bibliotecas de la gente dejan de agrupar bien lo ya descargado.

use crate::error::Result;
use mp4ameta::{Data, FreeformIdent, Fourcc, Img, ImgFmt, Tag};
use serde_json::Value;
use std::path::Path;

fn s(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

/// Escribe las etiquetas sobre el archivo ya montado.
pub fn write(
    path: &Path,
    track: &Value,
    album: &Value,
    cover: Option<&[u8]>,
    lyrics: Option<&str>,
) -> Result<()> {
    let mut tag = Tag::read_from_path(path).unwrap_or_default();

    tag.set_title(s(track, "name"));
    tag.set_artist(s(track, "artistName"));
    // El artista del álbum manda sobre el del track: es lo que agrupa los
    // recopilatorios en una sola entrada en vez de una por tema.
    let album_artist = if album["artistName"].is_string() {
        s(album, "artistName")
    } else {
        s(track, "artistName")
    };
    tag.set_album_artist(album_artist);

    // Un music video no tiene álbum: escribir la etiqueta vacía deja un campo
    // en blanco en la biblioteca, que se ve peor que no tenerlo.
    let album_name = if track["albumName"].is_string() { s(track, "albumName") } else { s(album, "name") };
    if !album_name.is_empty() {
        tag.set_album(album_name);
    }

    let release = if album["releaseDate"].is_string() { s(album, "releaseDate") } else { s(track, "releaseDate") };
    let year: String = release.chars().take(4).collect();
    if !year.is_empty() {
        tag.set_year(year);
    }

    let composer = s(track, "composerName");
    if !composer.is_empty() {
        tag.set_composer(composer);
    }

    // Solo el primer género: Apple manda una lista jerárquica ("Dance",
    // "Electronic", "Music") y meterlas todas ensucia la biblioteca.
    let genre = track["genreNames"]
        .as_array()
        .or_else(|| album["genreNames"].as_array())
        .and_then(|g| g.first())
        .and_then(Value::as_str);
    if let Some(g) = genre {
        tag.set_genre(g);
    }

    // Número de pista y disco solo si de verdad vienen: los videos no los traen.
    if let Some(n) = track["trackNumber"].as_u64() {
        tag.set_track(n as u16, album["trackCount"].as_u64().unwrap_or(0) as u16);
    }
    if let Some(d) = track["discNumber"].as_u64() {
        tag.set_disc(d as u16, 0);
    }

    let copyright = s(album, "copyright");
    if !copyright.is_empty() {
        tag.set_copyright(copyright);
    }

    for (name, value) in [
        ("LABEL", s(album, "recordLabel")),
        ("UPC", s(album, "upc")),
        ("ISRC", s(track, "isrc")),
    ] {
        if !value.is_empty() {
            tag.set_data(FreeformIdent::new("com.apple.iTunes", name), Data::Utf8(value));
        }
    }

    // rtng: 1 = explícito, 2 = limpio. Es un entero de un byte, no texto.
    match track["contentRating"].as_str() {
        Some("explicit") => tag.set_data(Fourcc(*b"rtng"), Data::BeSigned(vec![1])),
        Some("clean") => tag.set_data(Fourcc(*b"rtng"), Data::BeSigned(vec![2])),
        _ => {}
    }

    if let Some(bytes) = cover {
        tag.set_artwork(Img { fmt: ImgFmt::Jpeg, data: bytes.to_vec() });
    }
    if let Some(l) = lyrics {
        tag.set_lyrics(l);
    }

    tag.write_to_path(path)
        .map_err(|e| crate::error::Error::Other(format!("no se pudieron escribir las etiquetas: {e}")))?;
    Ok(())
}
