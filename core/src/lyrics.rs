//! TTML de Apple → LRC.
//!
//! Apple publica tres formas: sincronizada por línea, por sílaba (`Word`) y sin
//! sincronizar (`None`). Las dos primeras se guardan con marca de tiempo por
//! línea; la tercera es texto plano y **no** lleva marcas inventadas.

use crate::error::Result;
use quick_xml::events::Event;
use quick_xml::Reader;

/// `hh:mm:ss.mmm`, `mm:ss.mmm` o `ss.mmm` → (minutos totales, segundos, centésimas).
fn parse_time(t: &str) -> (u32, u32, u32) {
    let t = t.trim();
    let mut parts: Vec<&str> = t.split(':').collect();
    let mut hours = 0u32;
    let mut minutes = 0u32;
    if parts.len() == 3 {
        hours = parts.remove(0).parse().unwrap_or(0);
        minutes = parts.remove(0).parse().unwrap_or(0);
    } else if parts.len() == 2 {
        minutes = parts.remove(0).parse().unwrap_or(0);
    }
    let secs_part = parts.first().copied().unwrap_or("0");
    let (s, cs) = match secs_part.split_once('.') {
        Some((s, ms)) => {
            // Se normaliza a tres dígitos antes de pasar a centésimas: ".5" son
            // 500 ms, no 5.
            let ms3: String = ms.chars().take(3).collect();
            let ms3 = format!("{:0<3}", ms3);
            (s.parse().unwrap_or(0), ms3.parse::<u32>().unwrap_or(0) / 10)
        }
        None => (secs_part.parse().unwrap_or(0), 0),
    };
    (minutes + hours * 60, s, cs)
}

pub fn ttml_to_lrc(ttml: &str) -> Result<String> {
    let mut reader = Reader::from_str(ttml);
    reader.config_mut().trim_text(false);

    let mut timing_is_none = false;
    let mut in_body = false;
    let mut in_p = false;
    let mut begin = String::new();
    let mut text = String::new();
    let mut lines: Vec<String> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.local_name();
                let name = String::from_utf8_lossy(name.as_ref()).to_string();
                if name == "tt" {
                    for a in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(a.key.local_name().as_ref()).to_string();
                        if key == "timing" {
                            timing_is_none = a.unescape_value().unwrap_or_default() == "None";
                        }
                    }
                } else if name == "body" {
                    in_body = true;
                } else if name == "p" && in_body {
                    in_p = true;
                    text.clear();
                    begin.clear();
                    for a in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(a.key.local_name().as_ref()).to_string();
                        if key == "begin" {
                            begin = a.unescape_value().unwrap_or_default().to_string();
                        }
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_p {
                    text.push_str(&e.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name();
                let name = String::from_utf8_lossy(name.as_ref()).to_string();
                if name == "p" && in_p {
                    in_p = false;
                    let content = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if content.is_empty() {
                        // Una línea vacía en el TTML es un silencio, no una línea.
                    } else if timing_is_none {
                        lines.push(content);
                    } else if !begin.is_empty() {
                        let (m, s, cs) = parse_time(&begin);
                        lines.push(format!("[{m:02}:{s:02}.{cs:02}]{content}"));
                    }
                } else if name == "body" {
                    in_body = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // TTML roto: se devuelve lo que se haya podido leer
            _ => {}
        }
        buf.clear();
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convierte_ttml_sincronizado_a_lrc() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="Line">
<body><div>
<p begin="00:12.500">Primera línea</p>
<p begin="1:05.25">Segunda línea</p>
</div></body></tt>"#;
        let lrc = ttml_to_lrc(ttml).unwrap();
        assert_eq!(lrc, "[00:12.50]Primera línea\n[01:05.25]Segunda línea");
    }

    #[test]
    fn sin_sincronizar_no_inventa_marcas() {
        let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="None">
<body><div><p>Solo texto</p><p>Otra</p></div></body></tt>"#;
        let lrc = ttml_to_lrc(ttml).unwrap();
        assert_eq!(lrc, "Solo texto\nOtra");
    }

    #[test]
    fn las_horas_se_suman_a_los_minutos() {
        assert_eq!(parse_time("01:02:03.400"), (62, 3, 40));
    }

    #[test]
    fn una_decima_no_son_centesimas() {
        assert_eq!(parse_time("00:01.5"), (0, 1, 50), "\".5\" son 500 ms");
    }
}
