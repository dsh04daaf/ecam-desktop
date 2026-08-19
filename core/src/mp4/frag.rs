//! Descifrado de un fragmento (`moof` + `mdat`) y limpieza del `traf`.
//!
//! La llave ya tiene que estar mandada al wrapper antes de llamar aquí: quién y
//! cuándo la manda es una decisión del orquestador, no de esta capa (ver
//! `track.rs`, y el porqué en INVENTARIO_CORE.md A1).

use super::{be_u16, be_u32, boxes, full_flags, mk_into, FourCc};
use crate::error::{Error, Result};

/// Con quién se descifra. Es un trait para poder probar toda la maquinaria de
/// cajas sin un wrapper de verdad delante.
pub trait Decryptor {
    fn decrypt(&mut self, data: &[u8]) -> Result<Vec<u8>>;
}

#[derive(Debug, Clone, Copy)]
pub struct Subsample {
    pub clear: u16,
    pub cipher: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SampleEnc {
    pub iv: Vec<u8>,
    pub subsamples: Vec<Subsample>,
}

/// `senc` con IV por sample de 8 o 16 bytes… **o de 0**.
///
/// `iv_size == 0` NO es un valor inválido: significa que el IV no viaja por
/// sample sino que es el constante del `tenc`. El catálogo de Apple manda ALAC
/// así. Tratarlo como "16 por si acaso" descoloca la lectura entera del `senc`
/// (se comen 16 bytes que eran del conteo de subsamples), los rangos cifrados
/// salen mal y el archivo queda medio descifrado: pasa el `ffprobe` y no
/// decodifica.
pub fn parse_senc(payload: &[u8], iv_size: usize) -> Vec<SampleEnc> {
    if payload.len() < 8 {
        return Vec::new();
    }
    let has_subsamples = full_flags(payload) & 0x2 != 0;
    let count = be_u32(payload, 4) as usize;
    let mut off = 8;
    let mut out = Vec::with_capacity(count);

    for _ in 0..count {
        if off + iv_size > payload.len() {
            break;
        }
        let iv = payload[off..off + iv_size].to_vec();
        off += iv_size;

        let mut subsamples = Vec::new();
        if has_subsamples {
            if off + 2 > payload.len() {
                break;
            }
            let n = be_u16(payload, off) as usize;
            off += 2;
            for _ in 0..n {
                if off + 6 > payload.len() {
                    break;
                }
                subsamples.push(Subsample { clear: be_u16(payload, off), cipher: be_u32(payload, off + 2) });
                off += 6;
            }
        }
        out.push(SampleEnc { iv, subsamples });
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct TrunInfo {
    pub data_offset: Option<i32>,
    pub sizes: Vec<Option<u32>>,
    pub durations: Vec<Option<u32>>,
}

pub fn parse_trun(payload: &[u8]) -> TrunInfo {
    let mut info = TrunInfo::default();
    if payload.len() < 8 {
        return info;
    }
    let flags = full_flags(payload);
    let count = be_u32(payload, 4) as usize;
    let mut off = 8;

    if flags & 0x001 != 0 {
        info.data_offset = Some(be_u32(payload, off) as i32);
        off += 4;
    }
    if flags & 0x004 != 0 {
        off += 4; // first_sample_flags
    }
    info.sizes.reserve(count);
    info.durations.reserve(count);
    for _ in 0..count {
        let mut dur = None;
        let mut size = None;
        if flags & 0x100 != 0 {
            dur = Some(be_u32(payload, off));
            off += 4;
        }
        if flags & 0x200 != 0 {
            size = Some(be_u32(payload, off));
            off += 4;
        }
        if flags & 0x400 != 0 {
            off += 4; // sample_flags
        }
        if flags & 0x800 != 0 {
            off += 4; // composition offset
        }
        info.sizes.push(size);
        info.durations.push(dur);
    }
    info
}

fn rebuild_trun(payload: &[u8], new_offset: i32) -> Vec<u8> {
    if payload.len() < 12 || full_flags(payload) & 0x001 == 0 {
        return payload.to_vec(); // sin campo data_offset no hay nada que mover
    }
    let mut out = payload[..8].to_vec();
    out.extend_from_slice(&new_offset.to_be_bytes());
    out.extend_from_slice(&payload[12..]);
    out
}

/// `default_sample_size` del `tfhd`, 0 si no viene.
pub fn tfhd_default_sample_size(traf: &[u8]) -> u32 {
    for b in boxes(traf) {
        if b.is(b"tfhd") && b.payload.len() >= 8 {
            let flags = full_flags(b.payload);
            let mut off = 8;
            if flags & 0x001 != 0 { off += 8 }
            if flags & 0x002 != 0 { off += 4 }
            if flags & 0x008 != 0 { off += 4 }
            if flags & 0x010 != 0 {
                return be_u32(b.payload, off);
            }
        }
    }
    0
}

/// `default_sample_duration` del `tfhd`, 0 si no viene (entonces manda el `trex`).
pub fn tfhd_default_sample_duration(traf: &[u8]) -> u32 {
    for b in boxes(traf) {
        if b.is(b"tfhd") && b.payload.len() >= 8 {
            let flags = full_flags(b.payload);
            let mut off = 8;
            if flags & 0x001 != 0 { off += 8 }
            if flags & 0x002 != 0 { off += 4 }
            if flags & 0x008 != 0 {
                return be_u32(b.payload, off);
            }
        }
    }
    0
}

const SENC_UUID: [u8; 16] = [
    0xa2, 0x39, 0x4f, 0x52, 0x5a, 0x9b, 0x4f, 0x14, 0xa2, 0x44, 0x6c, 0x42, 0x7c, 0x64, 0x8d, 0xf4,
];

fn senc_from_traf(traf: &[u8], iv_size: usize) -> Option<Vec<SampleEnc>> {
    for b in boxes(traf) {
        if b.is(b"senc") {
            return Some(parse_senc(b.payload, iv_size));
        }
        // Hay material viejo donde el senc viaja dentro de un uuid.
        if b.is(b"uuid") && b.payload.len() >= 16 && b.payload[..16] == SENC_UUID {
            return Some(parse_senc(&b.payload[16..], iv_size));
        }
    }
    None
}

fn is_crypto_group(kind: FourCc, payload: &[u8]) -> bool {
    (&kind == b"sbgp" || &kind == b"sgpd")
        && payload.len() >= 8
        && matches!(&payload[4..8], b"seig" | b"seam")
}

/// Cuántos bytes desaparecen del `traf` al quitarle el cifrado. Ese mismo número
/// es el que hay que restarle al `data_offset` de cada `trun`, o el fragmento
/// apunta a bytes que ya no están donde estaban.
fn removed_bytes(traf: &[u8]) -> i32 {
    let mut n = 0i32;
    for b in boxes(traf) {
        if matches!(&b.kind, b"senc" | b"saiz" | b"saio") || is_crypto_group(b.kind, b.payload) {
            n += (b.payload.len() + 8) as i32;
        }
    }
    n
}

fn clean_traf(traf: &[u8], offset_delta: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(traf.len());
    for b in boxes(traf) {
        if matches!(&b.kind, b"senc" | b"saiz" | b"saio") || is_crypto_group(b.kind, b.payload) {
            continue;
        }
        if b.is(b"trun") {
            let info = parse_trun(b.payload);
            let payload = match info.data_offset {
                Some(off) => rebuild_trun(b.payload, off + offset_delta),
                None => b.payload.to_vec(),
            };
            mk_into(&mut out, b"trun", &payload);
        } else {
            mk_into(&mut out, &b.kind, b.payload);
        }
    }
    out
}

/// Comprueba la firma del códec en un sample ya descifrado.
///
/// Una sesión FairPlay muerta **no da error**: devuelve ruido. Sin esta
/// comprobación el archivo se escribe entero y solo se nota al reproducirlo.
pub fn validate_sample(data: &[u8], codec: &str) -> bool {
    if data.is_empty() {
        return true;
    }
    match codec {
        // ALAC: los 2 bits altos del primer byte son el tipo de elemento
        // (SCE=000, CPE=001). Datos AES aleatorios fallan ~75% de las veces.
        "alac" => data[0] & 0xC0 == 0x00,
        // (E)AC-3: sync word fijo.
        "ec-3" | "ac-3" => data.len() >= 2 && data[0] == 0x0B && data[1] == 0x77,
        // AAC-LC no tiene ninguna firma de un byte fiable: no se inventa una.
        _ => true,
    }
}

/// Resultado de procesar un fragmento: además de los bytes limpios devuelve la
/// tabla de samples, que es lo que luego permite armar el MP4 final sin volver
/// a leer el archivo.
#[derive(Debug)]
pub struct Fragment {
    pub moof: Vec<u8>,
    pub mdat: Vec<u8>,
    pub sample_sizes: Vec<u32>,
    pub sample_durations: Vec<u32>,
}

/// Descifra un fragmento. `mdat_payload` es la carga SIN la cabecera de 8 bytes.
pub fn decrypt_fragment(
    moof_raw: &[u8],
    mdat_payload: &[u8],
    tenc: &super::init::TencInfo,
    dec: &mut impl Decryptor,
    trex_default_duration: u32,
) -> Result<Fragment> {
    let moof_payload = &moof_raw[8.min(moof_raw.len())..];
    let Some(traf) = boxes(moof_payload).find(|b| b.is(b"traf")).map(|b| b.payload) else {
        // Sin traf no hay nada que descifrar; pasa de largo.
        return Ok(Fragment {
            moof: moof_raw.to_vec(),
            mdat: mdat_payload.to_vec(),
            sample_sizes: Vec::new(),
            sample_durations: Vec::new(),
        });
    };

    // Se usa tal cual lo diga el tenc, incluido el 0 (IV constante).
    let senc = senc_from_traf(traf, tenc.iv_size as usize);
    let default_size = tfhd_default_sample_size(traf);
    let default_dur = match tfhd_default_sample_duration(traf) {
        0 => trex_default_duration,
        n => n,
    };

    // Todos los trun del traf apuntan al MISMO mdat, el que va justo detrás.
    let mut sizes: Vec<u32> = Vec::new();
    let mut durations: Vec<u32> = Vec::new();
    for b in boxes(traf) {
        if b.is(b"trun") {
            let info = parse_trun(b.payload);
            for i in 0..info.sizes.len() {
                sizes.push(info.sizes[i].unwrap_or(default_size));
                durations.push(info.durations[i].unwrap_or(default_dur));
            }
        }
    }

    if sizes.is_empty() {
        tracing::warn!("fragmento sin trun utilizable: se pasa sin descifrar");
        return Ok(Fragment {
            moof: moof_raw.to_vec(),
            mdat: mdat_payload.to_vec(),
            sample_sizes: Vec::new(),
            sample_durations: Vec::new(),
        });
    }

    let codec = tenc.codec.as_str();
    let has_subsamples = senc.as_ref().is_some_and(|s| s.iter().any(|e| !e.subsamples.is_empty()));

    let plain = if !has_subsamples {
        // Sin subsamples (incluye los cbc2 legacy, que ni traen senc): cada sample
        // es una cadena AES-CBC independiente, así que va en su propia llamada.
        let mut out = Vec::with_capacity(mdat_payload.len());
        let mut off = 0usize;
        for (i, &sz) in sizes.iter().enumerate() {
            let end = (off + sz as usize).min(mdat_payload.len());
            let chunk = &mdat_payload[off.min(end)..end];
            let dec_chunk = dec.decrypt(chunk)?;
            if i == 0 && !validate_sample(&dec_chunk, codec) {
                return Err(Error::DecryptionCorrupted(format!(
                    "cabecera {codec} inválida en el primer sample (0x{:02x})",
                    dec_chunk.first().copied().unwrap_or(0)
                )));
            }
            out.extend_from_slice(&dec_chunk);
            off = end;
        }
        // Cola que no cubre ningún sample: se copia tal cual.
        if off < mdat_payload.len() {
            out.extend_from_slice(&mdat_payload[off..]);
        }
        out
    } else {
        // Con subsamples: dentro de cada sample solo el tramo cifrado va al
        // wrapper, y cada tramo es su propia cadena CBC.
        let senc = senc.unwrap_or_default();
        let mut buf = mdat_payload.to_vec();
        let mut starts = Vec::with_capacity(sizes.len());
        let mut off = 0usize;
        for &sz in &sizes {
            starts.push(off);
            off += sz as usize;
        }

        let mut checked = false;
        for (i, &sample_start) in starts.iter().enumerate() {
            let Some(enc) = senc.get(i) else { continue };
            if enc.subsamples.is_empty() {
                continue;
            }
            let mut pos = sample_start;
            for ss in &enc.subsamples {
                pos += ss.clear as usize;
                let n = ss.cipher as usize;
                if n == 0 {
                    continue;
                }
                let end = (pos + n).min(buf.len());
                if pos >= end {
                    break;
                }
                let dec_chunk = dec.decrypt(&buf[pos..end])?;
                if !checked {
                    checked = true;
                    // Si el tramo cifrado empieza en el byte 0 del sample, la
                    // firma del códec sale del descifrado; si hay prefijo en
                    // claro, la firma está en ese prefijo y ya es legible.
                    let probe: &[u8] = if pos == sample_start {
                        &dec_chunk
                    } else {
                        &buf[sample_start..end.min(buf.len())]
                    };
                    if !validate_sample(probe, codec) {
                        return Err(Error::DecryptionCorrupted(format!(
                            "falta la firma de {codec} (0x{})",
                            hex::encode(&probe[..probe.len().min(2)])
                        )));
                    }
                }
                buf[pos..end].copy_from_slice(&dec_chunk[..end - pos]);
                pos = end;
            }
        }
        buf
    };

    let delta = -removed_bytes(traf);
    let clean = clean_traf(traf, delta);

    let mut moof_payload_out = Vec::with_capacity(moof_payload.len());
    for b in boxes(moof_payload) {
        if b.is(b"pssh") {
            continue;
        }
        if b.is(b"traf") {
            mk_into(&mut moof_payload_out, b"traf", &clean);
        } else {
            mk_into(&mut moof_payload_out, &b.kind, b.payload);
        }
    }

    Ok(Fragment {
        moof: super::mk(b"moof", &moof_payload_out),
        mdat: plain,
        sample_sizes: sizes,
        sample_durations: durations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4::mk;

    /// Descifrador de mentira: invierte los bits. Sirve para comprobar que los
    /// tramos correctos son los que pasan por él.
    struct Flip {
        pub calls: Vec<usize>,
    }
    impl Decryptor for Flip {
        fn decrypt(&mut self, data: &[u8]) -> Result<Vec<u8>> {
            self.calls.push(data.len());
            Ok(data.iter().map(|b| !b).collect())
        }
    }

    fn trun(sizes: &[u32], data_offset: i32) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0x0000_0201u32.to_be_bytes()); // flags: data_offset + sample_size
        p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
        p.extend_from_slice(&data_offset.to_be_bytes());
        for s in sizes {
            p.extend_from_slice(&s.to_be_bytes());
        }
        mk(b"trun", &p)
    }

    #[test]
    fn sin_subsamples_cada_sample_va_por_separado() {
        let mut traf_payload = trun(&[4, 4], 100);
        traf_payload.extend_from_slice(&mk(b"senc", &[0u8; 4]));
        let traf = mk(b"traf", &traf_payload);
        let moof = mk(b"moof", &traf);
        let mdat = vec![0u8; 8];

        let mut tenc = crate::mp4::init::TencInfo::fallback();
        tenc.codec = "mp4a".into(); // sin validación de firma
        let mut d = Flip { calls: vec![] };
        let out = decrypt_fragment(&moof, &mdat, &tenc, &mut d, 0).unwrap();

        assert_eq!(d.calls, vec![4, 4], "un sample por llamada");
        assert_eq!(out.mdat, vec![0xFFu8; 8]);
        assert_eq!(out.sample_sizes, vec![4, 4]);
    }

    #[test]
    fn el_traf_pierde_el_cifrado_y_el_trun_se_recoloca() {
        let senc = mk(b"senc", &[0u8; 4]);
        let mut traf_payload = trun(&[4], 100);
        traf_payload.extend_from_slice(&senc);
        let traf = mk(b"traf", &traf_payload);
        let moof = mk(b"moof", &traf);

        let mut tenc = crate::mp4::init::TencInfo::fallback();
        tenc.codec = "mp4a".into();
        let mut d = Flip { calls: vec![] };
        let out = decrypt_fragment(&moof, &[0u8; 4], &tenc, &mut d, 0).unwrap();

        let traf_out = crate::mp4::find(&out.moof[8..], &[b"traf"]).unwrap();
        assert!(boxes(traf_out).all(|b| !b.is(b"senc")), "el senc debe salir");
        let trun_out = boxes(traf_out).find(|b| b.is(b"trun")).unwrap();
        let info = parse_trun(trun_out.payload);
        assert_eq!(info.data_offset, Some(100 - (senc.len() as i32)));
    }

    /// Regresión: el catálogo de Apple manda ALAC con `iv_size = 0` (IV
    /// constante en el tenc, no por sample). Leerlo como si fuera 16 descoloca
    /// todo el senc y el archivo sale medio descifrado — pasa el ffprobe y no
    /// decodifica. Verificado contra un track real: con el fix el mdat sale
    /// byte a byte igual al del downloader original.
    #[test]
    fn un_senc_sin_iv_por_sample_se_lee_bien() {
        let mut p = Vec::new();
        p.extend_from_slice(&0x0000_0002u32.to_be_bytes()); // flags: hay subsamples
        p.extend_from_slice(&2u32.to_be_bytes());           // 2 samples
        for cipher in [32u32, 48] {
            p.extend_from_slice(&1u16.to_be_bytes());       // 1 subsample
            p.extend_from_slice(&0u16.to_be_bytes());       // clear = 0
            p.extend_from_slice(&cipher.to_be_bytes());
        }
        let parsed = parse_senc(&p, 0);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].iv.is_empty(), "sin IV por sample");
        assert_eq!(parsed[0].subsamples[0].cipher, 32);
        assert_eq!(parsed[1].subsamples[0].cipher, 48);

        // Y leerlo como si el IV midiera 16 da cualquier cosa: por eso el guard.
        let mal = parse_senc(&p, 16);
        assert!(mal.is_empty() || mal[0].subsamples.first().map(|s| s.cipher) != Some(32));
    }

    #[test]
    fn una_sesion_muerta_se_detecta_en_el_primer_sample() {
        let traf = mk(b"traf", &trun(&[4], 100));
        let moof = mk(b"moof", &traf);
        let mut tenc = crate::mp4::init::TencInfo::fallback();
        tenc.codec = "alac".into();
        // Flip convierte 0x00 en 0xFF, que NO es un elemento ALAC válido.
        let mut d = Flip { calls: vec![] };
        let err = decrypt_fragment(&moof, &[0u8; 4], &tenc, &mut d, 0).unwrap_err();
        assert!(matches!(err, Error::DecryptionCorrupted(_)), "debe cazarse como corrupción");
    }
}
