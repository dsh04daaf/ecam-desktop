//! Enrutado de URLs y descarga de colecciones (álbum, playlist, artista, room).

use crate::amp::Amp;
use crate::config::{Config, Quality};
use crate::error::{Error, Result};
use crate::track::{download_track, Progress, TrackJob, TrackOutcome};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Qué pidió el usuario. `Song` y `Album` acaban en el mismo sitio: un track
/// suelto es un álbum del que solo se baja una pista.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Album { storefront: String, id: String, only_song: Option<String> },
    Song { storefront: String, id: String },
    Playlist { storefront: String, id: String },
    Artist { storefront: String, id: String },
    Room { storefront: String, id: String },
    MusicVideo { storefront: String, id: String },
}

pub fn parse_url(url: &str) -> Option<Target> {
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"music\.apple\.com/([a-zA-Z]{2})/(album|song|playlist|artist|room|music-video)/[^/]*/?([a-zA-Z0-9._-]+)")
            .unwrap()
    });
    static SONG_PARAM: Lazy<Regex> = Lazy::new(|| Regex::new(r"[?&]i=(\d+)").unwrap());

    let c = RE.captures(url)?;
    let storefront = c[1].to_lowercase();
    let id = c[3].to_string();
    let only_song = SONG_PARAM.captures(url).map(|m| m[1].to_string());

    Some(match &c[2] {
        // `?i=` significa "de este álbum, solo esta canción".
        "album" => Target::Album { storefront, id, only_song },
        "song" => Target::Song { storefront, id },
        "playlist" => Target::Playlist { storefront, id },
        "artist" => Target::Artist { storefront, id },
        "room" => Target::Room { storefront, id },
        "music-video" => Target::MusicVideo { storefront, id },
        _ => return None,
    })
}

/// Se avisa track a track para que la UI no espere al final de un álbum.
pub type OnTrack = std::sync::Arc<dyn Fn(usize, usize, &std::result::Result<TrackOutcome, Error>) + Send + Sync>;

#[derive(Default)]
pub struct Report {
    pub done: Vec<TrackOutcome>,
    /// Motivo por track que no salió. Se guarda el nombre porque el usuario
    /// piensa en canciones, no en ids.
    pub failed: Vec<(String, Error)>,
}

impl Report {
    fn push(&mut self, label: String, r: std::result::Result<TrackOutcome, Error>) {
        match r {
            Ok(o) => self.done.push(o),
            Err(e) => self.failed.push((label, e)),
        }
    }
}

/// Punto de entrada: cualquier URL de Apple Music.
pub async fn download_url(
    cfg: &Config,
    amp: &Amp,
    url: &str,
    quality: Quality,
    progress: Option<Progress>,
    on_track: Option<OnTrack>,
) -> Result<Report> {
    let target = parse_url(url).ok_or_else(|| Error::Other(format!("URL no reconocida: {url}")))?;
    match target {
        Target::Album { storefront, id, only_song } => {
            download_album(cfg, amp, &storefront, &id, only_song.as_deref(), quality, &cfg.output_dir, progress, on_track).await
        }
        Target::Song { storefront, id } => {
            // Una canción suelta necesita su álbum para las etiquetas (número de
            // pista, sello, copyright): se resuelve y se baja solo esa.
            let song = amp.song(&id).await?;
            let album_id = song["data"][0]["relationships"]["albums"]["data"][0]["id"]
                .as_str()
                .ok_or_else(|| Error::Other("no se encontró el álbum de esa canción".into()))?
                .to_string();
            download_album(cfg, amp, &storefront, &album_id, Some(&id), quality, &cfg.output_dir, progress, on_track).await
        }
        Target::Playlist { id, .. } => download_playlist(cfg, amp, &id, quality, progress, on_track).await,
        Target::Artist { id, .. } => download_artist(cfg, amp, &id, quality, progress, on_track).await,
        Target::Room { id, .. } => download_room(cfg, amp, &id, quality, progress, on_track).await,
        Target::MusicVideo { id, .. } => {
            let path = crate::mv::download_music_video(cfg, amp, &id, &cfg.output_dir).await?;
            let mut r = Report::default();
            r.done.push(TrackOutcome {
                path,
                name: id,
                artist: String::new(),
                album: String::new(),
                quality_label: "Music Video".into(),
                skipped: false,
            });
            Ok(r)
        }
    }
}

/// Mete el `enhancedHls` de la tienda con lossless en la metadata de otra tienda.
///
/// La metadata se pide en la tienda de la URL para que los títulos salgan en su
/// idioma original, pero **fuera de la tienda de la cuenta no vienen las URLs
/// lossless**. Sin este injerto, un álbum abierto desde la tienda japonesa se
/// bajaría en AAC sin que nadie entienda por qué.
fn inject_enhanced_hls(meta: &mut Value, source: &Value) {
    let by_id: std::collections::HashMap<String, String> = source["data"][0]["relationships"]["tracks"]["data"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let id = t["id"].as_str()?.to_string();
                    let hls = t["attributes"]["extendedAssetUrls"]["enhancedHls"].as_str()?.to_string();
                    Some((id, hls))
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(tracks) = meta["data"][0]["relationships"]["tracks"]["data"].as_array_mut() {
        for t in tracks {
            let has = t["attributes"]["extendedAssetUrls"]["enhancedHls"].is_string();
            if has {
                continue;
            }
            if let Some(hls) = t["id"].as_str().and_then(|id| by_id.get(id)) {
                t["attributes"]["extendedAssetUrls"]["enhancedHls"] = Value::String(hls.clone());
            }
        }
    }
}

/// El artista de la carpeta. Los álbumes editoriales de Apple vienen con
/// `artistName = "Apple Music"`, que como nombre de carpeta no dice nada: se cae
/// al primer artista real de la lista.
fn folder_artist(album_attrs: &Value, tracks: &[Value]) -> String {
    let name = album_attrs["artistName"].as_str().unwrap_or("").to_string();
    let generic = |s: &str| matches!(s.trim().to_lowercase().as_str(), "apple music" | "apple");
    if !generic(&name) {
        return name;
    }
    tracks
        .iter()
        .filter_map(|t| t["attributes"]["artistName"].as_str())
        .find(|a| !generic(a))
        .unwrap_or(&name)
        .to_string()
}

#[allow(clippy::too_many_arguments)]
pub async fn download_album(
    cfg: &Config,
    amp: &Amp,
    url_storefront: &str,
    album_id: &str,
    only_song: Option<&str>,
    quality: Quality,
    base_dir: &Path,
    progress: Option<Progress>,
    on_track: Option<OnTrack>,
) -> Result<Report> {
    // Metadata en la tienda de la URL; si esa tienda no lo tiene, la de la cuenta.
    let (mut meta, used_storefront) = {
        let by_url = Amp { storefront: url_storefront.to_string(), ..amp.clone() };
        match by_url.album(album_id).await {
            Ok(m) => (m, url_storefront.to_string()),
            Err(_) => (amp.album(album_id).await?, amp.storefront.clone()),
        }
    };

    if used_storefront != amp.storefront {
        match amp.album(album_id).await {
            Ok(home) => inject_enhanced_hls(&mut meta, &home),
            Err(e) => tracing::warn!("no se pudo traer la metadata de {}: {e}", amp.storefront),
        }
    }

    let album = meta["data"][0].clone();
    let album_attrs = album["attributes"].clone();
    let tracks: Vec<Value> = album["relationships"]["tracks"]["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if tracks.is_empty() {
        return Err(Error::NotFound);
    }

    let artist = folder_artist(&album_attrs, &tracks);
    let mut dir = base_dir.to_path_buf();
    if let Some(af) = crate::naming::artist_folder(cfg, "", &artist) {
        dir = dir.join(af);
    }
    let mut album_for_naming = album.clone();
    album_for_naming["attributes"]["artistName"] = Value::String(artist.clone());
    dir = dir.join(crate::naming::album_folder(cfg, &album_for_naming, quality));
    tokio::fs::create_dir_all(&dir).await?;

    // La carátula se baja UNA vez para todo el álbum, no una por track.
    let cover = crate::artwork::fetch_cover(&album_attrs["artwork"], &cfg.cover_size).await;
    if cfg.save_cover {
        crate::artwork::save_cover(&album_attrs["artwork"], &dir).await.ok();
    }

    let selected: Vec<(usize, &Value)> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| only_song.is_none_or(|id| t["id"].as_str() == Some(id)))
        .collect();

    let total = selected.len();
    let mut report = Report::default();

    for (i, (idx, t)) in selected.into_iter().enumerate() {
        let attrs = t["attributes"].clone();
        let label = attrs["name"].as_str().unwrap_or("?").to_string();
        let job = TrackJob {
            track: attrs.clone(),
            album: album_attrs.clone(),
            adam_id: t["id"].as_str().unwrap_or_default().to_string(),
            // El número de pista real manda; el índice es solo el respaldo.
            track_num: attrs["trackNumber"].as_u64().unwrap_or(idx as u64 + 1) as u32,
            disc_override: None,
            output_dir: dir.clone(),
            quality,
            cover: cover.clone(),
        };
        let res = download_track(cfg, amp, job, progress.clone()).await;
        if let Some(cb) = &on_track {
            cb(i + 1, total, &res);
        }
        report.push(label, res);
    }
    Ok(report)
}

pub async fn download_playlist(
    cfg: &Config,
    amp: &Amp,
    playlist_id: &str,
    quality: Quality,
    progress: Option<Progress>,
    on_track: Option<OnTrack>,
) -> Result<Report> {
    let (name, tracks) = amp.playlist(playlist_id).await?;
    let dir = cfg
        .output_dir
        .join(crate::naming::playlist_folder(cfg, playlist_id, &name, quality));
    tokio::fs::create_dir_all(&dir).await?;

    let total = tracks.len();
    let mut report = Report::default();

    for (i, t) in tracks.iter().enumerate() {
        let attrs = t["attributes"].clone();
        let label = attrs["name"].as_str().unwrap_or("?").to_string();
        // En una playlist cada track viene de un álbum distinto: se usan sus
        // propios datos como "álbum" para que las etiquetas sigan siendo suyas.
        let album_attrs = serde_json::json!({
            "name": attrs["albumName"].clone(),
            "artistName": attrs["artistName"].clone(),
            "releaseDate": attrs["releaseDate"].clone(),
            "artwork": attrs["artwork"].clone(),
            "trackCount": Value::from(total as u64),
            "genreNames": attrs["genreNames"].clone(),
        });
        let job = TrackJob {
            track: attrs,
            album: album_attrs,
            adam_id: t["id"].as_str().unwrap_or_default().to_string(),
            // La posición en la playlist, no la del álbum de origen.
            track_num: i as u32 + 1,
            disc_override: Some(1),
            output_dir: dir.clone(),
            quality,
            cover: None,
        };
        let res = download_track(cfg, amp, job, progress.clone()).await;
        if let Some(cb) = &on_track {
            cb(i + 1, total, &res);
        }
        report.push(label, res);
    }
    Ok(report)
}

pub async fn download_artist(
    cfg: &Config,
    amp: &Amp,
    artist_id: &str,
    quality: Quality,
    progress: Option<Progress>,
    on_track: Option<OnTrack>,
) -> Result<Report> {
    let (name, album_ids) = amp.artist_albums(artist_id).await?;
    let base: PathBuf = match crate::naming::artist_folder(cfg, artist_id, &name) {
        Some(f) => cfg.output_dir.join(f),
        None => cfg.output_dir.clone(),
    };

    let mut report = Report::default();
    for id in album_ids {
        // Un álbum que falle no puede tumbar la discografía entera.
        match download_album(cfg, amp, &amp.storefront, &id, None, quality, &base, progress.clone(), on_track.clone()).await {
            Ok(r) => {
                report.done.extend(r.done);
                report.failed.extend(r.failed);
            }
            Err(e) => report.failed.push((format!("álbum {id}"), e)),
        }
    }
    Ok(report)
}

/// Una "room" editorial: páginas curadas que mezclan álbumes y playlists.
pub async fn download_room(
    cfg: &Config,
    amp: &Amp,
    room_id: &str,
    quality: Quality,
    progress: Option<Progress>,
    on_track: Option<OnTrack>,
) -> Result<Report> {
    let (title, items) = amp.room(room_id).await?;
    let base = cfg.output_dir.join(crate::naming::sanitize(&title, 200));
    tokio::fs::create_dir_all(&base).await?;

    let mut report = Report::default();
    for (kind, id) in items {
        let r = match kind.as_str() {
            "albums" => download_album(cfg, amp, &amp.storefront, &id, None, quality, &base, progress.clone(), on_track.clone()).await,
            "playlists" => download_playlist(cfg, amp, &id, quality, progress.clone(), on_track.clone()).await,
            other => {
                tracing::debug!("en la room hay un {other} que no se baja");
                continue;
            }
        };
        match r {
            Ok(rep) => {
                report.done.extend(rep.done);
                report.failed.extend(rep.failed);
            }
            Err(e) => report.failed.push((format!("{kind} {id}"), e)),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_las_urls_de_apple_music() {
        assert_eq!(
            parse_url("https://music.apple.com/nz/album/hyperspace/1234567890"),
            Some(Target::Album { storefront: "nz".into(), id: "1234567890".into(), only_song: None })
        );
        assert_eq!(
            parse_url("https://music.apple.com/jp/album/x/111?i=222"),
            Some(Target::Album { storefront: "jp".into(), id: "111".into(), only_song: Some("222".into()) })
        );
        assert!(matches!(
            parse_url("https://music.apple.com/us/playlist/todo/pl.u-abc123"),
            Some(Target::Playlist { .. })
        ));
        assert!(matches!(
            parse_url("https://music.apple.com/nz/music-video/x/999"),
            Some(Target::MusicVideo { .. })
        ));
        assert!(parse_url("https://open.spotify.com/album/x").is_none());
    }

    #[test]
    fn el_artista_generico_de_apple_cae_al_primero_real() {
        let attrs = serde_json::json!({ "artistName": "Apple Music" });
        let tracks = vec![
            serde_json::json!({"attributes": {"artistName": "Apple Music"}}),
            serde_json::json!({"attributes": {"artistName": "Martin Garrix"}}),
        ];
        assert_eq!(folder_artist(&attrs, &tracks), "Martin Garrix");
    }

    #[test]
    fn el_enhanced_hls_se_injerta_solo_donde_falta() {
        let mut meta = serde_json::json!({"data": [{"relationships": {"tracks": {"data": [
            {"id": "1", "attributes": {}},
            {"id": "2", "attributes": {"extendedAssetUrls": {"enhancedHls": "ya-tenia"}}}
        ]}}}]});
        let source = serde_json::json!({"data": [{"relationships": {"tracks": {"data": [
            {"id": "1", "attributes": {"extendedAssetUrls": {"enhancedHls": "nuevo"}}},
            {"id": "2", "attributes": {"extendedAssetUrls": {"enhancedHls": "otro"}}}
        ]}}}]});

        inject_enhanced_hls(&mut meta, &source);
        let tracks = &meta["data"][0]["relationships"]["tracks"]["data"];
        assert_eq!(tracks[0]["attributes"]["extendedAssetUrls"]["enhancedHls"], "nuevo");
        assert_eq!(tracks[1]["attributes"]["extendedAssetUrls"]["enhancedHls"], "ya-tenia");
    }
}
