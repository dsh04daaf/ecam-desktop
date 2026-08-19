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
use crate::wrapper::Wrapper;
use serde_json::Value;
use std::io::{BufReader, BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Se llama con los bytes que se acaban de bajar (no acumulados).
pub type Progress = Arc<dyn Fn(u64) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct TrackOutcome {
    pub path: PathBuf,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub quality_label: String,
    /// Ya estaba en disco: no se volvió a bajar.
    pub skipped: bool,
}

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
        });
    }

    // ── 1. De dónde salen los segmentos ────────────────────────────────────
    let enhanced = t["extendedAssetUrls"]["enhancedHls"].as_str().unwrap_or("");
    let (segments, quality_label) = if enhanced.is_empty() {
        // Catálogo viejo: no hay enhancedHls. `webPlayback` devuelve una playlist
        // FairPlay (30:cbcp256) que el mismo wrapper sí sabe descifrar, pero solo
        // en AAC — se avisa para que nadie crea que bajó lossless.
        let token = match wrapper_music_token(&cfg.decrypt_port).await {
            Some(tk) if !tk.is_empty() => tk,
            _ => cfg.media_user_token.clone(),
        };
        if token.is_empty() {
            return Err(Error::Track(TrackError::unavailable("sin stream lossless y sin token para el respaldo")));
        }
        let url = webplayback_media_url(&job.adam_id, &amp.bearer, &token)
            .await
            .ok_or_else(|| Error::Track(TrackError::unavailable("no hay stream disponible para este track")))?;
        let text = http().get(&url).header("User-Agent", UA).send().await?.text().await?;
        let segs = crate::hls::parse_media_playlist(&text, &url);
        if segs.is_empty() {
            return Err(Error::Track(TrackError::unavailable("la playlist de respaldo vino vacía")));
        }
        tracing::warn!("{name}: sin stream lossless, se usa el respaldo AAC de webPlayback");
        (segs, "AAC".to_string())
    } else {
        let master = http().get(enhanced).header("User-Agent", UA).send().await?.text().await?;
        let (media_url, label) = crate::hls::select_media_url(&master, enhanced, job.quality, cfg)
            .ok_or_else(|| {
                Error::Track(TrackError::unavailable(format!(
                    "este track no tiene {} (o no cabe en el máximo configurado)",
                    job.quality.display()
                )))
            })?;
        let media = http().get(&media_url).header("User-Agent", UA).send().await?.text().await?;
        let segs = crate::hls::parse_media_playlist(&media, &media_url);
        if segs.is_empty() {
            return Err(Error::Track(TrackError::unavailable("la playlist no trae segmentos")));
        }
        (segs, label)
    };

    // ── 2. Bajar los segmentos a un temporal ───────────────────────────────
    let enc_file = tempfile::NamedTempFile::new_in(&job.output_dir)?;
    {
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
                    p(chunk.len() as u64);
                }
            }
        }
        w.flush()?;
    }

    cancel.check()?;

    // ── 3. Descifrar y montar (bloqueante: el wrapper es secuencial) ────────
    let key_uris: Vec<Option<String>> = segments.iter().map(|s| s.key_uri.clone()).collect();
    let decrypt_port = cfg.decrypt_port.clone();
    let adam_id = job.adam_id.clone();
    let out_path_c = out_path.clone();
    let dir = job.output_dir.clone();

    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut wrapper = Wrapper::connect(&decrypt_port)?;
        decrypt_to_file(enc_file.path(), &out_path_c, &dir, &mut wrapper, &adam_id, &key_uris)
    })
    .await
    .map_err(|e| Error::Other(format!("la tarea de descifrado se cayó: {e}")))??;

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

    crate::tags::write(
        &out_path,
        t,
        a,
        cover.as_deref(),
        if cfg.embed_lrc { lrc.as_deref() } else { None },
    )?;

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
    })
}

/// Lee el archivo cifrado caja por caja, descifra y escribe el MP4 final.
///
/// Se hace en dos ficheros temporales y un `rename` al final: si algo falla a
/// medias, en la carpeta del usuario no queda un .m4a roto que luego parezca
/// descargado (y que el `skip` de la próxima vez daría por bueno).
fn decrypt_to_file(
    enc_path: &Path,
    out_path: &Path,
    dir: &Path,
    wrapper: &mut Wrapper,
    adam_id: &str,
    key_uris: &[Option<String>],
) -> Result<()> {
    let mut enc = BufReader::new(std::fs::File::open(enc_path)?);
    let mut raw = tempfile::tempfile_in(dir)?;
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
    let tmp_out = tempfile::NamedTempFile::new_in(dir)?;
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
