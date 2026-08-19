//! Primitivas de cajas MP4 y las tres transformaciones que necesita un track de
//! Apple Music: limpiar el init, descifrar fragmentos y armar el MP4 final.
//!
//! Todo trabaja sobre `&[u8]` de una caja concreta, nunca sobre el archivo
//! entero — es lo que permite que un continuous mix de una hora no cueste RAM.

pub mod assemble;
pub mod frag;
pub mod init;

use std::io::Read;

pub type FourCc = [u8; 4];

pub const fn cc(s: &[u8; 4]) -> FourCc {
    *s
}

/// Una caja dentro de un buffer: su tipo, su carga y dónde empieza y acaba.
#[derive(Debug, Clone, Copy)]
pub struct BoxRef<'a> {
    pub kind: FourCc,
    pub payload: &'a [u8],
    pub start: usize,
    pub end: usize,
}

impl BoxRef<'_> {
    pub fn is(&self, k: &[u8; 4]) -> bool {
        &self.kind == k
    }
}

/// Recorre las cajas de un buffer. Respeta `size == 1` (64 bits) y `size == 0`
/// ("hasta el final"), que aparecen en material real y no son teoría.
pub fn boxes(data: &[u8]) -> BoxIter<'_> {
    BoxIter { data, off: 0, done: false }
}

pub struct BoxIter<'a> {
    data: &'a [u8],
    off: usize,
    done: bool,
}

impl<'a> Iterator for BoxIter<'a> {
    type Item = BoxRef<'a>;

    fn next(&mut self) -> Option<BoxRef<'a>> {
        if self.done || self.off + 8 > self.data.len() {
            return None;
        }
        let off = self.off;
        let size32 = u32::from_be_bytes(self.data[off..off + 4].try_into().ok()?) as usize;
        let kind: FourCc = self.data[off + 4..off + 8].try_into().ok()?;

        let (payload_start, end) = match size32 {
            1 => {
                if off + 16 > self.data.len() {
                    self.done = true;
                    return None;
                }
                let size64 = u64::from_be_bytes(self.data[off + 8..off + 16].try_into().ok()?) as usize;
                let end = (off + size64).min(self.data.len());
                (off + 16, end)
            }
            0 => {
                // Caja abierta: llega hasta el final del buffer y no hay más después.
                self.done = true;
                (off + 8, self.data.len())
            }
            n if n < 8 => {
                // Tamaño imposible: el buffer está corrupto, se corta aquí en vez
                // de avanzar en bucle infinito.
                self.done = true;
                return None;
            }
            n => (off + 8, (off + n).min(self.data.len())),
        };

        self.off = if self.done { self.data.len() } else { end };
        Some(BoxRef { kind, payload: &self.data[payload_start.min(end)..end], start: off, end })
    }
}

/// Arma una caja con su cabecera de 32 bits.
pub fn mk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// Igual que `mk` pero escribiendo en un buffer existente (evita una copia).
pub fn mk_into(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
}

/// Busca una caja anidada por camino: `find(moov, &[b"trak", b"mdia", b"mdhd"])`.
pub fn find<'a>(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    let mut cur = data;
    for name in path {
        cur = boxes(cur).find(|b| b.is(name))?.payload;
    }
    Some(cur)
}

/// Reconstruye un contenedor aplicando una función a cada hijo. Devolver `None`
/// en la función elimina esa caja.
pub fn rebuild<F>(payload: &[u8], mut f: F) -> Vec<u8>
where
    F: FnMut(FourCc, &[u8]) -> Option<Vec<u8>>,
{
    let mut out = Vec::with_capacity(payload.len());
    for b in boxes(payload) {
        if let Some(new_payload) = f(b.kind, b.payload) {
            mk_into(&mut out, &b.kind, &new_payload);
        }
    }
    out
}

/// ¿Es un `sbgp`/`sgpd` que describe cifrado? Se mira el `grouping_type`.
///
/// Si estas cajas sobreviven, el reproductor sigue creyendo que el archivo está
/// protegido aunque los bytes ya estén en claro.
pub fn is_crypto_group(kind: FourCc, payload: &[u8]) -> bool {
    (&kind == b"sbgp" || &kind == b"sgpd")
        && payload.len() >= 8
        && matches!(&payload[4..8], b"seig" | b"seam")
}

pub fn be_u32(data: &[u8], off: usize) -> u32 {
    if off + 4 > data.len() {
        return 0;
    }
    u32::from_be_bytes(data[off..off + 4].try_into().unwrap())
}

pub fn be_u16(data: &[u8], off: usize) -> u16 {
    if off + 2 > data.len() {
        return 0;
    }
    u16::from_be_bytes(data[off..off + 2].try_into().unwrap())
}

pub fn be_u64(data: &[u8], off: usize) -> u64 {
    if off + 8 > data.len() {
        return 0;
    }
    u64::from_be_bytes(data[off..off + 8].try_into().unwrap())
}

/// Los 24 bits de flags de una FullBox.
pub fn full_flags(payload: &[u8]) -> u32 {
    be_u32(payload, 0) & 0x00FF_FFFF
}

/// Lee la siguiente caja completa (cabecera incluida) de un stream secuencial.
/// Devuelve `None` al llegar al final limpio.
pub fn read_box<R: Read>(r: &mut R) -> std::io::Result<Option<(FourCc, Vec<u8>)>> {
    let mut hdr = [0u8; 8];
    let mut got = 0;
    while got < 8 {
        match r.read(&mut hdr[got..])? {
            0 => {
                // EOF: limpio solo si no habíamos leído nada de esta caja.
                return Ok(None);
            }
            n => got += n,
        }
    }
    let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as usize;
    let kind: FourCc = hdr[4..8].try_into().unwrap();

    let mut raw = hdr.to_vec();
    match size32 {
        1 => {
            let mut ext = [0u8; 8];
            r.read_exact(&mut ext)?;
            let size64 = u64::from_be_bytes(ext) as usize;
            raw.extend_from_slice(&ext);
            let rest = size64.saturating_sub(16);
            raw.resize(16 + rest, 0);
            r.read_exact(&mut raw[16..])?;
        }
        0 => {
            r.read_to_end(&mut raw)?;
        }
        n if n < 8 => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("caja con tamaño imposible ({n})"),
            ));
        }
        n => {
            raw.resize(n, 0);
            r.read_exact(&mut raw[8..])?;
        }
    }
    Ok(Some((kind, raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorre_y_encuentra_cajas_anidadas() {
        let mdhd = mk(b"mdhd", &[1, 2, 3]);
        let mdia = mk(b"mdia", &mdhd);
        let trak = mk(b"trak", &mdia);
        let moov = mk(b"moov", &trak);
        let found = find(&moov[8..], &[b"trak", b"mdia", b"mdhd"]).unwrap();
        assert_eq!(found, &[1, 2, 3]);
    }

    #[test]
    fn una_caja_de_tamano_cero_llega_al_final() {
        let mut data = Vec::new();
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[9, 9, 9, 9]);
        let all: Vec<_> = boxes(&data).collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].payload, &[9, 9, 9, 9]);
    }

    #[test]
    fn un_tamano_imposible_corta_en_vez_de_colgarse() {
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_be_bytes()); // < 8
        data.extend_from_slice(b"junk");
        assert_eq!(boxes(&data).count(), 0);
    }
}
