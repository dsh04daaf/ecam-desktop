//! Lectura de las playlists HLS de Apple Music y elección de variante.

use crate::config::{Config, Quality};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Url;

/// Atributos de una línea `#EXT-X-...`: `CLAVE=valor` o `CLAVE="valor"`.
pub fn attrs(s: &str) -> Vec<(String, String)> {
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"([A-Z0-9-]+)=("[^"]*"|[^,]*)"#).unwrap());
    RE.captures_iter(s)
        .map(|c| {
            let k = c[1].to_string();
            let v = c[2].trim_matches('"').to_string();
            (k, v)
        })
        .collect()
}

pub fn attr<'a>(list: &'a [(String, String)], key: &str) -> Option<&'a str> {
    list.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn join(base: &str, rel: &str) -> String {
    match Url::parse(base).and_then(|b| b.join(rel)) {
        Ok(u) => u.to_string(),
        Err(_) => rel.to_string(),
    }
}

fn fmt_sample_rate(hz: u32) -> String {
    if hz % 1000 == 0 {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", hz as f64 / 1000.0)
    }
}

struct Variant {
    codec: String,
    audio: String,
    bandwidth: u64,
    uri: String,
}

fn parse_variants(master: &str) -> Vec<Variant> {
    let lines: Vec<&str> = master.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attrs(rest);
            if let Some(uri) = lines.get(i + 1).map(|l| l.trim()) {
                if !uri.starts_with('#') && !uri.is_empty() {
                    out.push(Variant {
                        codec: attr(&a, "CODECS").unwrap_or("").to_string(),
                        audio: attr(&a, "AUDIO").unwrap_or("").to_string(),
                        bandwidth: attr(&a, "AVERAGE-BANDWIDTH")
                            .or_else(|| attr(&a, "BANDWIDTH"))
                            .and_then(|b| b.parse().ok())
                            .unwrap_or(0),
                        uri: uri.to_string(),
                    });
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

/// Elige la mejor variante para la calidad pedida.
///
/// `alac_max` y `atmos_max` son **techos**, no selectores: las variantes se
/// ordenan por ancho de banda descendente y gana la primera que quepa debajo.
/// Si ninguna cabe se devuelve `None` — nunca se baja calidad en silencio.
pub fn select_media_url(
    master: &str,
    base: &str,
    quality: Quality,
    cfg: &Config,
) -> Option<(String, String)> {
    let mut variants = parse_variants(master);
    variants.sort_by(|a, b| b.bandwidth.cmp(&a.bandwidth));

    for v in &variants {
        let parts: Vec<&str> = v.audio.split('-').collect();
        match quality {
            Quality::Alac if v.codec == "alac" => {
                let rate: u32 = parts
                    .get(parts.len().wrapping_sub(2))
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0);
                if rate <= cfg.alac_max {
                    let depth: u32 = parts.last().and_then(|p| p.parse().ok()).unwrap_or(0);
                    let label = if depth > 0 {
                        format!("ALAC/{depth}-bit/{}", fmt_sample_rate(rate))
                    } else {
                        format!("ALAC/{}", fmt_sample_rate(rate))
                    };
                    return Some((join(base, &v.uri), label));
                }
            }
            Quality::Atmos => {
                if v.codec == "ec-3" && v.audio.contains("atmos") {
                    let raw = parts.last().copied().unwrap_or("");
                    // Rareza real de Apple: los bitrates de Atmos vienen con un
                    // "2" delante (2768 = 768 kbps). Sin quitarlo, NINGUNA
                    // variante pasa el techo y el track parece no existir.
                    let raw = if raw.len() == 4 && raw.starts_with('2') { &raw[1..] } else { raw };
                    match raw.parse::<u32>() {
                        Ok(br) if br <= cfg.atmos_max => {
                            return Some((join(base, &v.uri), format!("Atmos/{br} kbps")))
                        }
                        Ok(_) => {}
                        Err(_) => return Some((join(base, &v.uri), "Atmos".into())),
                    }
                } else if v.codec == "ac-3" {
                    return Some((join(base, &v.uri), "AC-3".into()));
                }
            }
            Quality::Binaural if v.codec == "mp4a.40.2" && v.audio.contains("binaural") => {
                return Some((join(base, &v.uri), "AAC Binaural".into()));
            }
            Quality::Aac if v.codec == "mp4a.40.2" => {
                // Binaural y downmix son otras calidades; si se cuelan aquí, el
                // usuario pide AAC y recibe una mezcla distinta sin enterarse.
                if v.audio.contains("binaural") || v.audio.contains("downmix") {
                    continue;
                }
                static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"audio-stereo-\d+").unwrap());
                let normalized = RE.replace_all(&v.audio, "aac");
                let effective = if cfg.aac_type == "aac-lc" { "aac" } else { cfg.aac_type.as_str() };
                if normalized == effective || cfg.aac_type == "aac" {
                    let br = parts.iter().rev().find(|p| p.parse::<u32>().is_ok());
                    let label = br.map(|b| format!("AAC/{b} kbps")).unwrap_or_else(|| "AAC".into());
                    return Some((join(base, &v.uri), label));
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub url: String,
    pub key_uri: Option<String>,
}

/// Lee la media playlist quedándose solo con las llaves de Apple.
///
/// La playlist trae varias `#EXT-X-KEY` (FairPlay, PlayReady, Widevine). Si se
/// coge la que no es, el wrapper recibe un URI que no sabe resolver y el
/// descifrado sale ruido.
pub fn parse_media_playlist(text: &str, base: &str) -> Vec<Segment> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !(l.starts_with("#EXT-X-KEY:") && !l.contains("streamingkeydelivery")))
        .collect();

    let mut segments = Vec::new();
    let mut current_key: Option<String> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let a = attrs(rest);
            current_key = attr(&a, "URI").map(|s| s.to_string());
        } else if line.starts_with("#EXTINF:") {
            // El URI puede no estar en la línea siguiente exacta.
            let mut j = i + 1;
            while j < lines.len() {
                let nl = lines[j].trim();
                if !nl.starts_with('#') && !nl.is_empty() {
                    segments.push(Segment { url: join(base, nl), key_uri: current_key.clone() });
                    i = j;
                    break;
                }
                j += 1;
            }
        }
        i += 1;
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    const MASTER: &str = r#"#EXTM3U
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=1411000,CODECS="alac",AUDIO="audio-alac-stereo-192000-24"
alac192/prog.m3u8
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=900000,CODECS="alac",AUDIO="audio-alac-stereo-44100-16"
alac44/prog.m3u8
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=768000,CODECS="ec-3",AUDIO="audio-atmos-2768"
atmos/prog.m3u8
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=256000,CODECS="mp4a.40.2",AUDIO="audio-stereo-256"
aac/prog.m3u8
#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=256000,CODECS="mp4a.40.2",AUDIO="audio-binaural-256"
binaural/prog.m3u8
"#;

    #[test]
    fn alac_respeta_el_techo_de_sample_rate() {
        let mut c = cfg();
        c.alac_max = 48000; // el usuario no quiere hi-res
        let (url, label) = select_media_url(MASTER, "https://x/y/master.m3u8", Quality::Alac, &c).unwrap();
        assert!(url.ends_with("alac44/prog.m3u8"), "debe caer a la de 44.1 kHz");
        assert_eq!(label, "ALAC/16-bit/44.1 kHz");
    }

    #[test]
    fn alac_toma_la_mejor_que_quepa() {
        let (_, label) = select_media_url(MASTER, "https://x/y/master.m3u8", Quality::Alac, &cfg()).unwrap();
        assert_eq!(label, "ALAC/24-bit/192 kHz");
    }

    #[test]
    fn atmos_le_quita_el_dos_de_delante_al_bitrate() {
        let (_, label) = select_media_url(MASTER, "https://x/y/master.m3u8", Quality::Atmos, &cfg()).unwrap();
        assert_eq!(label, "Atmos/768 kbps", "2768 en el nombre son 768 kbps de verdad");
    }

    #[test]
    fn aac_no_se_traga_la_binaural() {
        let (url, _) = select_media_url(MASTER, "https://x/y/master.m3u8", Quality::Aac, &cfg()).unwrap();
        assert!(url.ends_with("aac/prog.m3u8"));
    }

    #[test]
    fn si_nada_cabe_no_hay_fallback_silencioso() {
        let mut c = cfg();
        c.alac_max = 1; // ninguna variante cabe
        assert!(select_media_url(MASTER, "https://x/y/master.m3u8", Quality::Alac, &c).is_none());
    }

    #[test]
    fn solo_sobrevive_la_llave_de_apple() {
        let pl = r#"#EXTM3U
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="skd://itunes.apple.com/P123",KEYFORMAT="com.apple.streamingkeydelivery"
#EXT-X-KEY:METHOD=SAMPLE-AES,URI="data:text/plain;base64,AAAA",KEYFORMAT="com.microsoft.playready"
#EXTINF:6.0,
seg1.m4a
#EXTINF:6.0,
seg2.m4a
"#;
        let segs = parse_media_playlist(pl, "https://x/y/prog.m3u8");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].key_uri.as_deref(), Some("skd://itunes.apple.com/P123"));
        assert!(segs[1].url.ends_with("/y/seg2.m4a"));
    }
}
