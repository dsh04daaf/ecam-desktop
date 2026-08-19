//! Armado del MP4 final.
//!
//! El original en Python hacía tres pasadas sobre el archivo entero: escribía un
//! fMP4 descifrado, lo releía completo para arreglar la duración y lo volvía a
//! releer para desfragmentarlo — cada pasada con el archivo en RAM. Un mix de
//! una hora llegaba a pedir ~4 GB y tumbaba el servidor.
//!
//! Aquí se hace en **una sola pasada**: mientras se descifra se van apuntando los
//! tamaños y duraciones de cada sample, y al final se escribe directamente el MP4
//! no fragmentado. El resultado es el mismo archivo que producían las tres
//! pasadas (mismas tablas, misma marca de `ftyp`), pero la memoria no depende de
//! lo que dure el track.

use super::{be_u32, boxes, find, mk, mk_into, rebuild};
use crate::error::Result;
use std::io::{Read, Write};

/// Tablas que se van llenando fragmento a fragmento.
#[derive(Debug, Default)]
pub struct SampleTables {
    /// Un "chunk" por `moof`, igual que hacía la desfragmentación original.
    chunk_sample_counts: Vec<u32>,
    sample_sizes: Vec<u32>,
    /// `stts` comprimido: (número de samples, duración). Se agrupa al vuelo.
    stts: Vec<(u32, u32)>,
    total_media_duration: u64,
    total_bytes: u64,
}

impl SampleTables {
    pub fn push_fragment(&mut self, sizes: &[u32], durations: &[u32]) {
        if sizes.is_empty() {
            return;
        }
        self.chunk_sample_counts.push(sizes.len() as u32);
        for &s in sizes {
            self.sample_sizes.push(s);
            self.total_bytes += s as u64;
        }
        for &d in durations {
            self.total_media_duration += d as u64;
            match self.stts.last_mut() {
                Some((count, delta)) if *delta == d => *count += 1,
                _ => self.stts.push((1, d)),
            }
        }
    }

    pub fn sample_count(&self) -> usize {
        self.sample_sizes.len()
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    pub fn is_empty(&self) -> bool {
        self.sample_sizes.is_empty()
    }
    pub fn duration_seconds(&self, media_timescale: u32) -> f64 {
        if media_timescale == 0 {
            return 0.0;
        }
        self.total_media_duration as f64 / media_timescale as f64
    }
}

fn build_stts(entries: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + entries.len() * 8);
    p.extend_from_slice(&0u32.to_be_bytes()); // versión + flags
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (count, delta) in entries {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&delta.to_be_bytes());
    }
    p
}

fn build_stsz(sizes: &[u32]) -> Vec<u8> {
    let mut p = Vec::with_capacity(12 + sizes.len() * 4);
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0 → tabla por sample
    p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    for s in sizes {
        p.extend_from_slice(&s.to_be_bytes());
    }
    p
}

/// `stsc` en forma compacta: solo se emite una entrada cuando cambia el número
/// de samples por chunk.
fn build_stsc(chunk_counts: &[u32]) -> Vec<u8> {
    let mut entries: Vec<(u32, u32)> = Vec::new(); // (primer chunk, samples)
    for (i, &n) in chunk_counts.iter().enumerate() {
        if entries.last().map(|(_, prev)| *prev) != Some(n) {
            entries.push((i as u32 + 1, n));
        }
    }
    let mut p = Vec::with_capacity(8 + entries.len() * 12);
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (first, n) in entries {
        p.extend_from_slice(&first.to_be_bytes());
        p.extend_from_slice(&n.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes()); // sample_description_index
    }
    p
}

fn build_stco(offsets: &[u64], wide: bool) -> (Vec<u8>, &'static [u8; 4]) {
    let mut p = Vec::with_capacity(8 + offsets.len() * if wide { 8 } else { 4 });
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for o in offsets {
        if wide {
            p.extend_from_slice(&o.to_be_bytes());
        } else {
            p.extend_from_slice(&(*o as u32).to_be_bytes());
        }
    }
    (p, if wide { b"co64" } else { b"stco" })
}

fn patch_duration(payload: &[u8], dur: u64, v1_off: usize, v0_off: usize) -> Vec<u8> {
    let mut out = payload.to_vec();
    if out.is_empty() {
        return out;
    }
    if out[0] == 1 {
        if out.len() >= v1_off + 8 {
            out[v1_off..v1_off + 8].copy_from_slice(&dur.to_be_bytes());
        }
    } else if out.len() >= v0_off + 4 {
        out[v0_off..v0_off + 4].copy_from_slice(&(dur as u32).to_be_bytes());
    }
    out
}

/// `elst` con `segment_duration = 0` y `media_time > 0`: es el caso de Apple.
/// Si se deja en cero, algunos reproductores muestran el track como vacío.
fn patch_elst(payload: &[u8], total_media: u64, movie_ts: u32, media_ts: u32) -> Vec<u8> {
    if payload.len() < 12 || media_ts == 0 || be_u32(payload, 4) < 1 {
        return payload.to_vec();
    }
    let mut out = payload.to_vec();
    let v = out[0];
    let off = 8;
    let (seg_dur, media_time): (u64, i64) = if v == 1 {
        if out.len() < off + 16 {
            return out;
        }
        (
            u64::from_be_bytes(out[off..off + 8].try_into().unwrap()),
            i64::from_be_bytes(out[off + 8..off + 16].try_into().unwrap()),
        )
    } else {
        if out.len() < off + 8 {
            return out;
        }
        (
            be_u32(&out, off) as u64,
            i32::from_be_bytes(out[off + 4..off + 8].try_into().unwrap()) as i64,
        )
    };

    if seg_dur == 0 && media_time > 0 {
        let correct = (total_media.saturating_sub(media_time as u64)) as u128 * movie_ts as u128
            / media_ts as u128;
        if correct > 0 {
            let correct = correct as u64;
            if v == 1 {
                out[off..off + 8].copy_from_slice(&correct.to_be_bytes());
            } else {
                out[off..off + 4].copy_from_slice(&(correct as u32).to_be_bytes());
            }
        }
    }
    out
}

/// La marca del `ftyp` según el códec. No es cosmético: hay decoders que se
/// guían por ella para decidir si el archivo es audio de iTunes o un MP4 genérico.
fn ftyp_for(codec: &str) -> Vec<u8> {
    match codec {
        "alac" | "mp4a" => mk(b"ftyp", b"M4A \x00\x00\x00\x00M4A mp42isom"),
        "ec-3" | "ac-3" => mk(b"ftyp", b"mp42\x00\x00\x00\x00mp42isom"),
        _ => mk(b"ftyp", b"M4A \x00\x00\x00\x00M4A mp42isom"),
    }
}

/// Reconstruye el `moov` no fragmentado con las tablas nuevas.
fn build_moov(
    moov_payload: &[u8],
    tables: &SampleTables,
    timing: super::init::MoovTiming,
    stco_payload: &[u8],
    stco_kind: &[u8; 4],
) -> Vec<u8> {
    let media_ts = timing.media_timescale.max(1);
    let movie_ts = timing.movie_timescale.max(1);
    let total_media = tables.total_media_duration;
    let movie_dur = (total_media as u128 * movie_ts as u128 / media_ts as u128) as u64;

    let new_stts = build_stts(&tables.stts);
    let new_stsz = build_stsz(&tables.sample_sizes);
    let new_stsc = build_stsc(&tables.chunk_sample_counts);

    rebuild(moov_payload, |kind, payload| match &kind {
        // Ya no es fragmentado: el mvex sobra y confunde a los reproductores.
        b"mvex" => None,
        b"mvhd" => Some(patch_duration(payload, movie_dur, 24, 16)),
        b"trak" => Some(rebuild(payload, |k2, p2| match &k2 {
            b"tkhd" => Some(patch_duration(p2, movie_dur, 28, 20)),
            b"edts" => Some(rebuild(p2, |k3, p3| {
                if &k3 == b"elst" {
                    Some(patch_elst(p3, total_media, movie_ts, media_ts))
                } else {
                    Some(p3.to_vec())
                }
            })),
            b"mdia" => Some(rebuild(p2, |k3, p3| match &k3 {
                b"mdhd" => Some(patch_duration(p3, total_media, 24, 16)),
                b"minf" => Some(rebuild(p3, |k4, p4| {
                    if &k4 != b"stbl" {
                        return Some(p4.to_vec());
                    }
                    let mut stbl = Vec::with_capacity(p4.len());
                    for b in boxes(p4) {
                        // Las tablas viejas se tiran enteras: las de Apple vienen
                        // vacías o mintiendo, y las nuestras salen de los truns.
                        if matches!(&b.kind, b"stco" | b"co64" | b"stsz" | b"stsc" | b"stts") {
                            continue;
                        }
                        mk_into(&mut stbl, &b.kind, b.payload);
                    }
                    mk_into(&mut stbl, b"stts", &new_stts);
                    mk_into(&mut stbl, stco_kind, stco_payload);
                    mk_into(&mut stbl, b"stsz", &new_stsz);
                    mk_into(&mut stbl, b"stsc", &new_stsc);
                    Some(stbl)
                })),
                _ => Some(p3.to_vec()),
            })),
            _ => Some(p2.to_vec()),
        })),
        _ => Some(payload.to_vec()),
    })
}

/// Escribe el MP4 final: `ftyp` + `moov` + un solo `mdat`.
///
/// `mdat_source` entrega los bytes ya descifrados en orden. No se cargan en
/// memoria: se copian a bloques.
pub fn write_mp4<W: Write, R: Read>(
    out: &mut W,
    clean_init: &[u8],
    tables: &SampleTables,
    timing: super::init::MoovTiming,
    codec: &str,
    mdat_source: &mut R,
) -> Result<u64> {
    let moov_payload = find(clean_init, &[b"moov"]).unwrap_or(&[]).to_vec();
    let ftyp = ftyp_for(codec);

    // Offsets por chunk. Se calculan en dos pasadas porque el tamaño del moov
    // depende de las tablas y los offsets dependen del tamaño del moov.
    let n_chunks = tables.chunk_sample_counts.len();
    // Si el mdat pudiera pasar de 4 GB hay que usar co64 desde la primera pasada,
    // o la segunda cambiaría de tamaño y descolocaría todos los offsets.
    let wide = tables.total_bytes + 1024 * 1024 > u32::MAX as u64;

    let (placeholder, stco_kind) = build_stco(&vec![0u64; n_chunks], wide);
    let moov_p1 = build_moov(&moov_payload, tables, timing, &placeholder, stco_kind);
    let moov_size = moov_p1.len() + 8;

    let mdat_start = ftyp.len() as u64 + moov_size as u64 + 8;
    let mut offsets = Vec::with_capacity(n_chunks);
    let mut running = mdat_start;
    let mut idx = 0usize;
    for &count in &tables.chunk_sample_counts {
        offsets.push(running);
        for _ in 0..count {
            running += tables.sample_sizes[idx] as u64;
            idx += 1;
        }
    }

    let (real_stco, _) = build_stco(&offsets, wide);
    let moov_p2 = build_moov(&moov_payload, tables, timing, &real_stco, stco_kind);
    debug_assert_eq!(
        moov_p1.len(),
        moov_p2.len(),
        "el moov cambió de tamaño entre pasadas: los offsets quedarían corridos"
    );

    out.write_all(&ftyp)?;
    out.write_all(&mk(b"moov", &moov_p2))?;

    // mdat: cabecera + los bytes descifrados tal cual.
    let mdat_len = tables.total_bytes;
    if mdat_len + 8 > u32::MAX as u64 {
        // mdat de 64 bits: size = 1 y el tamaño real detrás del tipo.
        out.write_all(&1u32.to_be_bytes())?;
        out.write_all(b"mdat")?;
        out.write_all(&(mdat_len + 16).to_be_bytes())?;
    } else {
        out.write_all(&((mdat_len + 8) as u32).to_be_bytes())?;
        out.write_all(b"mdat")?;
    }
    let copied = std::io::copy(mdat_source, out)?;
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4::init::MoovTiming;

    fn tables() -> SampleTables {
        let mut t = SampleTables::default();
        t.push_fragment(&[10, 10, 10], &[512, 512, 512]);
        t.push_fragment(&[10, 20], &[512, 256]);
        t
    }

    #[test]
    fn el_stts_se_comprime_por_duraciones_iguales() {
        let t = tables();
        assert_eq!(t.stts, vec![(4, 512), (1, 256)]);
        assert_eq!(t.total_media_duration, 512 * 4 + 256);
        assert_eq!(t.total_bytes, 60);
    }

    #[test]
    fn el_stsc_solo_emite_entrada_cuando_cambia_el_conteo() {
        let payload = build_stsc(&[3, 3, 2, 2, 2]);
        assert_eq!(be_u32(&payload, 4), 2, "dos entradas: 3 por chunk y luego 2");
    }

    #[test]
    fn el_mp4_final_no_es_fragmentado_y_las_tablas_cuadran() {
        // moov mínimo pero completo: mvex (que debe desaparecer) + trak/stbl.
        let stbl = mk(b"stbl", &[mk(b"stsd", b"x"), mk(b"stco", b"viejo")].concat());
        let minf = mk(b"minf", &stbl);
        let mdhd = {
            let mut p = vec![0u8; 24];
            p[12..16].copy_from_slice(&44100u32.to_be_bytes());
            mk(b"mdhd", &p)
        };
        let mdia = mk(b"mdia", &[mdhd, minf].concat());
        let trak = mk(b"trak", &mdia);
        let mvhd = {
            let mut p = vec![0u8; 100];
            p[12..16].copy_from_slice(&1000u32.to_be_bytes());
            mk(b"mvhd", &p)
        };
        let mvex = mk(b"mvex", b"trex-aqui");
        let moov = mk(b"moov", &[mvhd, trak, mvex].concat());
        let init = [mk(b"ftyp", b"isom0000"), moov].concat();

        let t = tables();
        let timing = MoovTiming { movie_timescale: 1000, media_timescale: 44100, trex_default_duration: 0 };
        let mut data = std::io::Cursor::new(vec![7u8; 60]);
        let mut out = Vec::new();
        write_mp4(&mut out, &init, &t, timing, "alac", &mut data).unwrap();

        let kinds: Vec<_> = boxes(&out).map(|b| b.kind).collect();
        assert_eq!(kinds, vec![*b"ftyp", *b"moov", *b"mdat"], "solo tres cajas, sin moof");

        let moov_out = find(&out, &[b"moov"]).unwrap();
        assert!(boxes(moov_out).all(|b| !b.is(b"mvex")), "el mvex debe irse");

        let stbl_out = find(moov_out, &[b"trak", b"mdia", b"minf", b"stbl"]).unwrap();
        let stsz = find(stbl_out, &[b"stsz"]).unwrap();
        assert_eq!(be_u32(stsz, 8), 5, "cinco samples");
        let stco = find(stbl_out, &[b"stco"]).unwrap();
        assert_eq!(be_u32(stco, 4), 2, "dos chunks");

        // El primer chunk tiene que apuntar justo detrás de la cabecera del mdat.
        let mdat = boxes(&out).find(|b| b.is(b"mdat")).unwrap();
        assert_eq!(be_u32(stco, 8) as usize, mdat.start + 8);

        // Y la marca del ftyp es la de audio de iTunes, no la genérica.
        let ftyp = boxes(&out).next().unwrap();
        assert_eq!(&ftyp.payload[..4], b"M4A ");
    }
}
