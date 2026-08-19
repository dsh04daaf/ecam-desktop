//! Carátulas y artwork animado.

use crate::amp::http;
use crate::config::Config;
use crate::error::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Sustituye `{w}` y `{h}` en la plantilla que da Apple.
fn sized_url(template: &str, w: &str, h: &str) -> String {
    template.replace("{w}", w).replace("{h}", h)
}

/// Carátula para embeber, al tamaño del config.
pub async fn fetch_cover(art: &Value, cover_size: &str) -> Option<Vec<u8>> {
    let template = art["url"].as_str()?;
    let (w, h) = cover_size.split_once('x').unwrap_or((cover_size, cover_size));
    let r = http().get(sized_url(template, w, h)).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.bytes().await.ok().map(|b| b.to_vec())
}

/// Carátula suelta a **la resolución que Apple reporta**, no a un tamaño fijo:
/// hay portadas de 6000×6000 y pedirlas a 1200 las tira a la basura.
pub async fn save_cover(art: &Value, dir: &Path) -> Result<Option<PathBuf>> {
    let Some(template) = art["url"].as_str() else { return Ok(None) };
    let w = art["width"].as_u64().unwrap_or(3000).to_string();
    let h = art["height"].as_u64().unwrap_or(3000).to_string();

    let r = http().get(sized_url(template, &w, &h)).send().await?;
    if !r.status().is_success() {
        return Ok(None);
    }
    let bytes = r.bytes().await?;
    let path = dir.join("cover.jpg");
    tokio::fs::write(&path, &bytes).await?;
    Ok(Some(path))
}

/// Las dos variantes que publica Apple, con sus nombres alternativos.
fn animated_urls(attrs: &Value) -> Vec<(&'static str, String)> {
    let ev = &attrs["editorialVideo"];
    let pick = |a: &str, b: &str| -> Option<String> {
        let block = if ev[a].is_object() { &ev[a] } else { &ev[b] };
        block["video"].as_str().map(String::from)
    };
    let mut out = Vec::new();
    if let Some(u) = pick("motionSquareVideo1x1", "motionDetailSquare") {
        out.push(("square", u));
    }
    if let Some(u) = pick("motionTallVideo3x4", "motionDetailTall") {
        out.push(("tall", u));
    }
    out
}

/// El artwork animado viene en HLS, pero el segmento de init (`EXT-X-MAP`) **ya
/// es el MP4 completo**: no hay que pegar segmentos, basta con resolverlo.
async fn resolve_mp4(m3u8_url: &str) -> Result<String> {
    let master = http().get(m3u8_url).send().await?.text().await?;
    let lines: Vec<&str> = master.lines().collect();
    let mut best: Option<(u64, String)> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("#EXT-X-STREAM-INF") {
            let bw = crate::hls::attrs(l)
                .iter()
                .find(|(k, _)| k == "BANDWIDTH")
                .and_then(|(_, v)| v.parse::<u64>().ok())
                .unwrap_or(0);
            if let Some(uri) = lines.get(i + 1).map(|s| s.trim()) {
                if uri.starts_with("http") && best.as_ref().is_none_or(|(b, _)| bw > *b) {
                    best = Some((bw, uri.to_string()));
                }
            }
        }
    }
    let (_, variant_url) = best
        .ok_or_else(|| crate::error::Error::Other("el artwork animado no trae variantes".into()))?;

    let variant = http().get(&variant_url).send().await?.text().await?;
    let map = variant
        .lines()
        .find_map(|l| l.strip_prefix("#EXT-X-MAP:URI=\""))
        .and_then(|rest| rest.split('"').next())
        .ok_or_else(|| crate::error::Error::Other("sin EXT-X-MAP en el artwork animado".into()))?;

    Ok(match reqwest::Url::parse(&variant_url).and_then(|b| b.join(map)) {
        Ok(u) => u.to_string(),
        Err(_) => map.to_string(),
    })
}

/// Baja las dos variantes. Necesita ffmpeg — es lo **único** del camino de audio
/// que lo necesita, así que si falta, se avisa y se sigue con la descarga.
pub async fn download_animated(
    cfg: &Config,
    attrs: &Value,
    dir: &Path,
    basename: &str,
) -> Vec<PathBuf> {
    let mut saved = Vec::new();
    for (variant, m3u8) in animated_urls(attrs) {
        let out = dir.join(format!("{basename} [{variant}].mp4"));
        match resolve_mp4(&m3u8).await {
            Ok(mp4_url) => {
                let status = tokio::process::Command::new(&cfg.ffmpeg_path)
                    .args(["-y", "-i", &mp4_url, "-c", "copy", "-movflags", "+faststart"])
                    .arg(&out)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
                match status {
                    Ok(s) if s.success() => saved.push(out),
                    Ok(s) => tracing::warn!("ffmpeg falló con el artwork {variant}: {s}"),
                    Err(e) => tracing::warn!("no se pudo lanzar ffmpeg ({}): {e}", cfg.ffmpeg_path.display()),
                }
            }
            Err(e) => tracing::warn!("artwork animado {variant}: {e}"),
        }
    }
    saved
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn la_plantilla_de_tamano_se_sustituye() {
        assert_eq!(sized_url("http://x/{w}x{h}bb.jpg", "1200", "1200"), "http://x/1200x1200bb.jpg");
    }

    #[test]
    fn reconoce_los_nombres_alternativos_del_artwork() {
        let attrs = json!({"editorialVideo": {
            "motionDetailSquare": {"video": "http://a/sq.m3u8"},
            "motionTallVideo3x4": {"video": "http://a/tall.m3u8"}
        }});
        let urls = animated_urls(&attrs);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].0, "square");
    }
}
