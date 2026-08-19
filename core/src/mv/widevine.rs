//! CDM de Widevine, solo para music videos.
//!
//! Los music videos no van por FairPlay: usan Widevine, así que el wrapper no
//! pinta nada aquí. Este módulo construye la petición de licencia y saca la
//! llave de contenido de la respuesta.
//!
//! **Las credenciales del dispositivo no se empotran en el binario.** Se leen de
//! disco (`widevine-device-key` y `widevine-client-id` en el config), que es la
//! misma regla que ya sigue ECBP Desktop: una credencial dentro de un ejecutable
//! se saca con `strings`, y una compartida por todos no se puede revocar.

use crate::config::Config;
use crate::error::{Error, Result};
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use base64::Engine;
use cmac::{Cmac, Mac};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::{Oaep, RsaPrivateKey};
use sha1::Sha1;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

// ── protobuf a mano ───────────────────────────────────────────────────────
// Son cuatro campos contados: meter prost y un build.rs por esto sería peor.

fn uvarint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = (n & 0x7F) as u8;
        n >>= 7;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            return out;
        }
    }
}

fn pb_varint(field: u32, value: u64) -> Vec<u8> {
    let mut out = uvarint((field << 3) as u64);
    out.extend_from_slice(&uvarint(value));
    out
}

fn pb_bytes(field: u32, value: &[u8]) -> Vec<u8> {
    let mut out = uvarint(((field << 3) | 2) as u64);
    out.extend_from_slice(&uvarint(value.len() as u64));
    out.extend_from_slice(value);
    out
}

fn read_varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut shift = 0u32;
    let mut out = 0u64;
    while *i < buf.len() {
        let b = buf[*i];
        *i += 1;
        out |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(out);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
    None
}

/// Parseo mínimo: número de campo → lista de cargas (solo longitud-delimitada).
fn pb_parse(buf: &[u8]) -> std::collections::HashMap<u32, Vec<Vec<u8>>> {
    let mut out: std::collections::HashMap<u32, Vec<Vec<u8>>> = Default::default();
    let mut i = 0usize;
    while i < buf.len() {
        let Some(tag) = read_varint(buf, &mut i) else { break };
        let field = (tag >> 3) as u32;
        match tag & 0x7 {
            0 => {
                let Some(v) = read_varint(buf, &mut i) else { break };
                out.entry(field).or_default().push(uvarint(v));
            }
            2 => {
                let Some(len) = read_varint(buf, &mut i) else { break };
                let end = i + len as usize;
                if end > buf.len() {
                    break;
                }
                out.entry(field).or_default().push(buf[i..end].to_vec());
                i = end;
            }
            5 => i += 4,
            1 => i += 8,
            _ => break,
        }
    }
    out
}

/// El PSSH que espera Apple. Los 32 bytes de delante son relleno que el CDM
/// vuelve a quitar; se mantienen para que la petición salga byte a byte igual
/// que la del cliente original.
pub fn build_pssh(kid_base64: &str) -> Result<Vec<u8>> {
    let kid = base64::engine::general_purpose::STANDARD
        .decode(kid_base64)
        .map_err(|_| Error::Other("el KID de Widevine no es base64".into()))?;
    let mut header = pb_varint(1, 1); // algoritmo = AESCTR
    header.extend_from_slice(&pb_bytes(2, &kid));
    header.extend_from_slice(&pb_bytes(3, b"")); // provider
    header.extend_from_slice(&pb_bytes(4, b"")); // content_id
    header.extend_from_slice(&pb_bytes(6, b"")); // policy

    let mut out = b"0123456789abcdef0123456789abcdef".to_vec();
    out.extend_from_slice(&header);
    Ok(out)
}

pub struct Cdm {
    cenc_header: Vec<u8>,
    private_key: RsaPrivateKey,
    client_id: Vec<u8>,
    session_id: Vec<u8>,
    request_msg: Vec<u8>,
}

/// Dónde se buscan las credenciales del dispositivo, en orden.
///
/// El blob de Widevine no es un secreto del usuario: es el del propio
/// reproductor y viene con la app. Aun así se lee de disco y no se empotra en el
/// binario, que es la regla de la casa (una credencial dentro de un ejecutable
/// se saca con `strings` y no se puede rotar sin recompilar). Para el usuario es
/// transparente: el instalador la deja en `resources/` y aquí se encuentra sola.
fn candidates(name: &str) -> Vec<std::path::PathBuf> {
    let mut out = vec![Config::config_dir().join("widevine").join(name)];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("resources").join("widevine").join(name));
            out.push(dir.join("widevine").join(name));
        }
    }
    out
}

fn find_credential(explicit: Option<&std::path::PathBuf>, name: &str) -> Option<std::path::PathBuf> {
    if let Some(p) = explicit {
        if p.exists() {
            return Some(p.clone());
        }
    }
    candidates(name).into_iter().find(|p| p.exists())
}

/// Carga las credenciales del dispositivo.
fn load_device(cfg: &Config) -> Result<(RsaPrivateKey, Vec<u8>)> {
    let key_path = find_credential(cfg.widevine_device_key.as_ref(), "device.pem").ok_or_else(|| {
        Error::Config(format!(
            "faltan las credenciales de Widevine para los music videos. Se buscaron en: {}",
            candidates("device.pem").iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        ))
    })?;
    let id_path = find_credential(cfg.widevine_client_id.as_ref(), "client_id.bin")
        .ok_or_else(|| Error::Config("falta el ClientId de Widevine (client_id.bin)".into()))?;
    let (key_path, id_path) = (&key_path, &id_path);

    let pem = std::fs::read_to_string(key_path)
        .map_err(|e| Error::Config(format!("no se pudo leer {}: {e}", key_path.display())))?;
    // Se aceptan las dos formas de PEM que se ven por ahí.
    let key = RsaPrivateKey::from_pkcs1_pem(&pem)
        .or_else(|_| RsaPrivateKey::from_pkcs8_pem(&pem))
        .map_err(|e| Error::Config(format!("la llave de dispositivo no es un RSA en PEM válido: {e}")))?;

    let raw = std::fs::read(id_path)
        .map_err(|e| Error::Config(format!("no se pudo leer {}: {e}", id_path.display())))?;
    // El blob puede estar en base64 o en binario.
    let client_id = base64::engine::general_purpose::STANDARD
        .decode(String::from_utf8_lossy(&raw).trim())
        .unwrap_or(raw);

    Ok((key, client_id))
}

impl Cdm {
    pub fn new(cfg: &Config, init_data: &[u8]) -> Result<Self> {
        if init_data.len() < 32 {
            return Err(Error::Other("initData de Widevine demasiado corto".into()));
        }
        let (private_key, client_id) = load_device(cfg)?;

        // Id de sesión con la pinta que espera el servidor: 16 caracteres del
        // alfabeto hexadecimal en mayúsculas, "01" y relleno.
        use rand::Rng;
        let alphabet = b"ABCDEF0123456789";
        let mut rng = rand::thread_rng();
        let mut session_id: Vec<u8> = (0..16).map(|_| alphabet[rng.gen_range(0..16)]).collect();
        session_id.extend_from_slice(b"01");
        session_id.extend_from_slice(&[b'0'; 14]);

        Ok(Self {
            cenc_header: init_data[32..].to_vec(),
            private_key,
            client_id,
            session_id,
            request_msg: Vec::new(),
        })
    }

    /// Construye y firma la petición de licencia.
    pub fn license_request(&mut self) -> Result<Vec<u8>> {
        let mut cenc = pb_bytes(1, &self.cenc_header);
        cenc.extend_from_slice(&pb_varint(2, 1)); // LicenseType::DEFAULT
        cenc.extend_from_slice(&pb_bytes(3, &self.session_id));

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut msg = pb_bytes(1, &self.client_id);
        msg.extend_from_slice(&pb_bytes(2, &pb_bytes(1, &cenc))); // ContentId::CencId
        msg.extend_from_slice(&pb_varint(3, 1));  // RequestType::NEW
        msg.extend_from_slice(&pb_varint(4, now));
        msg.extend_from_slice(&pb_varint(6, 21)); // ProtocolVersion::CURRENT
        msg.extend_from_slice(&pb_varint(7, rand::random::<u32>() as u64));
        self.request_msg = msg.clone();

        use rsa::signature::{RandomizedSigner, SignatureEncoding};
        let signing_key = rsa::pss::SigningKey::<Sha1>::new(self.private_key.clone());
        let sig = signing_key
            .sign_with_rng(&mut rand::thread_rng(), &msg)
            .to_vec();

        let mut out = pb_varint(1, 1); // SignedMessage::LICENSE_REQUEST
        out.extend_from_slice(&pb_bytes(2, &msg));
        out.extend_from_slice(&pb_bytes(3, &sig));
        Ok(out)
    }

    /// Saca la llave de contenido (en hex) de la licencia.
    pub fn content_key(&self, license: &[u8]) -> Result<String> {
        let signed = pb_parse(license);
        let session_key_enc = signed
            .get(&4)
            .and_then(|v| v.first())
            .ok_or_else(|| Error::Other("la licencia no trae session key".into()))?;
        let license_msg = signed
            .get(&2)
            .and_then(|v| v.first())
            .ok_or_else(|| Error::Other("la licencia viene malformada".into()))?;

        let session_key = self
            .private_key
            .decrypt(Oaep::new::<Sha1>(), session_key_enc)
            .map_err(|e| Error::Other(format!("no se pudo abrir la session key: {e}")))?;

        // Derivación estándar de Widevine: CMAC sobre el mensaje de petición.
        let mut mac = <Cmac<aes::Aes128> as Mac>::new_from_slice(&session_key)
            .map_err(|_| Error::Other("session key de tamaño inesperado".into()))?;
        mac.update(b"\x01ENCRYPTION\x00");
        mac.update(&self.request_msg);
        mac.update(&[0x00, 0x00, 0x00, 0x80]);
        let enc_key = mac.finalize().into_bytes();

        for container in pb_parse(license_msg).get(&3).into_iter().flatten() {
            let f = pb_parse(container);
            // Solo interesa la llave de CONTENIDO (tipo 2); las demás son de
            // firma y de control.
            let key_type = f.get(&4).and_then(|v| v.first()).and_then(|b| b.first()).copied().unwrap_or(0);
            if key_type != 2 {
                continue;
            }
            let (Some(iv), Some(enc)) = (
                f.get(&2).and_then(|v| v.first()),
                f.get(&3).and_then(|v| v.first()),
            ) else {
                continue;
            };
            if iv.len() != 16 || enc.len() % 16 != 0 || enc.is_empty() {
                continue;
            }
            let mut buf = enc.clone();
            let mut dec = Aes128CbcDec::new(enc_key.as_slice().into(), iv.as_slice().into());
            for block in buf.chunks_mut(16) {
                dec.decrypt_block_mut(block.into());
            }
            // PKCS#7
            if let Some(&pad) = buf.last() {
                let pad = pad as usize;
                if pad > 0 && pad <= buf.len() {
                    buf.truncate(buf.len() - pad);
                }
            }
            return Ok(hex::encode(buf));
        }
        Err(Error::Other("la licencia no traía llave de contenido".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_varint_se_codifican_como_manda_protobuf() {
        assert_eq!(uvarint(0), vec![0x00]);
        assert_eq!(uvarint(300), vec![0xAC, 0x02]);
        assert_eq!(pb_varint(1, 1), vec![0x08, 0x01]);
        assert_eq!(pb_bytes(2, b"hi"), vec![0x12, 0x02, b'h', b'i']);
    }

    #[test]
    fn el_parseo_recupera_lo_que_se_escribio() {
        let mut buf = pb_bytes(2, b"hola");
        buf.extend_from_slice(&pb_varint(3, 7));
        let parsed = pb_parse(&buf);
        assert_eq!(parsed[&2][0], b"hola");
        assert_eq!(parsed[&3][0], vec![7]);
    }

    #[test]
    fn el_pssh_lleva_el_relleno_de_32_bytes() {
        let pssh = build_pssh(&base64::engine::general_purpose::STANDARD.encode([0u8; 16])).unwrap();
        assert_eq!(&pssh[..32], b"0123456789abcdef0123456789abcdef");
        assert!(pssh.len() > 32);
    }
}
