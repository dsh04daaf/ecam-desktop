//! Descifrado cbcs (ISO 23001-7) para music videos: sustituye a mp4decrypt.
//!
//! Además de descifrar los bytes hay tres cosas que, cuando faltaban, rompían la
//! reproducción de verdad:
//!   * se procesa **cada `traf`** del `moof`, no solo el primero (Apple manda un
//!     track de subtítulos `clcp` junto al vídeo);
//!   * los offsets salen del `data_offset` de cada `trun`;
//!   * las entradas del `stsd` se reescriben (`encv` → `hvc1`, `enca` → `mp4a`) y
//!     se tiran `senc`/`saiz`/`saio`. Si no, el reproductor sigue creyendo que el
//!     archivo está protegido y pinta bloques verdes. ffmpeg resuelve `encv` en
//!     silencio, así que nunca te avisa de que falta este paso.

use crate::error::{Error, Result};
use crate::mp4::{be_u32, boxes, find, full_flags, is_crypto_group, mk, mk_into};
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use std::io::{Read, Seek, SeekFrom, Write};

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// Descifra un rango protegido, en su sitio.
///
/// Con el patrón que usa Apple (1:9) solo se cifra el primer bloque de cada
/// diez, así que apenas un 10% de los bytes pasa por AES. Cada rango reinicia la
/// cadena CBC desde el IV del sample.
pub fn decrypt_range(buf: &mut [u8], key: &[u8; 16], iv: &[u8; 16], crypt_blocks: u8, skip_blocks: u8) {
    if buf.len() < 16 {
        return;
    }
    let mut dec = Aes128CbcDec::new(key.into(), iv.into());

    if skip_blocks == 0 {
        // Sin patrón: todo el rango es una sola cadena.
        let full = buf.len() - (buf.len() % 16);
        for block in buf[..full].chunks_mut(16) {
            dec.decrypt_block_mut(block.into());
        }
        return;
    }

    let unit = (crypt_blocks as usize + skip_blocks as usize) * 16;
    let crypt_len = crypt_blocks as usize * 16;
    let mut off = 0usize;
    while off + crypt_len <= buf.len() {
        for block in buf[off..off + crypt_len].chunks_mut(16) {
            dec.decrypt_block_mut(block.into());
        }
        off += unit;
    }
}

/// Reescribe las entradas cifradas del `stsd` como su códec real.
///
/// El `stsd` de audio del camino normal no sirve aquí: lo que cambia es el largo
/// de la cabecera fija antes de las cajas hijas — 78 bytes en vídeo
/// (VisualSampleEntry) contra 28 en audio (AudioSampleEntry).
pub fn clean_stsd(payload: &[u8]) -> Vec<u8> {
    if payload.len() < 8 {
        return payload.to_vec();
    }
    let version = payload[0];
    let flags = &payload[1..4];
    let count = be_u32(payload, 4);

    let mut entries = Vec::new();
    for b in boxes(&payload[8..]) {
        let is_enc = matches!(&b.kind, b"encv" | b"enca");
        if !is_enc {
            mk_into(&mut entries, &b.kind, b.payload);
            continue;
        }
        let head_len = if &b.kind == b"encv" { 78 } else { 28 };
        if b.payload.len() < head_len {
            mk_into(&mut entries, &b.kind, b.payload);
            continue;
        }
        let (header, children) = b.payload.split_at(head_len);

        let mut real_codec: Option<[u8; 4]> = None;
        let mut rest = Vec::new();
        for c in boxes(children) {
            if c.is(b"sinf") {
                if let Some(frma) = find(c.payload, &[b"frma"]) {
                    if frma.len() >= 4 {
                        real_codec = frma[..4].try_into().ok();
                    }
                }
                continue; // la caja de protección se va entera
            }
            mk_into(&mut rest, &c.kind, c.payload);
        }

        match real_codec {
            Some(codec) => {
                let mut entry = header.to_vec();
                entry.extend_from_slice(&rest);
                mk_into(&mut entries, &codec, &entry);
            }
            // Sin `frma` no se sabe qué era: mejor dejarlo como estaba que
            // inventarse un códec.
            None => mk_into(&mut entries, &b.kind, b.payload),
        }
    }

    if entries.is_empty() {
        return payload.to_vec();
    }
    let mut out = Vec::with_capacity(entries.len() + 8);
    out.push(version);
    out.extend_from_slice(flags);
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&entries);
    out
}

/// Quita del `moov` todo rastro de cifrado.
pub fn clean_moov(moov: &[u8]) -> Vec<u8> {
    fn walk(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(payload.len());
        for b in boxes(payload) {
            match &b.kind {
                b"pssh" => continue,
                b"stbl" => {
                    let mut stbl = Vec::new();
                    for c in boxes(b.payload) {
                        if c.is(b"stsd") {
                            mk_into(&mut stbl, b"stsd", &clean_stsd(c.payload));
                        } else if is_crypto_group(c.kind, c.payload) {
                            continue;
                        } else {
                            mk_into(&mut stbl, &c.kind, c.payload);
                        }
                    }
                    mk_into(&mut out, b"stbl", &stbl);
                }
                b"trak" | b"mdia" | b"minf" => {
                    let inner = walk(b.payload);
                    mk_into(&mut out, &b.kind, &inner);
                }
                _ => mk_into(&mut out, &b.kind, b.payload),
            }
        }
        out
    }
    walk(moov)
}

fn strip_traf_crypto(traf: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(traf.len());
    for b in boxes(traf) {
        if matches!(&b.kind, b"senc" | b"saiz" | b"saio") || is_crypto_group(b.kind, b.payload) {
            continue;
        }
        mk_into(&mut out, &b.kind, b.payload);
    }
    out
}

/// Suma `delta` al `data_offset` de todos los `trun` (van medidos desde el
/// principio del `moof`, que acaba de encoger).
fn shift_trun_offsets(moof_payload: &[u8], delta: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(moof_payload.len());
    for b in boxes(moof_payload) {
        if !b.is(b"traf") {
            mk_into(&mut out, &b.kind, b.payload);
            continue;
        }
        let mut traf = Vec::with_capacity(b.payload.len());
        for c in boxes(b.payload) {
            if c.is(b"trun") && full_flags(c.payload) & 0x1 != 0 && c.payload.len() >= 12 {
                let cur = be_u32(c.payload, 8) as i32 as i64;
                let mut p = c.payload.to_vec();
                p[8..12].copy_from_slice(&((cur + delta) as i32).to_be_bytes());
                mk_into(&mut traf, b"trun", &p);
            } else {
                mk_into(&mut traf, &c.kind, c.payload);
            }
        }
        mk_into(&mut out, b"traf", &traf);
    }
    out
}

/// Posición absoluta y tamaño de cada sample de un `traf`, sacados de sus `trun`.
fn trun_layout(traf: &[u8], moof_start: u64) -> Vec<(u64, u32)> {
    let mut base = moof_start;
    let mut default_size = 0u32;
    if let Some(tfhd) = find(traf, &[b"tfhd"]) {
        let flags = full_flags(tfhd);
        let mut p = 8usize;
        if flags & 0x1 != 0 {
            base = crate::mp4::be_u64(tfhd, p);
            p += 8;
        }
        if flags & 0x2 != 0 {
            p += 4;
        }
        if flags & 0x8 != 0 {
            p += 4;
        }
        if flags & 0x10 != 0 {
            default_size = be_u32(tfhd, p);
        }
    }

    let mut out = Vec::new();
    for trun in boxes(traf).filter(|b| b.is(b"trun")) {
        let tr = trun.payload;
        let flags = full_flags(tr);
        let count = be_u32(tr, 4) as usize;
        let mut q = 8usize;
        let mut data_offset = 0i64;
        if flags & 0x1 != 0 {
            data_offset = be_u32(tr, q) as i32 as i64;
            q += 4;
        }
        if flags & 0x4 != 0 {
            q += 4;
        }
        let mut cur = (base as i64 + data_offset).max(0) as u64;
        for _ in 0..count {
            let mut size = default_size;
            if flags & 0x100 != 0 {
                q += 4;
            }
            if flags & 0x200 != 0 {
                size = be_u32(tr, q);
                q += 4;
            }
            if flags & 0x400 != 0 {
                q += 4;
            }
            if flags & 0x800 != 0 {
                q += 4;
            }
            out.push((cur, size));
            cur += size as u64;
        }
    }
    out
}

/// Descifra un fMP4 entero, fragmento a fragmento. La memoria que gasta es la de
/// un `mdat`, no la del archivo: un vídeo de cuatro horas cuesta lo mismo que uno
/// de tres minutos.
pub fn decrypt_file<R: Read + Seek, W: Write>(src: &mut R, out: &mut W, key_hex: &str) -> Result<()> {
    let key_bytes = hex::decode(key_hex)
        .map_err(|_| Error::Other("la llave de contenido no es hexadecimal".into()))?;
    let key: [u8; 16] = key_bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::Other("la llave de contenido no mide 16 bytes".into()))?;

    // El tenc del init manda: patrón e IV constante.
    let mut head = vec![0u8; 65536];
    src.seek(SeekFrom::Start(0))?;
    let n = src.read(&mut head)?;
    head.truncate(n);
    let (crypt, skip, iv_size, const_iv) = read_tenc(&head)?;

    src.seek(SeekFrom::Start(0))?;
    let mut pending: Vec<(Vec<crate::mp4::frag::SampleEnc>, Vec<(u64, u32)>)> = Vec::new();

    loop {
        let box_start = src.stream_position()?;
        let mut hdr = [0u8; 8];
        if src.read_exact(&mut hdr).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = hdr[4..8].try_into().unwrap();

        if size32 == 1 {
            // Caja de 64 bits: se copia tal cual (en la práctica, un mdat enorme).
            let mut ext = [0u8; 8];
            src.read_exact(&mut ext)?;
            let size = u64::from_be_bytes(ext);
            out.write_all(&hdr)?;
            out.write_all(&ext)?;
            let mut rest = src.take(size - 16);
            std::io::copy(&mut rest, out)?;
            continue;
        }
        if size32 < 8 {
            break;
        }
        let mut body = vec![0u8; (size32 - 8) as usize];
        src.read_exact(&mut body)?;

        match &kind {
            b"moov" => {
                let clean = clean_moov(&body);
                out.write_all(&mk(b"moov", &clean))?;
            }
            b"moof" => {
                pending.clear();
                let mut rebuilt = Vec::with_capacity(body.len());
                for b in boxes(&body) {
                    if !b.is(b"traf") {
                        mk_into(&mut rebuilt, &b.kind, b.payload);
                        continue;
                    }
                    if let Some(senc) = senc_of(b.payload, iv_size) {
                        pending.push((senc, trun_layout(b.payload, box_start)));
                    }
                    mk_into(&mut rebuilt, b"traf", &strip_traf_crypto(b.payload));
                }
                // Al quitar senc/saiz/saio el moof encoge, así que todos los
                // data_offset tienen que moverse lo mismo o la tabla apunta
                // detrás de los datos.
                let delta = rebuilt.len() as i64 - body.len() as i64;
                if delta != 0 {
                    rebuilt = shift_trun_offsets(&rebuilt, delta);
                }
                out.write_all(&mk(b"moof", &rebuilt))?;
            }
            b"mdat" if !pending.is_empty() => {
                let mdat_start = box_start + 8;
                for (senc, layout) in &pending {
                    for (i, (abs_off, size)) in layout.iter().enumerate() {
                        let Some(info) = senc.get(i) else { break };
                        let iv: [u8; 16] = match if info.iv.is_empty() { &const_iv[..] } else { &info.iv[..] } {
                            b if b.len() == 16 => b.try_into().unwrap(),
                            // IV de 8 bytes: se rellena a la derecha, como manda cbcs.
                            b if b.len() == 8 => {
                                let mut full = [0u8; 16];
                                full[..8].copy_from_slice(b);
                                full
                            }
                            _ => continue,
                        };
                        let Some(off) = abs_off.checked_sub(mdat_start) else { continue };
                        let (off, size) = (off as usize, *size as usize);
                        if off + size > body.len() {
                            continue;
                        }
                        let sample = &mut body[off..off + size];
                        if info.subsamples.is_empty() {
                            decrypt_range(sample, &key, &iv, crypt, skip);
                        } else {
                            let mut p = 0usize;
                            for ss in &info.subsamples {
                                p += ss.clear as usize;
                                let n = ss.cipher as usize;
                                if n > 0 && p + n <= sample.len() {
                                    decrypt_range(&mut sample[p..p + n], &key, &iv, crypt, skip);
                                    p += n;
                                }
                            }
                        }
                    }
                }
                out.write_all(&hdr)?;
                out.write_all(&body)?;
                pending.clear();
            }
            _ => {
                out.write_all(&hdr)?;
                out.write_all(&body)?;
            }
        }
    }
    Ok(())
}

fn senc_of(traf: &[u8], iv_size: usize) -> Option<Vec<crate::mp4::frag::SampleEnc>> {
    for b in boxes(traf) {
        if b.is(b"senc") {
            // Con IV constante el senc no trae IV por sample: se lee con tamaño 0
            // y cada sample usa el del tenc.
            return Some(crate::mp4::frag::parse_senc(b.payload, if iv_size == 0 { 0 } else { iv_size }));
        }
    }
    None
}

/// Devuelve (crypt_byte_block, skip_byte_block, iv_size, IV constante).
fn read_tenc(head: &[u8]) -> Result<(u8, u8, usize, Vec<u8>)> {
    let pos = head
        .windows(4)
        .position(|w| w == b"tenc")
        .ok_or_else(|| Error::Mp4("no hay tenc: el stream no está cifrado como se esperaba".into()))?;
    let payload = &head[pos + 4..];
    let t = crate::mp4::init::parse_tenc(payload);
    Ok((t.crypt_byte_block, t.skip_byte_block, t.iv_size as usize, t.const_iv))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_stsd_de_video_pierde_la_proteccion() {
        let frma = mk(b"frma", b"hvc1");
        let sinf = mk(b"sinf", &frma);
        let mut encv_payload = vec![0u8; 78]; // VisualSampleEntry
        encv_payload.extend_from_slice(&mk(b"hvcC", b"config"));
        encv_payload.extend_from_slice(&sinf);
        let encv = mk(b"encv", &encv_payload);

        let mut stsd = vec![0u8; 4];
        stsd.extend_from_slice(&1u32.to_be_bytes());
        stsd.extend_from_slice(&encv);

        let clean = clean_stsd(&stsd);
        let entry = boxes(&clean[8..]).next().unwrap();
        assert_eq!(&entry.kind, b"hvc1", "encv debe pasar a ser el códec real");
        // Ojo: los primeros 78 bytes de la entrada son la cabecera fija de
        // VisualSampleEntry, NO cajas — hay que saltarlos para mirar los hijos.
        let children = &entry.payload[78..];
        assert!(find(children, &[b"sinf"]).is_none(), "la protección se va");
        assert!(find(children, &[b"hvcC"]).is_some(), "la config del códec se queda");
    }

    #[test]
    fn el_patron_cbcs_solo_toca_un_bloque_de_cada_diez() {
        let key = [0u8; 16];
        let iv = [0u8; 16];
        let mut buf = vec![0xAAu8; 160]; // 10 bloques
        let original = buf.clone();
        decrypt_range(&mut buf, &key, &iv, 1, 9);
        assert_ne!(buf[..16], original[..16], "el primer bloque sí se descifra");
        assert_eq!(buf[16..], original[16..], "los otros nueve se quedan igual");
    }
}
