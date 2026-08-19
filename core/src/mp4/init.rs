//! Limpieza del segmento de init (ftyp + moov): quitarle el cifrado al `moov`.
//!
//! Lo que hay que saber y no es evidente:
//!   * el `stsd` de Apple trae **dos entradas idénticas** tras descifrar. Hay que
//!     quedarse con la primera y forzar `entry_count = 1`, o ffmpeg no reconoce
//!     el stream (incidente 2026-07-31).
//!   * el códec real vive en el `frma` dentro del `sinf`; la entrada visible es
//!     `enca`, que es solo el envoltorio cifrado.
//!   * `sbgp`/`sgpd` de tipo `seig`/`seam` describen el cifrado por grupos: si se
//!     quedan, el reproductor cree que el archivo sigue protegido.

use super::{be_u32, boxes, find, mk, mk_into, rebuild, FourCc};

/// Lo que el `tenc` dice sobre el cifrado del track.
#[derive(Debug, Clone, Default)]
pub struct TencInfo {
    pub crypt_byte_block: u8,
    pub skip_byte_block: u8,
    pub is_protected: u8,
    pub iv_size: u8,
    pub kid: Vec<u8>,
    pub const_iv: Vec<u8>,
    /// Códec real sacado del `frma` (`alac`, `mp4a`, `ec-3`…).
    pub codec: String,
}

impl TencInfo {
    /// Sin `tenc` el default es IV de 16 bytes, que es lo que usa ALAC.
    pub fn fallback() -> Self {
        Self { iv_size: 16, codec: "alac".into(), ..Default::default() }
    }
}

/// tenc = FullBox(4) + reservado(1) + patrón(1) + is_protected(1) + iv_size(1) + KID(16)
/// y, cuando `iv_size == 0`, un IV constante al final.
pub fn parse_tenc(payload: &[u8]) -> TencInfo {
    if payload.len() < 24 {
        return TencInfo::fallback();
    }
    let crypt_skip = payload[5];
    let mut info = TencInfo {
        crypt_byte_block: (crypt_skip >> 4) & 0xF,
        skip_byte_block: crypt_skip & 0xF,
        is_protected: payload[6],
        iv_size: payload[7],
        kid: payload[8..24].to_vec(),
        const_iv: Vec::new(),
        codec: String::new(),
    };
    // IV constante: solo existe si no hay IV por sample.
    if info.is_protected == 1 && info.iv_size == 0 && payload.len() > 24 {
        let n = payload[24] as usize;
        if payload.len() >= 25 + n {
            info.const_iv = payload[25..25 + n].to_vec();
        }
    }
    info
}

/// Transforma el `stsd`: `enca` → códec real, sin `sinf`, y una sola entrada.
fn transform_stsd(payload: &[u8]) -> (Vec<u8>, TencInfo) {
    if payload.len() < 8 {
        return (payload.to_vec(), TencInfo::fallback());
    }
    let version = payload[0];
    let flags = &payload[1..4];
    let entries = &payload[8..];

    let mut tenc = TencInfo::fallback();
    let mut clean_entry = Vec::new();

    if let Some(first) = boxes(entries).next() {
        if first.is(b"enca") && first.payload.len() >= 28 {
            let audio_header = &first.payload[..28];
            let children = &first.payload[28..];

            let mut codec = "alac".to_string();
            for c in boxes(children) {
                if c.is(b"sinf") {
                    if let Some(frma) = find(c.payload, &[b"frma"]) {
                        if frma.len() >= 4 {
                            codec = String::from_utf8_lossy(&frma[..4]).trim().to_string();
                        }
                    }
                    if let Some(t) = find(c.payload, &[b"schi", b"tenc"]) {
                        tenc = parse_tenc(t);
                    }
                    break;
                }
            }

            // Se conserva todo menos el sinf: ahí vivía el cifrado.
            let mut rest = Vec::new();
            for c in boxes(children) {
                if !c.is(b"sinf") {
                    mk_into(&mut rest, &c.kind, c.payload);
                }
            }
            let mut entry_payload = Vec::with_capacity(audio_header.len() + rest.len());
            entry_payload.extend_from_slice(audio_header);
            entry_payload.extend_from_slice(&rest);

            let mut kind: FourCc = *b"alac";
            let bytes = codec.as_bytes();
            for (i, slot) in kind.iter_mut().enumerate() {
                *slot = if i < bytes.len() { bytes[i] } else { b' ' };
            }
            clean_entry = mk(&kind, &entry_payload);
            tenc.codec = codec;
        } else {
            // Ya venía sin cifrar: se respeta tal cual.
            clean_entry = mk(&first.kind, first.payload);
            tenc.codec = String::from_utf8_lossy(&first.kind).trim().to_string();
        }
    }

    let mut out = Vec::with_capacity(clean_entry.len() + 8);
    out.push(version);
    out.extend_from_slice(flags);
    out.extend_from_slice(&1u32.to_be_bytes()); // entry_count = 1, SIEMPRE
    out.extend_from_slice(&clean_entry);
    (out, tenc)
}

/// ¿Es un `sbgp`/`sgpd` que describe cifrado? Se mira el `grouping_type`.
fn is_crypto_group(kind: FourCc, payload: &[u8]) -> bool {
    (&kind == b"sbgp" || &kind == b"sgpd")
        && payload.len() >= 8
        && matches!(&payload[4..8], b"seig" | b"seam")
}

fn transform_stbl(payload: &[u8], tenc: &mut TencInfo) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    for b in boxes(payload) {
        if b.is(b"stsd") {
            let (new_stsd, t) = transform_stsd(b.payload);
            *tenc = t;
            mk_into(&mut out, b"stsd", &new_stsd);
        } else if is_crypto_group(b.kind, b.payload) {
            continue;
        } else {
            mk_into(&mut out, &b.kind, b.payload);
        }
    }
    out
}

/// Limpia el init entero. Devuelve (ftyp+moov limpios, info del tenc).
pub fn transform_init(raw_init: &[u8]) -> (Vec<u8>, TencInfo) {
    let mut tenc = TencInfo::fallback();
    let mut out = Vec::with_capacity(raw_init.len());

    for b in boxes(raw_init) {
        if b.is(b"moov") {
            let moov = rebuild(b.payload, |kind, payload| {
                if &kind == b"pssh" {
                    return None; // metadata de DRM: fuera
                }
                if &kind == b"trak" {
                    return Some(rebuild(payload, |k2, p2| {
                        if &k2 == b"mdia" {
                            Some(rebuild(p2, |k3, p3| {
                                if &k3 == b"minf" {
                                    Some(rebuild(p3, |k4, p4| {
                                        if &k4 == b"stbl" {
                                            Some(transform_stbl(p4, &mut tenc))
                                        } else {
                                            Some(p4.to_vec())
                                        }
                                    }))
                                } else {
                                    Some(p3.to_vec())
                                }
                            }))
                        } else {
                            Some(p2.to_vec())
                        }
                    }));
                }
                Some(payload.to_vec())
            });
            mk_into(&mut out, b"moov", &moov);
        } else {
            out.extend_from_slice(&raw_init[b.start..b.end]);
        }
    }
    (out, tenc)
}

/// Timescales y duración por defecto del `trex`, que hacen falta luego para
/// reconstruir la duración real del track.
#[derive(Debug, Clone, Copy, Default)]
pub struct MoovTiming {
    pub movie_timescale: u32,
    pub media_timescale: u32,
    /// Respaldo cuando un `tfhd` no trae `default_sample_duration`.
    pub trex_default_duration: u32,
}

pub fn read_timing(moov_payload: &[u8]) -> MoovTiming {
    let mut t = MoovTiming::default();
    if let Some(mvhd) = find(moov_payload, &[b"mvhd"]) {
        let off = if !mvhd.is_empty() && mvhd[0] == 1 { 20 } else { 12 };
        t.movie_timescale = be_u32(mvhd, off);
    }
    if let Some(mdhd) = find(moov_payload, &[b"trak", b"mdia", b"mdhd"]) {
        let off = if !mdhd.is_empty() && mdhd[0] == 1 { 20 } else { 12 };
        t.media_timescale = be_u32(mdhd, off);
    }
    if let Some(trex) = find(moov_payload, &[b"mvex", b"trex"]) {
        if trex.len() >= 16 {
            t.trex_default_duration = be_u32(trex, 12);
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4::mk;

    fn enca_moov() -> Vec<u8> {
        let frma = mk(b"frma", b"alac");
        let tenc_payload = {
            let mut p = vec![0u8; 24];
            p[5] = 0x00; // sin patrón
            p[6] = 1;    // protegido
            p[7] = 16;   // iv de 16
            p
        };
        let tenc = mk(b"tenc", &tenc_payload);
        let schi = mk(b"schi", &tenc);
        let mut sinf_payload = frma.clone();
        sinf_payload.extend_from_slice(&schi);
        let sinf = mk(b"sinf", &sinf_payload);

        let mut enca_payload = vec![0u8; 28];
        enca_payload.extend_from_slice(&mk(b"alac", b"magic-cookie"));
        enca_payload.extend_from_slice(&sinf);
        let enca = mk(b"enca", &enca_payload);

        // stsd con DOS entradas idénticas, como lo manda Apple
        let mut stsd_payload = vec![0u8, 0, 0, 0];
        stsd_payload.extend_from_slice(&2u32.to_be_bytes());
        stsd_payload.extend_from_slice(&enca);
        stsd_payload.extend_from_slice(&enca);
        let stsd = mk(b"stsd", &stsd_payload);

        let sgpd = mk(b"sgpd", &{
            let mut p = vec![0u8; 4];
            p.extend_from_slice(b"seig");
            p
        });
        let mut stbl_payload = stsd;
        stbl_payload.extend_from_slice(&sgpd);
        let stbl = mk(b"stbl", &stbl_payload);
        let minf = mk(b"minf", &stbl);
        let mdia = mk(b"mdia", &minf);
        let trak = mk(b"trak", &mdia);
        let pssh = mk(b"pssh", b"drm");
        let mut moov_payload = trak;
        moov_payload.extend_from_slice(&pssh);
        mk(b"moov", &moov_payload)
    }

    #[test]
    fn el_init_queda_sin_cifrado_y_con_una_sola_entrada() {
        let init = enca_moov();
        let (clean, tenc) = transform_init(&init);

        assert_eq!(tenc.codec, "alac");
        assert_eq!(tenc.iv_size, 16);

        let moov = find(&clean, &[b"moov"]).unwrap();
        let stsd = find(moov, &[b"trak", b"mdia", b"minf", b"stbl", b"stsd"]).unwrap();
        assert_eq!(be_u32(stsd, 4), 1, "el stsd debe quedar con UNA entrada");

        let entry = boxes(&stsd[8..]).next().unwrap();
        assert_eq!(&entry.kind, b"alac", "enca debe volverse el códec real");
        assert!(find(entry.payload, &[b"sinf"]).is_none(), "el sinf no debe sobrevivir");

        let stbl = find(moov, &[b"trak", b"mdia", b"minf", b"stbl"]).unwrap();
        assert!(boxes(stbl).all(|b| !b.is(b"sgpd")), "seig/seam fuera");
        assert!(boxes(moov).all(|b| !b.is(b"pssh")), "pssh fuera");
    }
}
