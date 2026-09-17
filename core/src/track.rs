//! Descarga de un track: del m3u8 al .m4a etiquetado.
//!
//! La diferencia grande con el original: **nada del track vive en RAM**. Los
//! segmentos cifrados van a un temporal, se descifran leyendo caja por caja
//! escribiendo los samples a otro temporal, y el MP4 final se monta copiando ese
//! temporal. El coste en memoria de un mix de una hora es el mismo que el de un
//! single de tres minutos.

use crate::amp::{http, wrapper_music_token, Amp, UA};
use crate::config::{Config, Quality};
use crate::error::{Error, Result, TrackError};
use crate::mp4::{self, assemble::SampleTables, frag, init::TencInfo};
use crate::wrapper::{KeyedDecryptor, Wrapper};
use serde_json::Value;
use std::io::{BufReader, BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// En qué va la descarga de un track.
///
/// Antes solo se informaba de bytes bajados, así que en cuanto empezaba el
/// descifrado la pantalla se quedaba congelada en la última cifra y parecía
/// colgada — que es justo lo que pasa: descifrar un track son miles de idas y
/// vueltas al wrapper y no baja ni un byte más.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "stage", content = "value")]
pub enum Stage {
    /// Bytes recién bajados (no acumulados).
    Downloading(u64),
    /// Fragmentos descifrados de un total.
    Decrypting { done: usize, total: usize },
    Tagging,
}

pub type Progress = Arc<dyn Fn(Stage) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct TrackOutcome {
    pub path: PathBuf,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub quality_label: String,
    /// Ya estaba en disco: no se volvió a bajar.
    pub skipped: bool,
    /// Cuánto costó cada fase, en segundos. Va a la vista para no tener que
    /// adivinar dónde se va el tiempo cuando algo va lento en otra máquina.
    pub secs_download: f32,
    pub secs_decrypt: f32,
    pub secs_total: f32,
}

#[derive(Clone)]
pub struct TrackJob {
    pub track: Value,
    pub album: Value,
    pub adam_id: String,
    pub track_num: u32,
    /// Para playlists: normaliza el disco **del nombre** (ver `naming`).
    pub disc_override: Option<u32>,
    pub output_dir: PathBuf,
    pub quality: Quality,
    /// Carátula ya bajada por el álbum, para no pedirla una vez por track.
    pub cover: Option<Vec<u8>>,
}

pub async fn download_track(
    cfg: &Config,
    amp: &Amp,
    job: TrackJob,
    progress: Option<Progress>,
    cancel: &crate::cancel::Cancel,
) -> Result<TrackOutcome> {
    cancel.check()?;
    let t = &job.track;
    let a = &job.album;
    let name = t["name"].as_str().unwrap_or("Desconocido").to_string();
    let artist = t["artistName"].as_str().unwrap_or("Desconocido").to_string();
    let album_name = a["name"].as_str().unwrap_or("Desconocido").to_string();

    let filename = crate::naming::track_filename(
        cfg,
        &job.adam_id,
        &name,
        job.track_num,
        t["discNumber"].as_u64().unwrap_or(1) as u32,
        job.disc_override,
        a["trackCount"].as_u64().unwrap_or(0) as u32,
        job.quality,
        t["contentRating"].as_str().unwrap_or(""),
    );
    tokio::fs::create_dir_all(&job.output_dir).await?;
    let out_path = job.output_dir.join(&filename);

    if out_path.exists() {
        return Ok(TrackOutcome {
            path: out_path,
            name,
            artist,
            album: album_name,
            quality_label: job.quality.display().into(),
            skipped: true,
            secs_download: 0.0,
            secs_decrypt: 0.0,
            secs_total: 0.0,
        });
    }
    let t_start = std::time::Instant::now();

    // ── 1. De dónde salen los segmentos ────────────────────────────────────
    let enhanced = t["extendedAssetUrls"]["enhancedHls"].as_str().unwrap_or("");
    // Catálogo viejo ya descifrado por Widevine (fMP4 limpio en un temporal).
    let mut legacy_clear: Option<tempfile::NamedTempFile> = None;
    let mut legacy_error = String::new();
    let (segments, quality_label) = if enhanced.is_empty() {
        // Catálogo viejo: no hay enhancedHls y solo existe en AAC 256. En Atmos o
        // binaural no hay nada que bajar: se omite, no es un fallo.
        if matches!(job.quality, Quality::Atmos | Quality::Binaural) {
            return Err(Error::Track(TrackError::skipped(format!(
                "{name}: no tiene versión {}", job.quality.display()
            ))));
        }
        let token = match wrapper_music_token(&cfg.decrypt_port).await {
            Some(tk) if !tk.is_empty() => tk,
            _ => cfg.media_user_token.clone(),
        };
        if token.is_empty() {
            return Err(Error::Track(TrackError::unavailable("sin stream lossless y sin token para el respaldo")));
        }
        // 1) Widevine (flavor 28:ctrp256, cenc). Descifra también las pistas con
        //    llave FairPlay `afs_`, que el key server rechaza SIEMPRE, y no pasa por
        //    el wrapper. Probado en el bot y en AMDL el 2026-09-17: PCM idéntico al
        //    de mp4decrypt con la misma llave.
        match legacy_widevine(cfg, amp, &job.adam_id, &token, &job.output_dir, cancel, progress.as_ref()).await {
            Ok(tmp) => legacy_clear = Some(tmp),
            Err(e) => {
                tracing::warn!("{name}: legacy por Widevine falló ({e}); se prueba FairPlay");
                legacy_error = e.to_string();
            }
        }
        if legacy_clear.is_some() {
            (Vec::new(), "AAC".to_string())
        } else {
        // 2) Respaldo: la playlist FairPlay (30:cbcp256) por el wrapper, como antes.
        let url = webplayback_media_url(&job.adam_id, &amp.bearer, &token)
            .await
            .ok_or_else(|| Error::Track(TrackError::unavailable(format!(
                "pista antigua que no se pudo descifrar ({legacy_error})"
            ))))?;
        let text = http().get(&url).header("User-Agent", UA).send().await?.text().await?;
        let segs = crate::hls::parse_media_playlist(&text, &url);
        if segs.is_empty() {
            return Err(Error::Track(TrackError::unavailable("la playlist de respaldo vino vacía")));
        }
        tracing::warn!("{name}: sin stream lossless, se usa el respaldo AAC de webPlayback");
        (segs, "AAC".to_string())
        }
    } else {
        let master = http().get(enhanced).header("User-Agent", UA).send().await?.text().await?;
        let mut chosen = crate::hls::select_media_url(&master, enhanced, job.quality, cfg);
        if chosen.is_none() && job.quality == Quality::Alac {
            // Pista sin ALAC: se entrega en AAC (lo que avisa la card) en vez de fallar.
            chosen = crate::hls::select_media_url(&master, enhanced, Quality::Aac, cfg);
            if chosen.is_some() {
                tracing::warn!("{name}: sin ALAC, se baja en AAC");
            }
        }
        let (media_url, label) = chosen.ok_or_else(|| {
            let motivo = format!(
                "{name}: no tiene versión {} (o no cabe en el máximo configurado)",
                job.quality.display()
            );
            // Sin la versión Atmos/binaural pedida no es un fallo: se omite.
            if matches!(job.quality, Quality::Atmos | Quality::Binaural) {
                Error::Track(TrackError::skipped(motivo))
            } else {
                Error::Track(TrackError::unavailable(motivo))
            }
        })?;
        let media = http().get(&media_url).header("User-Agent", UA).send().await?.text().await?;
        let segs = crate::hls::parse_media_playlist(&media, &media_url);
        if segs.is_empty() {
            return Err(Error::Track(TrackError::unavailable("la playlist no trae segmentos")));
        }
        (segs, label)
    };

    // Llave FairPlay de formato antiguo (skd://itunes.apple.com/afs_...): la traen
    // las pistas de catálogo viejo que caen al respaldo de webPlayback. El key
    // server de Apple las rechaza SIEMPRE con "Invalid CKC error", así que ni se
    // baja el audio ni se pide la llave: no tiene arreglo.
    if legacy_clear.is_none()
        && segments
            .iter()
            .any(|s| s.key_uri.as_deref().is_some_and(|u| u.contains("://itunes.apple.com/afs_")))
    {
        return Err(Error::Track(TrackError::unavailable(format!(
            "pista antigua (llave afs_) que no se pudo descifrar ({legacy_error})"
        ))));
    }

    let t_download = std::time::Instant::now();
    // ── 2. Bajar los segmentos a un temporal ───────────────────────────────
    // Los temporales van a una carpeta propia, NO a la del usuario: ver un
    // montón de .tmp junto a la música (y que sobrevivan a un cierre a lo
    // bruto) es feo y confunde.
    let tmp_dir = scratch_dir(&job.output_dir)?;
    let enc_file = tempfile::NamedTempFile::new_in(&tmp_dir)?;
    if legacy_clear.is_none() {
        let mut w = BufWriter::new(enc_file.as_file());
        // El HLS de Apple llega de dos formas: un solo archivo repetido en todos
        // los #EXTINF, o un .m4a por fragmento. Deduplicar por URL cubre las dos.
        let mut seen = std::collections::HashSet::new();
        for seg in &segments {
            if !seen.insert(seg.url.clone()) {
                continue;
            }
            let mut resp = http().get(&seg.url).header("User-Agent", UA).send().await?;
            if !resp.status().is_success() {
                return Err(Error::Track(TrackError::transient(format!(
                    "el segmento respondió {}",
                    resp.status().as_u16()
                ))));
            }
            while let Some(chunk) = resp.chunk().await? {
                // Se mira por trozo, no por segmento: un track lossless es un
                // solo archivo y esperar al siguiente segmento sería no cancelar.
                cancel.check()?;
                w.write_all(&chunk)?;
                if let Some(p) = &progress {
                    p(Stage::Downloading(chunk.len() as u64));
                }
            }
        }
        w.flush()?;
    }

    let secs_download = t_download.elapsed().as_secs_f32();
    cancel.check()?;

    let t_decrypt = std::time::Instant::now();
    // ── 3. Descifrar y montar (bloqueante: el wrapper es secuencial) ────────
    let key_uris: Vec<Option<String>> = segments.iter().map(|s| s.key_uri.clone()).collect();
    let decrypt_port = cfg.decrypt_port.clone();
    let motor = cfg.decrypt_engine.to_ascii_lowercase();
    let key_port = cfg.key_port;
    let adam_id = job.adam_id.clone();
    let out_path_c = out_path.clone();
    let progress_c = progress.clone();
    let total_segments = segments.len();
    let dir = tmp_dir.clone();
    let final_dir = job.output_dir.clone();

    // Con el motor temari las plantillas se piden AQUI, en async, antes de
    // entrar al hilo bloqueante: los URI de llave ya se conocen (vienen del
    // m3u8), asi que no hace falta HTTP bloqueante dentro de spawn_blocking.
    // "auto" mira si el wrapper instalado tiene key server. Hace falta porque la
    // app se distribuye y el usuario puede arrastrar una distro vieja sin el
    // puerto 40020: forzar temari ahi dejaria la app inservible.
    let usar_temari = legacy_clear.is_none() && match motor.as_str() {
        "wrapper" => false,
        "temari" => true,
        _ => crate::temari::TemariDecrypter::probe(&decrypt_port, key_port).await,
    };
    if motor != "wrapper" && !usar_temari {
        tracing::info!(
            "el wrapper no expone key server en {key_port}: se descifra por el camino antiguo"
        );
    }
    let mut temari_pre = if usar_temari && legacy_clear.is_none() {
        Some(
            crate::temari::TemariDecrypter::prepare(&decrypt_port, key_port, &adam_id, &key_uris)
                .await?,
        )
    } else {
        None
    };

    let legacy_path = legacy_clear.as_ref().map(|f| f.path().to_path_buf());
    tokio::task::spawn_blocking(move || -> Result<()> {
        // Legacy ya descifrado por Widevine: se monta igual que el resto, con un
        // "descifrador" que deja los samples tal cual.
        if let Some(clear) = legacy_path {
            let mut pass = PassThrough;
            let frags = count_fragments(&clear).unwrap_or(0);
            return decrypt_to_file(&clear, &out_path_c, &dir, &final_dir, &mut pass, &adam_id, &[], frags, progress_c);
        }
        let mut wrapper_conn;
        let dec: &mut dyn KeyedDecryptor = match temari_pre.as_mut() {
            Some(t) => t,
            None => {
                wrapper_conn = Wrapper::connect(&decrypt_port)?;
                &mut wrapper_conn
            }
        };
        decrypt_to_file(
            enc_file.path(), &out_path_c, &dir, &final_dir, dec, &adam_id, &key_uris,
            total_segments, progress_c,
        )
    })
    .await
    .map_err(|e| Error::Other(format!("la tarea de descifrado se cayó: {e}")))??;

    let secs_decrypt = t_decrypt.elapsed().as_secs_f32();
    // El fMP4 legacy en claro ya está montado: fuera antes de limpiar la carpeta de
    // trabajo, o el `remove_dir` del final la encuentra llena y se queda.
    drop(legacy_clear);


    // ── 4. Carátula, letras y etiquetas ────────────────────────────────────
    let cover = match job.cover {
        Some(c) => Some(c),
        None => {
            let art = if t["artwork"].is_object() { &t["artwork"] } else { &a["artwork"] };
            crate::artwork::fetch_cover(art, &cfg.cover_size).await
        }
    };

    let lrc = match amp.lyrics_ttml(&job.adam_id).await {
        Ok(Some(ttml)) => crate::lyrics::ttml_to_lrc(&ttml).ok().filter(|s| !s.is_empty()),
        Ok(None) => None,
        // Que falten las letras nunca tumba la descarga: el audio ya está bien.
        Err(e) => {
            tracing::debug!("sin letras para {name}: {e}");
            None
        }
    };
    if let (Some(text), true) = (&lrc, cfg.save_lrc) {
        let lrc_path = out_path.with_extension("lrc");
        tokio::fs::write(&lrc_path, text).await.ok();
    }

    if let Some(p) = &progress {
        p(Stage::Tagging);
    }
    crate::tags::write(
        &out_path,
        t,
        a,
        cover.as_deref(),
        if cfg.embed_lrc { lrc.as_deref() } else { None },
    )?;

    // La carpeta de trabajo se va si ya no queda nada dentro (falla sin ruido si
    // otra descarga sigue usándola).
    let _ = std::fs::remove_dir(&tmp_dir);

    if cfg.save_animated_artwork {
        let stem = out_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        crate::artwork::download_animated(cfg, &a["attributes"], &job.output_dir, &stem).await;
    }

    Ok(TrackOutcome {
        path: out_path,
        name,
        artist,
        album: album_name,
        quality_label,
        skipped: false,
        secs_download,
        secs_decrypt,
        secs_total: t_start.elapsed().as_secs_f32(),
    })
}

/// Lee el archivo cifrado caja por caja, descifra y escribe el MP4 final.
///
/// Se hace en dos ficheros temporales y un `rename` al final: si algo falla a
/// medias, en la carpeta del usuario no queda un .m4a roto que luego parezca
/// descargado (y que el `skip` de la próxima vez daría por bueno).
/// Carpeta de trabajo: al lado de la de salida pero oculta, y en el mismo
/// sistema de archivos para que el `rename` final no tenga que copiar.
fn scratch_dir(output_dir: &Path) -> Result<PathBuf> {
    let dir = output_dir.join(".ecam-tmp");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[allow(clippy::too_many_arguments)]
fn decrypt_to_file(
    enc_path: &Path,
    out_path: &Path,
    tmp_dir: &Path,
    final_dir: &Path,
    wrapper: &mut dyn KeyedDecryptor,
    adam_id: &str,
    key_uris: &[Option<String>],
    total_fragments: usize,
    progress: Option<Progress>,
) -> Result<()> {
    let mut enc = BufReader::new(std::fs::File::open(enc_path)?);
    let mut raw = tempfile::tempfile_in(tmp_dir)?;
    let mut tables = SampleTables::default();

    let mut clean_init: Vec<u8> = Vec::new();
    let mut tenc = TencInfo::fallback();
    let mut timing = mp4::init::MoovTiming::default();
    let mut pending_moof: Option<Vec<u8>> = None;
    let mut seg_idx = 0usize;

    {
        let mut raw_w = BufWriter::new(&mut raw);
        let mut init_raw: Vec<u8> = Vec::new();

        while let Some((kind, box_raw)) = mp4::read_box(&mut enc)? {
            match &kind {
                b"ftyp" => init_raw.extend_from_slice(&box_raw),
                b"moov" => {
                    init_raw.extend_from_slice(&box_raw);
                    timing = mp4::init::read_timing(&box_raw[8..]);
                    let (clean, t) = mp4::init::transform_init(&init_raw);
                    clean_init = clean;
                    tenc = t;
                }
                b"moof" => pending_moof = Some(box_raw),
                b"mdat" => {
                    let Some(moof) = pending_moof.take() else {
                        continue; // mdat suelto sin su moof: no hay con qué descifrarlo
                    };
                    // La llave solo se remanda cuando el URI cambia (ver wrapper).
                    if let Some(Some(uri)) = key_uris.get(seg_idx.min(key_uris.len().saturating_sub(1))) {
                        wrapper.ensure_key(adam_id, uri)?;
                    }
                    let fragment = frag::decrypt_fragment(
                        &moof,
                        &box_raw[8..],
                        &tenc,
                        wrapper,
                        timing.trex_default_duration,
                    )?;
                    raw_w.write_all(&fragment.mdat)?;
                    tables.push_fragment(&fragment.sample_sizes, &fragment.sample_durations);
                    seg_idx += 1;
                    if let Some(p) = &progress {
                        p(Stage::Decrypting { done: seg_idx, total: total_fragments.max(seg_idx) });
                    }
                }
                // sidx, free, skip… no aportan nada al archivo final.
                _ => {}
            }
        }
        raw_w.flush()?;
    }

    if clean_init.is_empty() {
        return Err(Error::Mp4("el stream no traía moov: no se puede montar el archivo".into()));
    }
    if tables.is_empty() {
        return Err(Error::Mp4("no se descifró ningún sample".into()));
    }

    raw.seek(std::io::SeekFrom::Start(0))?;
    // El archivo a medio montar se crea en la carpeta FINAL para que el
    // `persist` sea un rename atómico y no una copia entre discos.
    let tmp_out = tempfile::NamedTempFile::new_in(final_dir)?;
    {
        let mut w = BufWriter::new(tmp_out.as_file());
        let mut reader = BufReader::new(&mut raw);
        mp4::assemble::write_mp4(&mut w, &clean_init, &tables, timing, &tenc.codec, &mut reader)?;
        w.flush()?;
    }
    tmp_out.persist(out_path).map_err(|e| Error::Io(e.error))?;

    tracing::info!(
        "montado {} ({} samples, {:.1}s)",
        out_path.display(),
        tables.sample_count(),
        tables.duration_seconds(timing.media_timescale)
    );
    Ok(())
}

/// "Descifrador" que no toca nada: para montar un fMP4 que ya viene en claro.
struct PassThrough;

impl crate::mp4::frag::Decryptor for PassThrough {
    fn decrypt(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        Ok(data.to_vec())
    }
}

impl KeyedDecryptor for PassThrough {
    fn ensure_key(&mut self, _adam_id: &str, _key_uri: &str) -> Result<()> {
        Ok(())
    }
}

fn count_fragments(path: &Path) -> Result<usize> {
    let mut r = BufReader::new(std::fs::File::open(path)?);
    let mut n = 0usize;
    while let Some((kind, _)) = mp4::read_box(&mut r)? {
        if &kind == b"moof" {
            n += 1;
        }
    }
    Ok(n)
}

/// Pista del catálogo viejo por Widevine. Devuelve el fMP4 ya descifrado (sin
/// montar) en un temporal.
///
/// Es la receta de AppleMusicDecrypt (`_rip_song_legacy`) y wrapper-manager
/// (`webplay.go`): webPlayback → asset `28:ctrp256` (cenc) → licencia Widevine con
/// el music token de la cuenta → descifrado AES-CTR nativo. Necesita las mismas
/// credenciales de Widevine que los music videos.
async fn legacy_widevine(
    cfg: &Config,
    amp: &Amp,
    adam_id: &str,
    music_token: &str,
    output_dir: &Path,
    cancel: &crate::cancel::Cancel,
    progress: Option<&Progress>,
) -> Result<tempfile::NamedTempFile> {
    let body = serde_json::json!({ "salableAdamId": adam_id });
    let v: Value = http()
        .post("https://play.music.apple.com/WebObjects/MZPlay.woa/wa/webPlayback")
        .header("Content-Type", "application/json")
        .header("Origin", "https://music.apple.com")
        .header("Referer", "https://music.apple.com/")
        .header("User-Agent", UA)
        .header("Authorization", format!("Bearer {}", amp.bearer))
        .header("x-apple-music-user-token", music_token)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    let media_url = v["songList"][0]["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["flavor"].as_str() == Some("28:ctrp256")))
        .and_then(|x| x["URL"].as_str())
        .map(String::from)
        .ok_or_else(|| Error::Other(format!(
            "webPlayback no trae el asset de Widevine (failureType={})", v["failureType"]
        )))?;

    let playlist = http().get(&media_url).header("User-Agent", UA).send().await?.text().await?;
    let mut key_uri = String::new();
    let mut files: Vec<String> = Vec::new();
    for line in playlist.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            if key_uri.is_empty() {
                key_uri = crate::hls::attr(&crate::hls::attrs(rest), "URI").unwrap_or("").to_string();
            }
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(u) = crate::hls::attr(&crate::hls::attrs(rest), "URI") {
                let u = crate::hls::join(&media_url, u);
                if !files.contains(&u) {
                    files.push(u);
                }
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            let u = crate::hls::join(&media_url, line);
            if !files.contains(&u) {
                files.push(u);
            }
        }
    }
    let (prefix, kid) = key_uri
        .split_once(',')
        .ok_or_else(|| Error::Other("la playlist de Widevine no trae llave".into()))?;
    if files.is_empty() {
        return Err(Error::Other("la playlist de Widevine no trae segmentos".into()));
    }
    let key = crate::mv::content_key(cfg, adam_id, kid, prefix, &amp.bearer, music_token).await?;
    tracing::info!("[AAC legacy] {adam_id}: licencia de Widevine obtenida");

    let tmp_dir = scratch_dir(output_dir)?;
    let enc = tempfile::NamedTempFile::new_in(&tmp_dir)?;
    {
        let mut w = BufWriter::new(enc.as_file());
        // Un solo .mp4 con rangos (init + fragmentos): se baja entero una vez.
        for u in &files {
            let mut resp = http().get(u).header("User-Agent", UA).send().await?;
            if !resp.status().is_success() {
                return Err(Error::Track(TrackError::transient(format!(
                    "el segmento respondió {}", resp.status().as_u16()
                ))));
            }
            while let Some(chunk) = resp.chunk().await? {
                cancel.check()?;
                w.write_all(&chunk)?;
                if let Some(p) = progress {
                    p(Stage::Downloading(chunk.len() as u64));
                }
            }
        }
        w.flush()?;
    }
    let clear = tempfile::NamedTempFile::new_in(&tmp_dir)?;
    let (src_path, dst_path) = (enc.path().to_path_buf(), clear.path().to_path_buf());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut src = std::fs::File::open(&src_path)?;
        let mut w = BufWriter::new(std::fs::File::create(&dst_path)?);
        crate::mv::cbcs::decrypt_file(&mut src, &mut w, &key)?;
        w.flush()?;
        Ok(())
    })
    .await
    .map_err(|e| Error::Other(format!("el descifrado legacy se cayó: {e}")))??;
    Ok(clear)
}

/// Respaldo para el catálogo viejo: la playlist FairPlay que sirve `webPlayback`.
async fn webplayback_media_url(adam_id: &str, bearer: &str, music_token: &str) -> Option<String> {
    let body = serde_json::json!({ "salableAdamId": adam_id });
    let v: Value = http()
        .post("https://play.music.apple.com/WebObjects/MZPlay.woa/wa/webPlayback")
        .header("Content-Type", "application/json")
        .header("Origin", "https://music.apple.com")
        .header("Referer", "https://music.apple.com/")
        .header("User-Agent", UA)
        .header("Authorization", format!("Bearer {bearer}"))
        .header("x-apple-music-user-token", music_token)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    v["songList"][0]["assets"]
        .as_array()?
        .iter()
        .find(|a| a["flavor"].as_str() == Some("30:cbcp256"))
        .and_then(|a| a["URL"].as_str())
        .map(String::from)
}
