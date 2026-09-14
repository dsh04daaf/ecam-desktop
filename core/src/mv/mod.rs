//! Music videos, de punta a punta y sin binarios externos.
//!
//! No pasan por el wrapper: van cifrados con **Widevine**, no con FairPlay. Todo
//! el camino (licencia, descifrado cbcs, mux y etiquetas) es nuestro; en el
//! original esto necesitaba `mp4decrypt` y `MP4Box`.

pub mod cbcs;
pub mod mux;
pub mod playready;
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
const PLAYREADY_KEYFORMAT: &str = "com.microsoft.playready";
/// User-Agent de Chrome, **sólo para el POST a webPlayback**.
///
/// ⚠️ Y **sólo ahí**. Mandarlo también al pedir el master playlist es lo que
/// tenía a esta app capada a 1080p sin que nadie lo notara: con UA de navegador
/// Apple sirve un manifiesto recortado (el reproductor web no hace 4K), y sin él
/// llegan las variantes de 1440p y 4K. Medido el 2026-09-14 sobre la misma URL:
/// sin UA → 11 variantes, máx 3840x2016; con Chrome → 10 variantes, máx 1920x1008.
/// El comentario anterior decía justo lo contrario y estaba equivocado.
const MV_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";

/// User-Agent para los GET de manifiestos y segmentos de music video.
///
/// Tiene que ser uno que **no** parezca un navegador. El cliente compartido
/// (`amp::http()`) manda por defecto uno con `AppleWebKit`, y basta eso para que
/// Apple recorte el manifiesto a 1080p, así que aquí se pisa en cada petición.
/// Medido: `(ninguno)`, `python-requests/2.31` y `ECAM/1.0` dan 4K; cualquiera
/// con `AppleWebKit` — el corto del cliente incluido — se queda en 1920x1008.
const MV_PLAYLIST_UA: &str = "ECAM/1.0";

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

/// Etiqueta para el nombre del archivo.
///
/// Apple entrega los music videos con barras (3840x2016, 3840x1636…), así que la
/// señal fiable es el **ancho**, no el alto: comparando sólo `h == 2160` un
/// máster 4K en scope salía nombrado «3840x2016». Mismos cortes que el bot.
fn res_label(w: u32, h: u32) -> String {
    match (w, h) {
        (w, h) if w >= 3840 || h >= 2160 => "4K".into(),
        (w, h) if w >= 2560 || h >= 1440 => "1440p".into(),
        (w, h) if w >= 1920 || h >= 1080 => "1080p".into(),
        (w, h) if w >= 1280 || h >= 720 => "720p".into(),
        (w, h) if w >= 852 || h >= 480 => "480p".into(),
        (w, h) if w >= 620 || h >= 340 => "360p".into(),
        (_, 0) => "SD".into(),
        (_, h) => format!("{h}p"),
    }
}

/// ¿Esta variante exige PlayReady SL3000?
///
/// Apple pone un `ALLOWED-CPC` por variante. Hasta 1080p dice
/// `com.microsoft.playready:SL2000` + `com.apple.streamingkeydelivery:Baseline`,
/// y el servidor de licencias de Widevine todavía la sirve. De 1440p para arriba
/// pasa a `SL3000` / `Main/AppleMain` y ahí Widevine se rechaza en seco
/// (`errorCode -42585`), igual que un cliente FairPlay Baseline: sólo se sirve
/// por PlayReady SL3000.
///
/// Apple hizo el cambio el **2026-09-10**; antes el camino de Widevine cubría
/// todas las resoluciones. Lo resuelve [`playready`], con el `.prd` que diga
/// `mv-playready-device` en el config.
pub(crate) fn needs_playready(allowed_cpc: &str) -> bool {
    allowed_cpc.to_ascii_uppercase().contains("SL3000")
}

/// Elige el vídeo: por ancho de banda descendente, el primero cuya **altura**
/// quepa en `mv-max`. Devuelve además si esa variante hay que licenciarla por
/// PlayReady. El tamaño sale del propio nombre del URI (`_1920x1080`).
fn select_video(master: &str, base_url: &str, mv_max: u32) -> Result<(String, u32, u32, bool)> {
    static SIZE: Lazy<Regex> = Lazy::new(|| Regex::new(r"_(\d+)x(\d+)").unwrap());
    let lines: Vec<&str> = master.lines().collect();
    let mut variants: Vec<(u64, String, bool)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("#EXT-X-STREAM-INF:") {
            let a = crate::hls::attrs(&l[18..]);
            let bw = crate::hls::attr(&a, "AVERAGE-BANDWIDTH")
                .or_else(|| crate::hls::attr(&a, "BANDWIDTH"))
                .and_then(|b| b.parse().ok())
                .unwrap_or(0);
            let cpc = crate::hls::attr(&a, "ALLOWED-CPC").unwrap_or_default();
            if let Some(uri) = lines.get(i + 1).map(|s| s.trim()) {
                if !uri.starts_with('#') && !uri.is_empty() {
                    variants.push((bw, uri.to_string(), needs_playready(&cpc)));
                }
            }
        }
    }
    variants.sort_by(|a, b| b.0.cmp(&a.0));

    for (_, uri, pr) in variants {
        let Some(c) = SIZE.captures(&uri) else { continue };
        let (w, h) = (c[1].parse().unwrap_or(0), c[2].parse().unwrap_or(0));
        if h <= mv_max {
            if pr {
                tracing::info!("MV: {w}x{h} va por PlayReady SL3000");
            }
            return Ok((join(base_url, &uri), w, h, pr));
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
/// Widevine). Se elige una por `KEYFORMAT`, no por orden de aparición: Widevine
/// para las variantes Baseline/SL2000 y PlayReady para las Main/SL3000.
fn extract_key_and_urls(
    playlist: &str,
    media_url: &str,
    keyformat: &str,
) -> Result<(String, Vec<String>, String)> {
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
            if format == keyformat {
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

    let kid = kid
        .ok_or_else(|| Error::Other(format!("la playlist no trae llave de {keyformat}")))?;
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

/// Llave de contenido del tier SL3000, por PlayReady en vez de Widevine.
///
/// Mismo endpoint y mismo sobre que [`content_key`]: sólo cambian `key-system`
/// y el challenge. Comprobado contra el camino de Widevine — en una variante que
/// sirvan los dos (c1/1080p), las dos llaves salen idénticas.
async fn content_key_playready(
    cfg: &Config,
    adam_id: &str,
    key_payload: &str,
    uri_prefix: &str,
    bearer: &str,
    mut_token: &str,
) -> Result<String> {
    let device_path =
        widevine::find_credential_in("playready", cfg.mv_playready_device.as_ref(), "device_sl3000.prd")
            .ok_or_else(|| {
                Error::Config(format!(
                    "esta variante necesita PlayReady SL3000 y falta el dispositivo. \
                     Se buscó en: {}. Sin él sólo se puede bajar hasta 1080p.",
                    widevine::candidates_in("playready", "device_sl3000.prd")
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
    let device = playready::Device::parse(&std::fs::read(&device_path)?)?;
    if device.security_level < 3000 {
        return Err(Error::Other(format!(
            "el dispositivo PlayReady es SL{}, y este tier pide SL3000",
            device.security_level
        )));
    }

    let cdm = playready::Cdm::new(device)?;
    let session = cdm.open()?;
    let wrm = playready::wrm_header_from_pssh(key_payload)?;
    let challenge = cdm.license_challenge(&session, &wrm)?;

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let v: Value = http()
        .post(LICENSE)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("x-apple-music-user-token", mut_token)
        .json(&serde_json::json!({
            "challenge": b64.encode(challenge.as_bytes()),
            "key-system": PLAYREADY_KEYFORMAT,
            "uri": format!("{uri_prefix},{key_payload}"),
            "adamId": adam_id,
            "isLibrary": false,
            "user-initiated": true,
        }))
        .send()
        .await?
        .json()
        .await?;

    // Apple manda estos dos unas veces como número y otras como cadena.
    let as_code = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => "0".into(),
    };
    let (code, status) = (as_code(&v["errorCode"]), as_code(&v["status"]));
    if code != "0" || status != "0" {
        return Err(Error::Other(format!(
            "Apple rechazó la licencia de PlayReady (errorCode={code}, status={status})"
        )));
    }
    let soap = v["license"]
        .as_str()
        .and_then(|s| b64.decode(s).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| Error::Other("la respuesta de licencia no traía licencia".into()))?;
    Ok(hex::encode(cdm.parse_license(&session, &soap)?))
}

/// Master playlist crudo de un music video. Sólo para los ejemplos de depuración.
pub async fn debug_master(amp: &Amp, adam_id: &str) -> Result<String> {
    let url = debug_master_url(amp, adam_id).await?;
    Ok(http().get(&url).header("User-Agent", MV_PLAYLIST_UA).send().await?.text().await?)
}

/// URL del master playlist. Sólo para los ejemplos de depuración.
pub async fn debug_master_url(amp: &Amp, adam_id: &str) -> Result<String> {
    webplayback_master(adam_id, &amp.bearer, &amp.media_user_token).await
}

/// Lo que devuelve [`debug_key_paths`].
pub struct KeyPathsReport {
    pub widevine_low: String,
    pub playready_low: String,
    pub playready_high: Option<String>,
}

/// Pide la llave del tier bajo por los DOS caminos y la del tier SL3000 por
/// PlayReady. Existe para el ejemplo `prkey`, que es como se comprueba que
/// nuestro CDM de PlayReady da exactamente lo mismo que Widevine donde ambos
/// sirven. No lo usa la app.
pub async fn debug_key_paths(cfg: &Config, amp: &Amp, adam_id: &str) -> Result<KeyPathsReport> {
    let master_url = webplayback_master(adam_id, &amp.bearer, &amp.media_user_token).await?;
    let master = http().get(&master_url).header("User-Agent", MV_PLAYLIST_UA).send().await?.text().await?;

    let lines: Vec<&str> = master.lines().collect();
    let (mut low, mut high) = (None, None);
    for (i, l) in lines.iter().enumerate() {
        let Some(rest) = l.strip_prefix("#EXT-X-STREAM-INF:") else { continue };
        let attrs = crate::hls::attrs(rest);
        let cpc = crate::hls::attr(&attrs, "ALLOWED-CPC").unwrap_or_default();
        let Some(uri) = lines.get(i + 1).map(|s| s.trim()) else { continue };
        let full = join(&master_url, uri);
        if needs_playready(&cpc) {
            high.get_or_insert(full);
        } else {
            low.get_or_insert(full);
        }
    }
    let low = low.ok_or_else(|| Error::Other("el vídeo no tiene variantes de tier bajo".into()))?;

    let fetch = |url: String, keyformat: &'static str| {
        let (cfg, amp, adam_id) = (cfg.clone(), amp.clone(), adam_id.to_string());
        async move {
            let pl = http().get(&url).header("User-Agent", MV_PLAYLIST_UA).send().await?.text().await?;
            let (k, _urls, prefix) = extract_key_and_urls(&pl, &url, keyformat)?;
            if keyformat == PLAYREADY_KEYFORMAT {
                content_key_playready(&cfg, &adam_id, &k, &prefix, &amp.bearer, &amp.media_user_token).await
            } else {
                content_key(&cfg, &adam_id, &k, &prefix, &amp.bearer, &amp.media_user_token).await
            }
        }
    };

    let widevine_low = fetch(low.clone(), WIDEVINE_KEYFORMAT).await?;
    let playready_low = fetch(low, PLAYREADY_KEYFORMAT).await?;
    let playready_high = match high {
        Some(u) => Some(fetch(u, PLAYREADY_KEYFORMAT).await?),
        None => None,
    };
    Ok(KeyPathsReport { widevine_low, playready_low, playready_high })
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
    use_playready: bool,
) -> Result<()> {
    let playlist = http().get(media_url).header("User-Agent", MV_PLAYLIST_UA).send().await?.text().await?;
    let keyformat = if use_playready { PLAYREADY_KEYFORMAT } else { WIDEVINE_KEYFORMAT };
    let (kid, urls, uri_prefix) = extract_key_and_urls(&playlist, media_url, keyformat)?;
    let key = if use_playready {
        content_key_playready(cfg, adam_id, &kid, &uri_prefix, bearer, mut_token).await?
    } else {
        content_key(cfg, adam_id, &kid, &uri_prefix, bearer, mut_token).await?
    };

    let dir = dest.parent().unwrap_or(Path::new("."));
    let enc = tempfile::NamedTempFile::new_in(dir)?;
    {
        let mut w = BufWriter::new(enc.as_file());
        for (i, u) in urls.iter().enumerate() {
            let mut resp = http().get(u).header("User-Agent", MV_PLAYLIST_UA).send().await?;
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
    let master = http().get(&master_url).header("User-Agent", MV_PLAYLIST_UA).send().await?.text().await?;
    let (video_url, w, h, video_needs_pr) = select_video(&master, &master_url, cfg.mv_max)?;
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
        let (bearer, mu) = (&amp.bearer, &amp.media_user_token);
        fetch_and_decrypt(cfg, "vídeo", mv_id, &video_url, &video_path, bearer, mu, video_needs_pr).await?;
        // El audio no trae ALLOWED-CPC propio y sigue yendo por Widevine.
        fetch_and_decrypt(cfg, "audio", mv_id, &audio_url, &audio_path, bearer, mu, false).await?;

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
        let (url, w, h, pr) = select_video(MASTER, "https://x/y/m.m3u8", 1080).unwrap();
        assert!(url.ends_with("v_1920x1080.m3u8"));
        assert_eq!((w, h), (1920, 1080));
        assert!(!pr, "sin ALLOWED-CPC no hay motivo para pedir PlayReady");
    }

    /// ALLOWED-CPC tal cual lo sirve Apple desde el 2026-09-10: el 4K en SL3000
    /// y el 1080p todavía en SL2000. Copiado de un manifiesto real.
    const MASTER_SL3000: &str = r#"#EXTM3U
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=20000000,CODECS="hvc1",HDCP-LEVEL=TYPE-1,ALLOWED-CPC="com.apple.streamingkeydelivery:Main/AppleMain,com.microsoft.playready:SL3000,urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed:WIDEVINE_HARDWARE"
v_3840x2016.m3u8
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=8000000,CODECS="avc1",HDCP-LEVEL=TYPE-0,ALLOWED-CPC="com.apple.streamingkeydelivery:Baseline/AppleBaseline,com.microsoft.playready:SL2000,urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed:WIDEVINE_HARDWARE"
v_1920x1008.m3u8
"#;

    #[test]
    fn un_4k_con_barras_se_llama_4k_y_no_3840x2016() {
        // Apple entrega los MV con barras: mirar sólo el alto dejaba nombres
        // como «GROSU - РЕАНІМАЦІЯ (3840x2016)» en vez de «(4K)».
        assert_eq!(res_label(3840, 2016), "4K");
        assert_eq!(res_label(3840, 1636), "4K");
        assert_eq!(res_label(3840, 2160), "4K");
        assert_eq!(res_label(2560, 1440), "1440p");
        assert_eq!(res_label(1920, 1008), "1080p");
        assert_eq!(res_label(1280, 720), "720p");
        assert_eq!(res_label(496, 260), "260p");
    }

    #[test]
    fn el_tier_sl3000_se_detecta_por_allowed_cpc() {
        assert!(needs_playready(
            "com.apple.streamingkeydelivery:Main/AppleMain,com.microsoft.playready:SL3000"
        ));
        // WIDEVINE_HARDWARE también sale en el 1080p, que SÍ se licencia: no sirve de criterio.
        assert!(!needs_playready(
            "com.apple.streamingkeydelivery:Baseline/AppleBaseline,com.microsoft.playready:SL2000,\
             urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed:WIDEVINE_HARDWARE"
        ));
        assert!(!needs_playready(""));
    }

    #[test]
    fn el_4k_en_sl3000_se_elige_y_se_marca_para_playready() {
        // Si esto devolviera pr=false pediríamos la llave por Widevine, Apple
        // contestaría -42585 y la descarga moriría entera.
        let (url, w, h, pr) = select_video(MASTER_SL3000, "https://x/y/m.m3u8", 2160).unwrap();
        assert!(url.ends_with("v_3840x2016.m3u8"), "el 4K sí se puede bajar");
        assert_eq!((w, h), (3840, 2016));
        assert!(pr, "el 4K tiene que ir por PlayReady");
    }

    #[test]
    fn por_debajo_del_tier_alto_se_sigue_usando_widevine() {
        let (url, _w, _h, pr) = select_video(MASTER_SL3000, "https://x/y/m.m3u8", 1080).unwrap();
        assert!(url.ends_with("v_1920x1008.m3u8"));
        assert!(!pr, "el 1080p se licencia por Widevine, como siempre");
    }

    #[test]
    fn la_llave_se_elige_por_keyformat_no_por_orden() {
        // La playlist trae las tres y la de Widevine va la última: quien coja
        // "la última que aparezca" se lleva la equivocada.
        let pl = "#EXTM3U\n\
            #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://itunes.apple.com/p1/c2\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n\
            #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"data:text/plain;charset=UTF-16;base64,UFI=\",KEYFORMAT=\"com.microsoft.playready\"\n\
            #EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"data:text/plain;base64,V1Y=\",KEYFORMAT=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\"\n\
            #EXT-X-MAP:URI=\"init.mp4\"\nseg1.m4s\n";
        let (k, _urls, _p) = extract_key_and_urls(pl, "https://x/y/m.m3u8", PLAYREADY_KEYFORMAT).unwrap();
        assert_eq!(k, "UFI=", "debía coger la de PlayReady");
        let (k, _urls, _p) = extract_key_and_urls(pl, "https://x/y/m.m3u8", WIDEVINE_KEYFORMAT).unwrap();
        assert_eq!(k, "V1Y=", "debía coger la de Widevine");
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
        let (kid, urls, prefix) = extract_key_and_urls(pl, "https://x/y/media.m3u8", WIDEVINE_KEYFORMAT).unwrap();
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
