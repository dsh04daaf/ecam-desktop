//! Music videos, de punta a punta y sin binarios externos.
//!
//! No pasan por el wrapper: van cifrados con **Widevine**, no con FairPlay. Todo
//! el camino (licencia, descifrado cbcs, mux y etiquetas) es nuestro; en el
//! original esto necesitaba `mp4decrypt` y `MP4Box`.

pub mod cbcs;
pub mod mux;
pub mod widevine;

use crate::amp::{http, Amp};
use crate::config::Config;
use crate::error::{Error, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const WEBPLAYBACK: &str = "https://play.music.apple.com/WebObjects/MZPlay.woa/wa/webPlayback";
const LICENSE: &str = "https://play.itunes.apple.com/WebObjects/MZPlay.woa/wa/acquireWebPlaybackLicense";
const WIDEVINE_KEYFORMAT: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
/// User-Agent de Chrome **obligatorio**: con otro, Apple entrega resoluciones
/// más bajas y nunca dice por qué.
const MV_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Master playlist del vídeo.
async fn webplayback_master(adam_id: &str, bearer: &str, mut_token: &str) -> Result<String> {
    let v: Value = http()
        .post(WEBPLAYBACK)
        .header("Content-Type", "application/json")
        .header("Origin", "https://music.apple.com")
        .header("Referer", "https://music.apple.com/")
        .header("User-Agent", MV_UA)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("x-apple-music-user-token", mut_token)
        .json(&serde_json::json!({ "salableAdamId": adam_id }))
        .send()
        .await?
        .json()
        .await?;

    v["songList"][0]["hls-playlist-url"]
        .as_str()
        .map(String::from)
        // Este error casi siempre es el token, no la red: decirlo ahorra media
        // hora de mirar el sitio equivocado.
        .ok_or_else(|| Error::Other("el media-user-token parece caducado o incorrecto".into()))
}

fn res_label(w: u32, h: u32) -> String {
    match h {
        2160 => "4K".into(),
        1080 => "1080p".into(),
        720 => "720p".into(),
        _ => format!("{w}x{h}"),
    }
}

/// Elige el vídeo: por ancho de banda descendente, el primero cuya **altura**
/// quepa en `mv-max`. El tamaño sale del propio nombre del URI (`_1920x1080`).
fn select_video(master: &str, base_url: &str, mv_max: u32) -> Result<(String, u32, u32)> {
    static SIZE: Lazy<Regex> = Lazy::new(|| Regex::new(r"_(\d+)x(\d+)").unwrap());
    let lines: Vec<&str> = master.lines().collect();
    let mut variants: Vec<(u64, String)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("#EXT-X-STREAM-INF:") {
            let a = crate::hls::attrs(&l[18..]);
            let bw = crate::hls::attr(&a, "AVERAGE-BANDWIDTH")
                .or_else(|| crate::hls::attr(&a, "BANDWIDTH"))
                .and_then(|b| b.parse().ok())
                .unwrap_or(0);
            if let Some(uri) = lines.get(i + 1).map(|s| s.trim()) {
                if !uri.starts_with('#') && !uri.is_empty() {
                    variants.push((bw, uri.to_string()));
                }
            }
        }
    }
    variants.sort_by(|a, b| b.0.cmp(&a.0));

    for (_, uri) in variants {
        let Some(c) = SIZE.captures(&uri) else { continue };
        let (w, h) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
        if h <= mv_max {
            return Ok((join(base_url, &uri), w, h));
        }
    }
    Err(Error::Other("ninguna variante de vídeo cabe en el máximo configurado".into()))
}

/// Puntuación de un GROUP-ID de audio cuando no está en la lista de prioridad.
fn group_score(group_id: &str) -> i64 {
    if group_id.contains("atmos") {
        return 10_000;
    }
    if group_id.contains("ac3") {
        return 9_000;
    }
    let kbps: i64 = group_id
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // El HE-AAC anuncia un bitrate bajo pero es paramétrico: se ordena por
    // debajo de un estéreo normal del mismo bitrate nominal.
    if group_id.contains("HE") {
        kbps - 1
    } else {
        kbps
    }
}

/// Elige el audio por prioridad de GROUP-ID, desempatando por el `_grN_` mayor.
///
/// Los vídeos viejos solo publican `audio-stereo-128` o `audio-HE-stereo-64`,
/// que no están en ninguna lista de prioridad. El original en Go se tragaba el
/// error, bajaba el vídeo entero y moría en el mux con un mensaje que no decía
/// nada. Aquí se cae al mejor audio que el vídeo ofrezca.
fn select_audio(master: &str, base_url: &str, audio_type: &str) -> Result<String> {
    static GR: Lazy<Regex> = Lazy::new(|| Regex::new(r"_gr(\d+)_").unwrap());
    let priority: &[&str] = match audio_type {
        "ac3" => &["audio-ac3", "audio-stereo-256"],
        "aac" => &["audio-stereo-256"],
        _ => &["audio-atmos", "audio-ac3", "audio-stereo-256"],
    };

    let mut found: Vec<(usize, i64, String, String)> = Vec::new();
    let mut fallback: Vec<(i64, i64, String, String)> = Vec::new();

    for l in master.lines() {
        let Some(rest) = l.strip_prefix("#EXT-X-MEDIA:") else { continue };
        let a = crate::hls::attrs(rest);
        if crate::hls::attr(&a, "TYPE") != Some("AUDIO") {
            continue;
        }
        let (Some(gid), Some(uri)) = (crate::hls::attr(&a, "GROUP-ID"), crate::hls::attr(&a, "URI")) else {
            continue;
        };
        let rank = GR
            .captures(uri)
            .and_then(|c| c[1].parse::<i64>().ok())
            .map(|n| -n)
            .unwrap_or(0);
        let full = join(base_url, uri);
        match priority.iter().position(|p| *p == gid) {
            Some(idx) => found.push((idx, rank, gid.to_string(), full)),
            None => fallback.push((-group_score(gid), rank, gid.to_string(), full)),
        }
    }

    found.sort();
    if let Some((_, _, gid, url)) = found.first() {
        tracing::info!("[MV] audio: {gid}");
        return Ok(url.clone());
    }
    fallback.sort();
    if let Some((_, _, gid, url)) = fallback.first() {
        tracing::info!("[MV] audio: {gid} (respaldo — sin atmos/ac3/stereo-256)");
        return Ok(url.clone());
    }
    Err(Error::Other("el vídeo no publica ninguna pista de audio".into()))
}

fn join(base: &str, rel: &str) -> String {
    match reqwest::Url::parse(base).and_then(|b| b.join(rel)) {
        Ok(u) => u.to_string(),
        Err(_) => rel.to_string(),
    }
}

/// Saca de la media playlist el KID de Widevine y la lista de segmentos.
///
/// Vienen **tres** `#EXT-X-KEY` (FairPlay `skd://`, PlayReady en UTF-16 y
/// Widevine). Se elige la de Widevine por `KEYFORMAT`, no por orden de aparición.
fn extract_key_and_urls(playlist: &str, media_url: &str) -> Result<(String, Vec<String>, String)> {
    let base = media_url.rsplit_once('/').map(|(b, _)| b).unwrap_or(media_url);
    let mut kid = None;
    let mut uri_prefix = String::new();
    let mut init_uri = None;
    let mut segments = Vec::new();

    for line in playlist.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let a = crate::hls::attrs(rest);
            let format = crate::hls::attr(&a, "KEYFORMAT").unwrap_or("").to_lowercase();
            let uri = crate::hls::attr(&a, "URI").unwrap_or("");
            if format == WIDEVINE_KEYFORMAT {
                if let Some((prefix, k)) = uri.split_once(',') {
                    uri_prefix = prefix.to_string();
                    kid = Some(k.to_string());
                }
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            init_uri = crate::hls::attr(&crate::hls::attrs(rest), "URI").map(String::from);
        } else if !line.is_empty() && !line.starts_with('#') {
            segments.push(format!("{base}/{line}"));
        }
    }

    let kid = kid.ok_or_else(|| Error::Other("la playlist no trae llave de Widevine".into()))?;
    let init = init_uri.ok_or_else(|| Error::Other("la playlist no trae segmento de init".into()))?;
    let mut urls = vec![format!("{base}/{init}")];
    urls.extend(segments);
    Ok((kid, urls, uri_prefix))
}

async fn content_key(
    cfg: &Config,
    adam_id: &str,
    kid: &str,
    uri_prefix: &str,
    bearer: &str,
    mut_token: &str,
) -> Result<String> {
    let pssh = widevine::build_pssh(kid)?;
    let mut cdm = widevine::Cdm::new(cfg, &pssh)?;
    let challenge = cdm.license_request()?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let v: Value = http()
        .post(LICENSE)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("x-apple-music-user-token", mut_token)
        .json(&serde_json::json!({
            "challenge": b64.encode(&challenge),
            "key-system": "com.widevine.alpha",
            "uri": format!("{uri_prefix},{kid}"),
            "adamId": adam_id,
            "isLibrary": false,
            "user-initiated": true,
        }))
        .send()
        .await?
        .json()
        .await?;

    if v["errorCode"].as_i64().unwrap_or(0) != 0 || v["status"].as_i64().unwrap_or(0) != 0 {
        return Err(Error::Other(format!(
            "Apple rechazó la licencia de Widevine (errorCode={}, status={})",
            v["errorCode"], v["status"]
        )));
    }
    let license = v["license"]
        .as_str()
        .and_then(|s| b64.decode(s).ok())
        .ok_or_else(|| Error::Other("la respuesta de licencia no traía licencia".into()))?;
    cdm.content_key(&license)
}

/// Baja todos los segmentos de un stream, los concatena y los descifra.
async fn fetch_and_decrypt(
    cfg: &Config,
    kind: &str,
    adam_id: &str,
    media_url: &str,
    dest: &Path,
    bearer: &str,
    mut_token: &str,
) -> Result<()> {
    let playlist = http().get(media_url).header("User-Agent", MV_UA).send().await?.text().await?;
    let (kid, urls, uri_prefix) = extract_key_and_urls(&playlist, media_url)?;
    let key = content_key(cfg, adam_id, &kid, &uri_prefix, bearer, mut_token).await?;

    let dir = dest.parent().unwrap_or(Path::new("."));
    let enc = tempfile::NamedTempFile::new_in(dir)?;
    {
        let mut w = BufWriter::new(enc.as_file());
        for (i, u) in urls.iter().enumerate() {
            let mut resp = http().get(u).header("User-Agent", MV_UA).send().await?;
            if !resp.status().is_success() {
                return Err(Error::Other(format!(
                    "el segmento {i} de {kind} respondió {}",
                    resp.status().as_u16()
                )));
            }
            while let Some(chunk) = resp.chunk().await? {
                w.write_all(&chunk)?;
            }
        }
        w.flush()?;
    }

    let dest = dest.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut src = std::fs::File::open(enc.path())?;
        let out = std::fs::File::create(&dest)?;
        let mut w = BufWriter::new(out);
        cbcs::decrypt_file(&mut src, &mut w, &key)?;
        w.flush()?;
        Ok(())
    })
    .await
    .map_err(|e| Error::Other(format!("el descifrado de {kind} se cayó: {e}")))??;
    Ok(())
}

/// Nombre de archivo seguro para el vídeo (Windows incluido).
fn safe_name(base: &str, fallback: &str) -> String {
    static CONTROL: Lazy<Regex> = Lazy::new(|| Regex::new(r"[\x00-\x1f\x7f-\x9f]").unwrap());
    static INVISIBLE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[\u{200b}-\u{200f}\u{2066}-\u{2069}\u{feff}]").unwrap());
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let s = INVISIBLE.replace_all(base, "");
    let s = CONTROL.replace_all(&s, " ");
    let s = crate::naming::sanitize(&s, 120);
    // Windows tampoco tolera un punto o un espacio al final.
    let s = s.trim_end_matches([' ', '.']).to_string();
    if s.is_empty() || RESERVED.contains(&s.to_uppercase().as_str()) {
        return fallback.to_string();
    }
    s
}

/// Descarga un music video completo. Devuelve la ruta del .mp4 final.
pub async fn download_music_video(cfg: &Config, amp: &Amp, mv_id: &str, base_dir: &Path) -> Result<PathBuf> {
    if amp.media_user_token.trim().len() < 20 {
        return Err(Error::NeedsUserToken);
    }
    let meta = amp.music_video(mv_id).await?;
    let attrs = meta["data"][0]["attributes"].clone();
    let name = attrs["name"].as_str().unwrap_or("").to_string();
    let artist = attrs["artistName"].as_str().unwrap_or("").to_string();
    let album = attrs["albumName"].as_str().unwrap_or("").to_string();

    let master_url = webplayback_master(mv_id, &amp.bearer, &amp.media_user_token).await?;
    let master = http().get(&master_url).header("User-Agent", MV_UA).send().await?.text().await?;
    let (video_url, w, h) = select_video(&master, &master_url, cfg.mv_max)?;
    let audio_url = select_audio(&master, &master_url, &cfg.mv_audio_type)?;

    let title = if artist.is_empty() { name.clone() } else { format!("{artist} - {name}") };
    let base = safe_name(&format!("{title} ({})", res_label(w, h)), mv_id);

    // Carpeta propia por vídeo: además del .mp4 pueden caer carátula y extras.
    let dir = base_dir.join(&base);
    tokio::fs::create_dir_all(&dir).await?;
    let out_path = dir.join(format!("{base}.mp4"));
    if out_path.exists() {
        return Ok(out_path);
    }

    tracing::info!("[MV] bajando {title} ({})", res_label(w, h));
    let video_path = dir.join(".video.tmp.mp4");
    let audio_path = dir.join(".audio.tmp.mp4");

    let result = async {
        fetch_and_decrypt(cfg, "vídeo", mv_id, &video_url, &video_path, &amp.bearer, &amp.media_user_token).await?;
        fetch_and_decrypt(cfg, "audio", mv_id, &audio_url, &audio_path, &amp.bearer, &amp.media_user_token).await?;

        let (v, a, o) = (video_path.clone(), audio_path.clone(), out_path.clone());
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut vf = std::fs::File::open(&v)?;
            let mut af = std::fs::File::open(&a)?;
            let mut sources: Vec<(mux::MvTrack, std::fs::File)> = Vec::new();
            for t in mux::read_tracks(&mut vf)? {
                sources.push((t, std::fs::File::open(&v)?));
            }
            for t in mux::read_tracks(&mut af)? {
                sources.push((t, std::fs::File::open(&a)?));
            }
            let out = std::fs::File::create(&o)?;
            let mut w = BufWriter::new(out);
            mux::mux(&mut sources, &mut w)?;
            w.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Other(format!("el mux se cayó: {e}")))??;
        Ok::<(), Error>(())
    }
    .await;

    // Los temporales se van pase lo que pase: son cientos de MB.
    tokio::fs::remove_file(&video_path).await.ok();
    tokio::fs::remove_file(&audio_path).await.ok();
    result?;

    let cover = crate::artwork::fetch_cover(&attrs["artwork"], &cfg.cover_size).await;
    let album_meta = serde_json::json!({
        "name": album,
        "artistName": artist,
        "releaseDate": attrs["releaseDate"].clone(),
        "genreNames": attrs["genreNames"].clone(),
        "trackCount": 0,
    });
    if let Err(e) = crate::tags::write(&out_path, &attrs, &album_meta, cover.as_deref(), None) {
        // El vídeo ya está bien: no se tira por las etiquetas.
        tracing::warn!("no se pudieron escribir las etiquetas del vídeo: {e}");
    }

    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = r#"#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio-stereo-128",URI="a_gr2_128.m3u8"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio-HE-stereo-128",URI="a_gr1_he.m3u8"
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=20000000,CODECS="hvc1"
v_3840x2160.m3u8
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=8000000,CODECS="avc1"
v_1920x1080.m3u8
"#;

    #[test]
    fn el_video_respeta_el_maximo_de_altura() {
        let (url, w, h) = select_video(MASTER, "https://x/y/m.m3u8", 1080).unwrap();
        assert!(url.ends_with("v_1920x1080.m3u8"));
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn sin_atmos_ni_ac3_cae_al_mejor_audio_que_haya() {
        let url = select_audio(MASTER, "https://x/y/m.m3u8", "atmos").unwrap();
        assert!(url.ends_with("a_gr2_128.m3u8"), "estéreo normal antes que HE-AAC");
    }

    #[test]
    fn el_he_aac_va_por_debajo_del_estereo_del_mismo_bitrate() {
        assert!(group_score("audio-stereo-128") > group_score("audio-HE-stereo-128"));
        assert!(group_score("audio-atmos") > group_score("audio-ac3"));
    }

    #[test]
    fn se_elige_la_llave_de_widevine_por_keyformat_no_por_orden() {
        let pl = r#"#EXTM3U
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="skd://apple/x",KEYFORMAT="com.apple.streamingkeydelivery"
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="data:text/plain;base64,UExBWQ==",KEYFORMAT="com.microsoft.playready"
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="data:text/plain;base64,QUJD",KEYFORMAT="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"
#EXT-X-MAP:URI="init.mp4"
seg1.mp4
"#;
        let (kid, urls, prefix) = extract_key_and_urls(pl, "https://x/y/media.m3u8").unwrap();
        assert_eq!(kid, "QUJD");
        assert_eq!(prefix, "data:text/plain;base64");
        assert_eq!(urls[0], "https://x/y/init.mp4");
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn los_nombres_reservados_de_windows_no_pasan() {
        assert_eq!(safe_name("CON", "fallback"), "fallback");
        assert_eq!(safe_name("Tema/raro ", "fb"), "Tema_raro");
    }
}
