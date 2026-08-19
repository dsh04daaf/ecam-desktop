//! Lectura de fMP4 y muxer propio: sustituye a MP4Box.
//!
//! Dos cosas que costaron sangre en el original y aquí están desde el principio:
//!   * Apple manda **más de un track**: junto al vídeo viaja un `clcp` de
//!     subtítulos EIA-608, y la mitad de los fragmentos traen dos `traf`. Si se
//!     lee solo el primero y se le pegan todos los samples, los subtítulos
//!     acaban dentro del vídeo y el decoder escupe "Invalid NAL unit size".
//!   * los offsets de cada sample salen del `data_offset` de **su** `trun`, no de
//!     ir sumando tamaños: un traf de vídeo puede traer cuatro truns.
//!
//! La memoria está acotada a propósito: los samples se copian leyendo del
//! archivo por rangos, nunca cargándolo entero.

use crate::error::{Error, Result};
use crate::mp4::{be_u16, be_u32, be_u64, boxes, full_flags, mk};
use std::io::{Read, Seek, SeekFrom, Write};

pub const MOVIE_TIMESCALE: u32 = 1000;
/// Matriz de transformación identidad (la que pone todo el mundo).
const MATRIX: [u8; 36] = [
    0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0, 0, 0,
];

#[derive(Debug, Clone, Copy)]
pub struct MvSample {
    pub offset: u64,
    pub size: u32,
    pub duration: u32,
    pub cts: i32,
    pub is_sync: bool,
}

#[derive(Debug, Clone)]
pub struct MvTrack {
    pub kind: [u8; 4], // vide | soun | clcp
    pub timescale: u32,
    pub stsd: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub language: u16,
    pub samples: Vec<MvSample>,
}

impl MvTrack {
    pub fn duration(&self) -> u64 {
        self.samples.iter().map(|s| s.duration as u64).sum()
    }
}

fn fullbox(kind: &[u8; 4], version: u8, flags: u32, parts: &[&[u8]]) -> Vec<u8> {
    let mut p = Vec::new();
    p.push(version);
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    for part in parts {
        p.extend_from_slice(part);
    }
    mk(kind, &p)
}

fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

// ── tablas ────────────────────────────────────────────────────────────────

fn tbl_stts(samples: &[MvSample]) -> Vec<u8> {
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for s in samples {
        match entries.last_mut() {
            Some((c, d)) if *d == s.duration => *c += 1,
            _ => entries.push((1, s.duration)),
        }
    }
    let mut p = vec![0u8; 4];
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (c, d) in entries {
        p.extend_from_slice(&c.to_be_bytes());
        p.extend_from_slice(&d.to_be_bytes());
    }
    mk(b"stts", &p)
}

/// `ctts` solo se emite si de verdad hay offsets de composición: un ctts todo a
/// cero en un stream sin B-frames confunde a algunos reproductores.
fn tbl_ctts(samples: &[MvSample]) -> Option<Vec<u8>> {
    if samples.iter().all(|s| s.cts == 0) {
        return None;
    }
    let mut entries: Vec<(u32, i32)> = Vec::new();
    for s in samples {
        match entries.last_mut() {
            Some((c, off)) if *off == s.cts => *c += 1,
            _ => entries.push((1, s.cts)),
        }
    }
    let mut p = vec![1u8, 0, 0, 0]; // versión 1: offsets con signo
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (c, off) in entries {
        p.extend_from_slice(&c.to_be_bytes());
        p.extend_from_slice(&off.to_be_bytes());
    }
    Some(mk(b"ctts", &p))
}

fn tbl_stsz(samples: &[MvSample]) -> Vec<u8> {
    let mut p = vec![0u8; 4];
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    for s in samples {
        p.extend_from_slice(&s.size.to_be_bytes());
    }
    mk(b"stsz", &p)
}

fn tbl_stsc(chunk_counts: &[u32]) -> Vec<u8> {
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for (i, &n) in chunk_counts.iter().enumerate() {
        if entries.last().map(|(_, prev)| *prev) != Some(n) {
            entries.push((i as u32 + 1, n));
        }
    }
    let mut p = vec![0u8; 4];
    p.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (first, n) in entries {
        p.extend_from_slice(&first.to_be_bytes());
        p.extend_from_slice(&n.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
    }
    mk(b"stsc", &p)
}

fn tbl_stco(offsets: &[u64], wide: bool) -> Vec<u8> {
    let mut p = vec![0u8; 4];
    p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for o in offsets {
        if wide {
            p.extend_from_slice(&o.to_be_bytes());
        } else {
            p.extend_from_slice(&(*o as u32).to_be_bytes());
        }
    }
    mk(if wide { b"co64" } else { b"stco" }, &p)
}

/// `stss` solo si NO todos los samples son de sincronía; si lo son, la tabla
/// sobra y algunos reproductores se atragantan con ella.
fn tbl_stss(samples: &[MvSample]) -> Option<Vec<u8>> {
    if samples.iter().all(|s| s.is_sync) {
        return None;
    }
    let idx: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_sync)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    let mut p = vec![0u8; 4];
    p.extend_from_slice(&(idx.len() as u32).to_be_bytes());
    for i in idx {
        p.extend_from_slice(&i.to_be_bytes());
    }
    Some(mk(b"stss", &p))
}

fn build_trak(track: &MvTrack, track_id: u32, chunk_counts: &[u32], offsets: &[u64], wide: bool) -> Vec<u8> {
    let mut stbl_parts = vec![track.stsd.clone(), tbl_stts(&track.samples)];
    if let Some(ctts) = tbl_ctts(&track.samples) {
        stbl_parts.push(ctts);
    }
    stbl_parts.push(tbl_stsc(chunk_counts));
    stbl_parts.push(tbl_stsz(&track.samples));
    stbl_parts.push(tbl_stco(offsets, wide));
    if let Some(stss) = tbl_stss(&track.samples) {
        stbl_parts.push(stss);
    }
    let stbl = mk(b"stbl", &concat(&stbl_parts));

    // La cabecera de medios tiene que casar con el handler, incluido el track de
    // subtítulos que Apple mete de propina.
    let header = match &track.kind {
        b"vide" => fullbox(b"vmhd", 0, 1, &[&[0u8; 8]]),
        b"soun" => fullbox(b"smhd", 0, 0, &[&[0u8; 4]]),
        _ => fullbox(b"nmhd", 0, 0, &[]),
    };
    let dref = fullbox(b"dref", 0, 0, &[&1u32.to_be_bytes(), &fullbox(b"url ", 0, 1, &[])]);
    let dinf = mk(b"dinf", &dref);
    let minf = mk(b"minf", &concat(&[header, dinf, stbl]));

    let handler_name: &[u8] = match &track.kind {
        b"vide" => b"VideoHandler\0",
        b"soun" => b"SoundHandler\0",
        _ => b"ClosedCaptionHandler\0",
    };
    let hdlr = fullbox(b"hdlr", 0, 0, &[&0u32.to_be_bytes(), &track.kind, &[0u8; 12], handler_name]);

    let dur = track.duration();
    let mdhd = fullbox(
        b"mdhd",
        0,
        0,
        &[
            &0u32.to_be_bytes(),
            &0u32.to_be_bytes(),
            &track.timescale.to_be_bytes(),
            &(dur as u32).to_be_bytes(),
            &track.language.to_be_bytes(),
            &0u16.to_be_bytes(),
        ],
    );
    let mdia = mk(b"mdia", &concat(&[mdhd, hdlr, minf]));

    let movie_dur = if track.timescale > 0 { dur * MOVIE_TIMESCALE as u64 / track.timescale as u64 } else { 0 };
    let volume: u16 = if &track.kind == b"soun" { 0x0100 } else { 0 };
    let (w, h) = if &track.kind == b"vide" { (track.width << 16, track.height << 16) } else { (0, 0) };
    let tkhd = fullbox(
        b"tkhd",
        0,
        0x000007, // habilitado + en película + en preview
        &[
            &0u32.to_be_bytes(),
            &0u32.to_be_bytes(),
            &track_id.to_be_bytes(),
            &0u32.to_be_bytes(),
            &(movie_dur as u32).to_be_bytes(),
            &[0u8; 8],
            &0u16.to_be_bytes(),
            &0u16.to_be_bytes(),
            &volume.to_be_bytes(),
            &0u16.to_be_bytes(),
            &MATRIX,
            &w.to_be_bytes(),
            &h.to_be_bytes(),
        ],
    );
    mk(b"trak", &concat(&[tkhd, mdia]))
}

/// Agrupa samples en trozos de ~1 s para que los tracks queden intercalados en
/// disco: si no, reproducir por red obliga a saltar de un extremo al otro.
fn chunks_of(track: &MvTrack, seconds: f64) -> Vec<(usize, usize)> {
    let limit = (track.timescale as f64 * seconds) as u64;
    let mut out = Vec::new();
    let (mut start, mut acc) = (0usize, 0u64);
    for (i, s) in track.samples.iter().enumerate() {
        acc += s.duration as u64;
        if acc >= limit {
            out.push((start, i + 1));
            start = i + 1;
            acc = 0;
        }
    }
    if start < track.samples.len() {
        out.push((start, track.samples.len()));
    }
    out
}

/// Une varios tracks fMP4 en un MP4 progresivo (moov delante, listo para
/// reproducir mientras se copia).
pub fn mux<R: Read + Seek, W: Write>(sources: &mut [(MvTrack, R)], out: &mut W) -> Result<()> {
    if sources.is_empty() {
        return Err(Error::Mp4("no hay tracks que unir".into()));
    }

    // Plan de escritura: (tiempo en segundos, índice de track, rango de samples).
    let mut plan: Vec<(f64, usize, usize, usize)> = Vec::new();
    for (idx, (track, _)) in sources.iter().enumerate() {
        let mut t = 0u64;
        for (a, b) in chunks_of(track, 1.0) {
            plan.push((t as f64 / track.timescale.max(1) as f64, idx, a, b));
            t += track.samples[a..b].iter().map(|s| s.duration as u64).sum::<u64>();
        }
    }
    plan.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal).then(x.1.cmp(&y.1)));

    let mut counts: Vec<Vec<u32>> = vec![Vec::new(); sources.len()];
    for (_, idx, a, b) in &plan {
        counts[*idx].push((b - a) as u32);
    }

    let ftyp = mk(b"ftyp", b"isom\x00\x00\x02\x00isomiso2avc1mp41hvc1");
    let total_bytes: u64 = sources
        .iter()
        .flat_map(|(t, _)| t.samples.iter())
        .map(|s| s.size as u64)
        .sum();
    let wide = total_bytes + 1024 * 1024 > u32::MAX as u64;

    let build_moov = |offsets: &[Vec<u64>]| -> Vec<u8> {
        let movie_dur = sources
            .iter()
            .map(|(t, _)| if t.timescale > 0 { t.duration() * MOVIE_TIMESCALE as u64 / t.timescale as u64 } else { 0 })
            .max()
            .unwrap_or(0);
        let mvhd = fullbox(
            b"mvhd",
            0,
            0,
            &[
                &0u32.to_be_bytes(),
                &0u32.to_be_bytes(),
                &MOVIE_TIMESCALE.to_be_bytes(),
                &(movie_dur as u32).to_be_bytes(),
                &0x00010000u32.to_be_bytes(), // velocidad 1.0
                &0x0100u16.to_be_bytes(),     // volumen 1.0
                &[0u8; 10],
                &MATRIX,
                &[0u8; 24],
                &((sources.len() + 1) as u32).to_be_bytes(),
            ],
        );
        let traks: Vec<u8> = sources
            .iter()
            .enumerate()
            .flat_map(|(i, (t, _))| build_trak(t, i as u32 + 1, &counts[i], &offsets[i], wide))
            .collect();
        mk(b"moov", &concat(&[mvhd, traks]))
    };

    // Pasada 1: offsets falsos para medir el moov.
    let provisional: Vec<Vec<u64>> = counts.iter().map(|c| vec![0u64; c.len()]).collect();
    let moov_len = build_moov(&provisional).len();

    let payload_start = ftyp.len() as u64 + moov_len as u64 + 8;
    let mut offsets: Vec<Vec<u64>> = vec![Vec::new(); sources.len()];
    let mut cursor = payload_start;
    for (_, idx, a, b) in &plan {
        offsets[*idx].push(cursor);
        cursor += sources[*idx].0.samples[*a..*b].iter().map(|s| s.size as u64).sum::<u64>();
    }
    let mdat_size = cursor - payload_start;

    let moov = build_moov(&offsets);
    if moov.len() != moov_len {
        return Err(Error::Mp4("el moov cambió de tamaño entre pasadas".into()));
    }

    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    out.write_all(&((mdat_size + 8) as u32).to_be_bytes())?;
    out.write_all(b"mdat")?;

    // Los samples de un trozo NO tienen por qué ser contiguos en el origen: un
    // trozo de ~1 s puede cruzar de un fragmento al siguiente, y en medio está
    // el `moof`. Leer el rango entero de una vez se traga esa cabecera y la
    // mete dentro del vídeo (se ve como "Invalid NAL unit size"). Se copian por
    // tramos contiguos: una lectura por tramo, no una por sample.
    let mut buf = Vec::new();
    for (_, idx, a, b) in &plan {
        let (track, src) = &mut sources[*idx];
        let mut i = *a;
        while i < *b {
            let start = track.samples[i].offset;
            let mut len = track.samples[i].size as u64;
            let mut j = i + 1;
            while j < *b && track.samples[j].offset == start + len {
                len += track.samples[j].size as u64;
                j += 1;
            }
            src.seek(SeekFrom::Start(start))?;
            buf.resize(len as usize, 0);
            src.read_exact(&mut buf)?;
            out.write_all(&buf)?;
            i = j;
        }
    }
    Ok(())
}

/// Lee todos los tracks de un fMP4 ya descifrado, con los offsets absolutos de
/// cada sample dentro del archivo.
pub fn read_tracks<R: Read + Seek>(f: &mut R) -> Result<Vec<MvTrack>> {
    f.seek(SeekFrom::Start(0))?;

    let mut moov: Option<Vec<u8>> = None;
    let mut moofs: Vec<(u64, Vec<u8>)> = Vec::new();

    // Se recorren las cabeceras saltando el mdat: cargarlo sería justo lo que no
    // queremos con un vídeo de 4K.
    loop {
        let start = f.stream_position()?;
        let mut hdr = [0u8; 8];
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = hdr[4..8].try_into().unwrap();
        let (size, header_len) = if size32 == 1 {
            let mut ext = [0u8; 8];
            f.read_exact(&mut ext)?;
            (u64::from_be_bytes(ext), 16u64)
        } else if size32 < 8 {
            break;
        } else {
            (size32, 8u64)
        };

        match &kind {
            b"moov" | b"moof" => {
                let mut payload = vec![0u8; (size - header_len) as usize];
                f.read_exact(&mut payload)?;
                if &kind == b"moov" {
                    moov = Some(payload);
                } else {
                    moofs.push((start, payload));
                }
            }
            _ => {
                f.seek(SeekFrom::Start(start + size))?;
            }
        }
    }

    let moov = moov.ok_or_else(|| Error::Mp4("el fMP4 no trae moov".into()))?;

    // trex: valores por defecto de cada track.
    let mut trex_defaults: std::collections::HashMap<u32, (u32, u32, u32)> = Default::default();
    if let Some(mvex) = crate::mp4::find(&moov, &[b"mvex"]) {
        for b in boxes(mvex) {
            if b.is(b"trex") && b.payload.len() >= 24 {
                trex_defaults.insert(
                    be_u32(b.payload, 4),
                    (be_u32(b.payload, 12), be_u32(b.payload, 16), be_u32(b.payload, 20)),
                );
            }
        }
    }

    let mut by_id: std::collections::HashMap<u32, usize> = Default::default();
    let mut tracks: Vec<MvTrack> = Vec::new();

    for b in boxes(&moov) {
        if !b.is(b"trak") {
            continue;
        }
        let Some(tkhd) = crate::mp4::find(b.payload, &[b"tkhd"]) else { continue };
        let v1 = tkhd.first() == Some(&1);
        let track_id = be_u32(tkhd, if v1 { 20 } else { 12 });
        let Some(mdia) = crate::mp4::find(b.payload, &[b"mdia"]) else { continue };
        let Some(mdhd) = crate::mp4::find(mdia, &[b"mdhd"]) else { continue };
        let mdhd_v1 = mdhd.first() == Some(&1);
        let (timescale, language) = if mdhd_v1 {
            (be_u32(mdhd, 20), be_u16(mdhd, 36))
        } else {
            (be_u32(mdhd, 12), be_u16(mdhd, 20))
        };
        let kind: [u8; 4] = crate::mp4::find(mdia, &[b"hdlr"])
            .and_then(|h| h.get(8..12).and_then(|s| s.try_into().ok()))
            .unwrap_or(*b"vide");
        let Some(stsd) = crate::mp4::find(mdia, &[b"minf", b"stbl", b"stsd"]) else { continue };

        // Ancho y alto viven al final del tkhd, en 16.16 fijo.
        let off = if v1 { 32 } else { 20 } + 52;
        let width = be_u32(tkhd, off) >> 16;
        let height = be_u32(tkhd, off + 4) >> 16;

        by_id.insert(track_id, tracks.len());
        tracks.push(MvTrack {
            kind,
            timescale,
            stsd: mk(b"stsd", &super::cbcs::clean_stsd(stsd)),
            width,
            height,
            language,
            samples: Vec::new(),
        });
    }

    for (moof_start, payload) in &moofs {
        for traf in boxes(payload).filter(|b| b.is(b"traf")) {
            let Some(tfhd) = crate::mp4::find(traf.payload, &[b"tfhd"]) else { continue };
            let flags = full_flags(tfhd);
            let mut p = 4usize;
            let track_id = be_u32(tfhd, p);
            p += 4;
            let mut base = *moof_start;
            if flags & 0x1 != 0 {
                base = be_u64(tfhd, p);
                p += 8;
            }
            if flags & 0x2 != 0 {
                p += 4;
            }
            let (mut def_dur, mut def_size, mut def_flags) =
                trex_defaults.get(&track_id).copied().unwrap_or((0, 0, 0));
            if flags & 0x8 != 0 {
                def_dur = be_u32(tfhd, p);
                p += 4;
            }
            if flags & 0x10 != 0 {
                def_size = be_u32(tfhd, p);
                p += 4;
            }
            if flags & 0x20 != 0 {
                def_flags = be_u32(tfhd, p);
            }

            let Some(&ti) = by_id.get(&track_id) else { continue };

            for trun in boxes(traf.payload).filter(|b| b.is(b"trun")) {
                let tr = trun.payload;
                let tr_flags = full_flags(tr);
                let count = be_u32(tr, 4) as usize;
                let mut q = 8usize;
                let mut data_offset = 0i64;
                if tr_flags & 0x1 != 0 {
                    data_offset = be_u32(tr, q) as i32 as i64;
                    q += 4;
                }
                let mut first_flags = None;
                if tr_flags & 0x4 != 0 {
                    first_flags = Some(be_u32(tr, q));
                    q += 4;
                }
                let mut cur = (base as i64 + data_offset).max(0) as u64;
                for i in 0..count {
                    let (mut dur, mut size, mut cts) = (def_dur, def_size, 0i32);
                    let mut sflags = if i == 0 { first_flags.unwrap_or(def_flags) } else { def_flags };
                    if tr_flags & 0x100 != 0 {
                        dur = be_u32(tr, q);
                        q += 4;
                    }
                    if tr_flags & 0x200 != 0 {
                        size = be_u32(tr, q);
                        q += 4;
                    }
                    if tr_flags & 0x400 != 0 {
                        sflags = be_u32(tr, q);
                        q += 4;
                    }
                    if tr_flags & 0x800 != 0 {
                        cts = be_u32(tr, q) as i32;
                        q += 4;
                    }
                    tracks[ti].samples.push(MvSample {
                        offset: cur,
                        size,
                        duration: dur,
                        cts,
                        is_sync: sflags & 0x0001_0000 == 0,
                    });
                    cur += size as u64;
                }
            }
        }
    }

    Ok(tracks.into_iter().filter(|t| !t.samples.is_empty()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(size: u32, dur: u32) -> MvSample {
        MvSample { offset: 0, size, duration: dur, cts: 0, is_sync: true }
    }

    #[test]
    fn el_stts_se_comprime() {
        let s = vec![sample(1, 100), sample(1, 100), sample(1, 50)];
        let stts = tbl_stts(&s);
        assert_eq!(be_u32(&stts[8..], 4), 2, "dos entradas");
    }

    #[test]
    fn sin_offsets_de_composicion_no_se_emite_ctts() {
        assert!(tbl_ctts(&[sample(1, 10)]).is_none());
    }

    #[test]
    fn si_todo_es_sync_no_se_emite_stss() {
        assert!(tbl_stss(&[sample(1, 10)]).is_none());
    }

    /// Regresión: los samples de un mismo trozo NO son contiguos cuando el
    /// trozo cruza de un fragmento al siguiente (en medio va el `moof`).
    /// Copiarlos de una sola lectura mete la cabecera dentro del vídeo y el
    /// decodificador reporta "Invalid NAL unit size". Verificado contra el
    /// muxer original: con el fix el archivo sale byte a byte idéntico.
    #[test]
    fn se_copian_por_tramos_contiguos_no_por_rango() {
        // Dos samples pegados y un tercero detrás de un hueco (el moof).
        let track = MvTrack {
            kind: *b"vide",
            timescale: 1000,
            stsd: mk(b"stsd", &[0u8; 8]),
            width: 0,
            height: 0,
            language: 0,
            samples: vec![
                MvSample { offset: 0, size: 4, duration: 500, cts: 0, is_sync: true },
                MvSample { offset: 4, size: 4, duration: 500, cts: 0, is_sync: true },
                MvSample { offset: 100, size: 4, duration: 500, cts: 0, is_sync: true },
            ],
        };
        // El origen tiene basura entre medias, como el moof de verdad.
        let mut src = vec![0xEEu8; 200];
        src[0..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        src[100..104].copy_from_slice(&[9, 9, 9, 9]);

        let mut sources = vec![(track, std::io::Cursor::new(src))];
        let mut out = Vec::new();
        mux(&mut sources, &mut out).unwrap();

        let mdat = crate::mp4::boxes(&out).find(|b| b.is(b"mdat")).unwrap();
        assert_eq!(
            mdat.payload,
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 9, 9, 9],
            "solo los bytes de los samples, sin la basura de en medio"
        );
    }

    #[test]
    fn los_trozos_son_de_un_segundo() {
        let t = MvTrack {
            kind: *b"vide",
            timescale: 1000,
            stsd: Vec::new(),
            width: 1920,
            height: 1080,
            language: 0x55C4,
            samples: (0..10).map(|_| sample(100, 400)).collect(),
        };
        // 400 ms por sample → 3 samples pasan de 1 s
        assert_eq!(chunks_of(&t, 1.0), vec![(0, 3), (3, 6), (6, 9), (9, 10)]);
    }
}
