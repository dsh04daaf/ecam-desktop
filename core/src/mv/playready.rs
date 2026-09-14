//! CDM de PlayReady, sólo para el tier SL3000 de los music videos.
//!
//! Desde el **2026-09-10** Apple sirve las variantes de 1440p y 4K únicamente por
//! PlayReady SL3000: en ellas Widevine devuelve `-42585` y un cliente FairPlay
//! Baseline también se rechaza. El manifiesto lo declara en `ALLOWED-CPC`
//! (ver [`super::needs_playready`]).
//!
//! Es el port del `ecplayready.py` que usan el bot y AMDL — **la misma
//! implementación, escrita por nosotros**, no una librería de terceros. Al portar
//! se mantiene el orden de los campos del XML, porque el challenge va firmado y
//! cualquier cambio de serialización invalida la firma.
//!
//! Igual que en Python, se implementa sólo lo que Apple usa: nada de listas de
//! revocación, verificación de cadena, dominios ni licencias encadenadas. Lo que
//! no está soportado levanta un error claro en vez de seguir a ciegas.
//!
//! **Las credenciales no van dentro del binario**: el `.prd` se lee de disco, la
//! misma regla que ya sigue el CDM de Widevine.

use crate::error::{Error, Result};
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use base64::Engine;
use cmac::{Cmac, Mac};
use p256::elliptic_curve::sec1::{FromSec1Point, Sec1Point, ToSec1Point};
use p256::elliptic_curve::PrimeField;
use p256::{AffinePoint, FieldBytes, NistP256, ProjectivePoint, Scalar};
use rand::RngCore;

type EncodedPoint = Sec1Point<NistP256>;

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn err(msg: impl Into<String>) -> Error {
    Error::Other(msg.into())
}

// ── curva ────────────────────────────────────────────────────────────────

/// Llave pública del servidor WMRM; es la misma para todo PlayReady.
const WMRM_X: [u8; 32] = [
    0xc8, 0xb6, 0xaf, 0x16, 0xee, 0x94, 0x1a, 0xad, 0xaa, 0x53, 0x89, 0xb4, 0xaf, 0x2c, 0x10, 0xe3,
    0x56, 0xbe, 0x42, 0xaf, 0x17, 0x5e, 0xf3, 0xfa, 0xce, 0x93, 0x25, 0x4e, 0x7b, 0x0b, 0x3d, 0x9b,
];
const WMRM_Y: [u8; 32] = [
    0x98, 0x2b, 0x27, 0xb5, 0xcb, 0x23, 0x41, 0x32, 0x6e, 0x56, 0xaa, 0x85, 0x7d, 0xbf, 0xd5, 0xc6,
    0x34, 0xce, 0x2c, 0xf9, 0xea, 0x74, 0xfc, 0xa8, 0xf2, 0xaf, 0x59, 0x57, 0xef, 0xee, 0xa5, 0x62,
];

fn point_from_xy(x: &[u8], y: &[u8]) -> Result<ProjectivePoint> {
    if x.len() != 32 || y.len() != 32 {
        return Err(err("coordenadas que no son de 32 bytes"));
    }
    let enc = EncodedPoint::from_affine_coordinates(
        FieldBytes::try_from(x).map_err(|_| err("X inválida"))?.as_ref(),
        FieldBytes::try_from(y).map_err(|_| err("Y inválida"))?.as_ref(),
        false,
    );
    let aff = Option::<AffinePoint>::from(AffinePoint::from_sec1_point(&enc))
        .ok_or_else(|| err("el punto no está en la curva P-256"))?;
    Ok(ProjectivePoint::from(aff))
}

/// (x, y) de un punto, 32 bytes cada una.
///
/// Siempre 32: la versión de upstream usaba longitud variable y una coordenada
/// que empezara por un byte cero salía más corta, descolocando todo el troceado
/// posterior. Pasa 1 de cada 256 veces y es dificilísimo de perseguir.
fn point_xy(p: &ProjectivePoint) -> Result<(Vec<u8>, Vec<u8>)> {
    let aff = p.to_affine();
    if bool::from(aff.is_identity()) {
        return Err(err("punto en el infinito"));
    }
    let enc = aff.to_sec1_point(false);
    Ok((
        enc.x().ok_or_else(|| err("sin X"))?.to_vec(),
        enc.y().ok_or_else(|| err("sin Y"))?.to_vec(),
    ))
}

fn scalar_from(bytes: &[u8]) -> Result<Scalar> {
    let fb = FieldBytes::try_from(bytes).map_err(|_| err("escalar que no es de 32 bytes"))?;
    Option::<Scalar>::from(Scalar::from_repr(fb))
        .filter(|s| !bool::from(<Scalar as p256::elliptic_curve::Field>::is_zero(s)))
        .ok_or_else(|| err("escalar fuera del orden de la curva"))
}

fn random_scalar() -> Scalar {
    loop {
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        if let Ok(s) = scalar_from(&buf) {
            return s;
        }
    }
}

fn elgamal_encrypt(message: &ProjectivePoint, public: &ProjectivePoint) -> Result<Vec<u8>> {
    let k = random_scalar();
    let p1 = ProjectivePoint::GENERATOR * k;
    let p2 = *message + (*public * k);
    let (x1, y1) = point_xy(&p1)?;
    let (x2, y2) = point_xy(&p2)?;
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(&x1);
    out.extend_from_slice(&y1);
    out.extend_from_slice(&x2);
    out.extend_from_slice(&y2);
    Ok(out)
}

/// Devuelve la coordenada X del punto descifrado (32 bytes).
fn elgamal_decrypt(ciphertext: &[u8], d: &Scalar) -> Result<Vec<u8>> {
    if ciphertext.len() < 128 {
        return Err(err(format!(
            "ciphertext ElGamal corto: {} bytes",
            ciphertext.len()
        )));
    }
    let p1 = point_from_xy(&ciphertext[0..32], &ciphertext[32..64])?;
    let p2 = point_from_xy(&ciphertext[64..96], &ciphertext[96..128])?;
    let m = p2 - (p1 * d);
    Ok(point_xy(&m)?.0)
}

// ── AES ──────────────────────────────────────────────────────────────────

fn aes_ecb_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if key.len() != 16 || data.len() % 16 != 0 {
        return Err(err("AES-ECB: tamaños inválidos"));
    }
    let cipher = Aes128::new_from_slice(key).map_err(|_| err("llave AES inválida"))?;
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(16) {
        cipher.encrypt_block(chunk.into());
    }
    Ok(out)
}

fn aes_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    type Enc = cbc::Encryptor<Aes128>;
    let enc = Enc::new_from_slices(key, iv).map_err(|_| err("AES-CBC: llave o IV inválidos"))?;
    let mut buf = vec![0u8; data.len() + 16];
    let n = enc
        .encrypt_padded_b2b_mut::<Pkcs7>(data, &mut buf)
        .map_err(|_| err("AES-CBC falló"))?
        .len();
    buf.truncate(n);
    Ok(buf)
}

fn aes_cmac(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(key).map_err(|_| err("CMAC: llave inválida"))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

// ── dispositivo (.prd) ───────────────────────────────────────────────────

const BCERT_TAG_BASIC: u16 = 0x0001;

fn be_u16(b: &[u8], off: usize) -> Result<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| err("lectura fuera de rango"))
}

fn be_u32(b: &[u8], off: usize) -> Result<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| err("lectura fuera de rango"))
}

/// Dispositivo PlayReady leído de un `.prd`.
pub struct Device {
    encryption_key: Scalar,
    signing_key: Scalar,
    /// Cadena CHAI **en crudo**: se reenvía tal cual, sin re-serializar. Ahí es
    /// donde upstream gasta 744 líneas parseando BCert para nada.
    group_certificate: Vec<u8>,
    pub security_level: u32,
}

/// A mano y sin las llaves: un `derive(Debug)` las volcaría en cuanto alguien
/// imprimiera un error, que es justo lo que no queremos de una credencial.
impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("security_level", &self.security_level)
            .field("certificate_bytes", &self.group_certificate.len())
            .finish_non_exhaustive()
    }
}

impl Device {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 8 || &data[0..3] != b"PRD" {
            return Err(err("no es un archivo .prd (falta la marca PRD)"));
        }
        let version = data[3];
        let mut pos = 4usize;

        let (enc_raw, sig_raw, cert): (&[u8], &[u8], &[u8]) = match version {
            3 => {
                pos += 96; // group_key, no se usa
                let enc = data.get(pos..pos + 96).ok_or_else(|| err(".prd truncado"))?;
                pos += 96;
                let sig = data.get(pos..pos + 96).ok_or_else(|| err(".prd truncado"))?;
                pos += 96;
                let len = be_u32(data, pos)? as usize;
                pos += 4;
                let cert = data.get(pos..pos + len).ok_or_else(|| err("certificado truncado"))?;
                (enc, sig, cert)
            }
            2 => {
                let len = be_u32(data, pos)? as usize;
                pos += 4;
                let cert = data.get(pos..pos + len).ok_or_else(|| err("certificado truncado"))?;
                pos += len;
                let enc = data.get(pos..pos + 96).ok_or_else(|| err(".prd truncado"))?;
                pos += 96;
                let sig = data.get(pos..pos + 96).ok_or_else(|| err(".prd truncado"))?;
                (enc, sig, cert)
            }
            1 => return Err(err("los .prd v1 no traen llaves de dispositivo; no sirven")),
            v => return Err(err(format!("versión de .prd desconocida: {v}"))),
        };

        // La privada son los primeros 32 bytes; la pública se deriva.
        let encryption_key = scalar_from(&enc_raw[..32])?;
        let signing_key = scalar_from(&sig_raw[..32])?;
        let security_level = Self::security_level_of(cert)?;

        Ok(Self {
            encryption_key,
            signing_key,
            group_certificate: cert.to_vec(),
            security_level,
        })
    }

    fn security_level_of(chain: &[u8]) -> Result<u32> {
        if chain.len() < 24 || &chain[0..4] != b"CHAI" {
            return Err(err("la cadena de certificados no empieza por CHAI"));
        }
        // "CHAI" + version(4) + total_length(4) + flags(4) + certificate_count(4)
        let cert_start = 20usize;
        if chain.get(cert_start..cert_start + 4) != Some(b"CERT") {
            return Err(err("no hay un CERT donde debería"));
        }
        // "CERT" + version(4) + total_length(4) + certificate_length(4)
        let mut pos = cert_start + 16;
        let cert_end = cert_start + be_u32(chain, cert_start + 8)? as usize;
        while pos + 8 <= cert_end.min(chain.len()) {
            let tag = be_u16(chain, pos + 2)?;
            let length = be_u32(chain, pos + 4)? as usize;
            if length < 8 || pos + length > chain.len() {
                break;
            }
            if tag == BCERT_TAG_BASIC {
                // cert_id(16) + security_level(4)
                return be_u32(chain, pos + 8 + 16);
            }
            pos += length;
        }
        Err(err("el certificado no declara nivel de seguridad"))
    }

    fn public_bytes(&self, scalar: &Scalar) -> Result<Vec<u8>> {
        let (x, y) = point_xy(&(ProjectivePoint::GENERATOR * scalar))?;
        let mut out = x;
        out.extend_from_slice(&y);
        Ok(out)
    }
}

// ── PlayReady Object / WRM header ────────────────────────────────────────

const PSSH_SYSTEM_ID: [u8; 16] = [
    0x9a, 0x04, 0xf0, 0x79, 0x98, 0x40, 0x42, 0x86, 0xab, 0x92, 0xe6, 0x5b, 0xe0, 0x88, 0x5f, 0x95,
];

/// WRM header (XML) a partir de una caja PSSH o de un PlayReady Object en base64.
///
/// Apple lo entrega en el `URI=` de la línea `#EXT-X-KEY` con
/// `KEYFORMAT="com.microsoft.playready"`.
pub fn wrm_header_from_pssh(payload_b64: &str) -> Result<String> {
    let data = B64
        .decode(payload_b64.trim())
        .map_err(|e| err(format!("el PSSH no es base64 válido: {e}")))?;
    if data.is_empty() {
        return Err(err("PSSH vacío"));
    }

    // ¿Caja PSSH ISO-BMFF? size(4) + 'pssh' + version/flags(4) + systemid(16)
    let pro: &[u8] = if data.len() > 28 && &data[4..8] == b"pssh" {
        if data[12..28] != PSSH_SYSTEM_ID {
            return Err(err("la caja PSSH no es de PlayReady"));
        }
        let mut pos = 28usize;
        if data[8] > 0 {
            let kid_count = be_u32(&data, pos)? as usize;
            pos += 4 + 16 * kid_count;
        }
        let len = be_u32(&data, pos)? as usize;
        pos += 4;
        data.get(pos..pos + len).ok_or_else(|| err("caja PSSH truncada"))?
    } else {
        &data
    };

    // PlayReady Object: length(4 LE) + record_count(2 LE) + registros
    if pro.len() < 6 {
        return Err(err("no se reconoce como caja PSSH ni PlayReady Object"));
    }
    let pro_len = u32::from_le_bytes([pro[0], pro[1], pro[2], pro[3]]) as usize;
    let count = u16::from_le_bytes([pro[4], pro[5]]) as usize;
    if pro_len != pro.len() || count == 0 || count >= 64 {
        return Err(err("no se reconoce como caja PSSH ni PlayReady Object"));
    }
    let mut pos = 6usize;
    for _ in 0..count {
        if pos + 4 > pro.len() {
            break;
        }
        let rec_type = u16::from_le_bytes([pro[pos], pro[pos + 1]]);
        let rec_len = u16::from_le_bytes([pro[pos + 2], pro[pos + 3]]) as usize;
        pos += 4;
        let body = pro.get(pos..pos + rec_len).ok_or_else(|| err("registro truncado"))?;
        pos += rec_len;
        if rec_type == 1 {
            // UTF-16LE
            let u16s: Vec<u16> = body
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            return String::from_utf16(&u16s)
                .map_err(|_| err("el WRM header no es UTF-16 válido"));
        }
    }
    Err(err("el PlayReady Object no trae WRM header"))
}

fn wrm_protocol_version(wrm: &str) -> u32 {
    if wrm.contains("version=\"4.3.0.0\"") {
        5
    } else if wrm.contains("version=\"4.2.0.0\"") {
        4
    } else {
        1
    }
}

// ── XMR (la licencia) ────────────────────────────────────────────────────

const XMR_CONTENT_KEY: u16 = 0x000A;
const XMR_SIGNATURE: u16 = 0x000B;
const XMR_ECC_DEVICE_KEY: u16 = 0x002A;
const XMR_AUX_KEY: u16 = 0x0051;

const CIPHER_ECC_256: u16 = 0x0003;
const CIPHER_ECC_256_WITH_KZ: u16 = 0x0004;
const CIPHER_ECC_256_VIA_SYMMETRIC: u16 = 0x0006;

/// Constante del propio PlayReady para derivar la llave en el camino "scalable".
const MAGIC_ZERO: [u8; 16] = [
    0x7e, 0xe9, 0xed, 0x4a, 0xf7, 0x73, 0x22, 0x4f, 0x00, 0xb8, 0xea, 0x7e, 0xfb, 0x02, 0x7c, 0xbb,
];

/// Recorre los objetos XMR entrando en los contenedores (flags 2 y 3).
fn xmr_walk(data: &[u8], start: usize, end: usize, out: &mut Vec<(u16, (usize, usize))>) {
    let mut pos = start;
    while pos + 8 <= end {
        let flags = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let type_ = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
        let length =
            u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        if length < 8 || pos + length > end {
            return;
        }
        let (body_start, body_end) = (pos + 8, pos + length);
        if flags == 2 || flags == 3 {
            xmr_walk(data, body_start, body_end, out);
        } else {
            out.push((type_, (body_start, body_end)));
        }
        pos += length;
    }
}

struct Xmr<'a> {
    raw: &'a [u8],
    objects: Vec<(u16, (usize, usize))>,
}

impl<'a> Xmr<'a> {
    fn parse(raw: &'a [u8]) -> Result<Self> {
        if raw.len() < 24 || &raw[0..4] != b"XMR\0" {
            return Err(err("la licencia no empieza por XMR"));
        }
        let mut objects = Vec::new();
        // "XMR\0" + version(4) + rights_id(16)
        xmr_walk(raw, 24, raw.len(), &mut objects);
        Ok(Self { raw, objects })
    }

    fn first(&self, type_: u16) -> Option<&'a [u8]> {
        self.objects
            .iter()
            .find(|(t, _)| *t == type_)
            .map(|(_, (s, e))| &self.raw[*s..*e])
    }

    /// Devuelve (key_id, content_key).
    fn content_key(&self, encryption_key: &Scalar, device_public: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
        let ecc = self
            .first(XMR_ECC_DEVICE_KEY)
            .ok_or_else(|| err("la licencia no trae llave pública ECC del dispositivo"))?;
        let key_len = be_u16(ecc, 2)? as usize;
        if ecc.get(4..4 + key_len) != Some(device_public) {
            return Err(err("la licencia va dirigida a otro dispositivo"));
        }

        let ck = self
            .first(XMR_CONTENT_KEY)
            .ok_or_else(|| err("la licencia no trae objeto de llave de contenido"))?;
        let key_id = ck.get(0..16).ok_or_else(|| err("objeto de llave truncado"))?.to_vec();
        let cipher_type = be_u16(ck, 18)?;
        let key_length = be_u16(ck, 20)? as usize;
        let encrypted = ck
            .get(22..22 + key_length)
            .ok_or_else(|| err("llave cifrada truncada"))?;

        if !matches!(
            cipher_type,
            CIPHER_ECC_256 | CIPHER_ECC_256_WITH_KZ | CIPHER_ECC_256_VIA_SYMMETRIC
        ) {
            return Err(err(format!("cipher_type {cipher_type} no soportado")));
        }

        let decrypted = elgamal_decrypt(encrypted, encryption_key)?;
        let mut ci = decrypted[0..16].to_vec();
        let mut ck_bytes = decrypted[16..32].to_vec();

        if let Some(aux) = self.first(XMR_AUX_KEY) {
            // "scalable": los dos valores van entrelazados byte a byte.
            ci = decrypted.iter().step_by(2).take(16).copied().collect();
            ck_bytes = decrypted.iter().skip(1).step_by(2).take(16).copied().collect();

            if cipher_type == CIPHER_ECC_256_VIA_SYMMETRIC {
                if encrypted.len() < 176 {
                    return Err(err(format!(
                        "llave via_symmetric corta: {} bytes, hacen falta 176",
                        encrypted.len()
                    )));
                }
                let (root, leaf_in) = encrypted.split_at(144);
                let rgb: Vec<u8> = ck_bytes
                    .iter()
                    .zip(MAGIC_ZERO.iter())
                    .map(|(a, b)| a ^ b)
                    .collect();
                let content_key_prime = aes_ecb_encrypt(&ck_bytes, &rgb)?;
                // count(2) + [location(4) + key(16)]...
                let aux_key = aux.get(6..22).ok_or_else(|| err("objeto AUX truncado"))?;
                let uplink_x_key = aes_ecb_encrypt(&content_key_prime, aux_key)?;
                let secondary_key = aes_ecb_encrypt(&ck_bytes, &root[128..144])?;

                let leaf = aes_ecb_encrypt(&uplink_x_key, leaf_in)?;
                let leaf = aes_ecb_encrypt(&secondary_key, &leaf)?;
                ci = leaf[0..16].to_vec();
                ck_bytes = leaf[16..32].to_vec();
            }
        }

        self.check_signature(&ci)?;
        Ok((key_id, ck_bytes))
    }

    fn check_signature(&self, integrity_key: &[u8]) -> Result<()> {
        let sig = self
            .first(XMR_SIGNATURE)
            .ok_or_else(|| err("la licencia no trae objeto de firma"))?;
        // signature_type(2) + signature_data_length(2) + signature_data
        let sig_len = be_u16(sig, 2)? as usize;
        let signature = sig.get(4..4 + sig_len).ok_or_else(|| err("firma truncada"))?;
        let cut = self
            .raw
            .len()
            .checked_sub(sig_len + 12)
            .ok_or_else(|| err("licencia demasiado corta para su firma"))?;
        if aes_cmac(integrity_key, &self.raw[..cut])? != signature {
            return Err(err("la firma de integridad de la licencia no cuadra"));
        }
        Ok(())
    }
}

// ── CDM ──────────────────────────────────────────────────────────────────

const NS_PROTOCOLS: &str = "http://schemas.microsoft.com/DRM/2007/03/protocols";
const NS_MESSAGES: &str = "http://schemas.microsoft.com/DRM/2007/03/protocols/messages";
const NS_XMLENC: &str = "http://www.w3.org/2001/04/xmlenc#";
const NS_XMLDSIG: &str = "http://www.w3.org/2000/09/xmldsig#";

/// Una sesión: la XMLKey es una pareja ECC efímera cuya X da IV+llave AES.
pub struct Session {
    scalar: Scalar,
    aes_iv: Vec<u8>,
    aes_key: Vec<u8>,
}

impl Session {
    fn new() -> Result<Self> {
        let scalar = random_scalar();
        let (x, _y) = point_xy(&(ProjectivePoint::GENERATOR * scalar))?;
        Ok(Self {
            scalar,
            aes_iv: x[..16].to_vec(),
            aes_key: x[16..].to_vec(),
        })
    }
}

pub struct Cdm {
    device: Device,
    encryption_public: Vec<u8>,
    signing_public: Vec<u8>,
}

impl Cdm {
    const CLIENT_VERSION: &'static str = "10.0.16384.10011";

    pub fn new(device: Device) -> Result<Self> {
        let encryption_public = device.public_bytes(&device.encryption_key)?;
        let signing_public = device.public_bytes(&device.signing_key)?;
        Ok(Self {
            device,
            encryption_public,
            signing_public,
        })
    }

    pub fn open(&self) -> Result<Session> {
        Session::new()
    }

    fn client_data(&self, session: &Session) -> Result<Vec<u8>> {
        // El orden y los espacios alrededor del certificado son los del cliente real.
        let body = format!(
            "<Data><CertificateChains><CertificateChain> {} </CertificateChain></CertificateChains>\
             <Features><Feature Name=\"AESCBC\"></Feature><REE><AESCBCS></AESCBCS></REE></Features></Data>",
            B64.encode(&self.device.group_certificate)
        );
        let mut out = session.aes_iv.clone();
        out.extend_from_slice(&aes_cbc_encrypt(
            &session.aes_key,
            &session.aes_iv,
            body.as_bytes(),
        )?);
        Ok(out)
    }

    /// Challenge SOAP listo para mandar al servidor de licencias.
    pub fn license_challenge(&self, session: &Session, wrm_header: &str) -> Result<String> {
        let mut nonce = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let wmrm = point_from_xy(&WMRM_X, &WMRM_Y)?;
        let xml_key_point = ProjectivePoint::GENERATOR * session.scalar;

        // El elemento LA, serializado exactamente como lo hace la versión en
        // Python: sin espacios entre etiquetas, sin auto-cierre y con los
        // atributos en este orden. El digest se calcula sobre estos bytes, así
        // que cualquier cambio de formato invalida la firma.
        let la = format!(
            "<LA xmlns=\"{ns}\" Id=\"SignedData\" xml:space=\"preserve\">\
             <Version>{ver}</Version>\
             <ContentHeader>{wrm}</ContentHeader>\
             <CLIENTINFO><CLIENTVERSION>{cv}</CLIENTVERSION></CLIENTINFO>\
             <LicenseNonce>{nonce}</LicenseNonce>\
             <ClientTime>{now}</ClientTime>\
             <EncryptedData xmlns=\"{enc}\" Type=\"{enc}Element\">\
             <EncryptionMethod Algorithm=\"{enc}aes128-cbc\"></EncryptionMethod>\
             <KeyInfo xmlns=\"{dsig}\">\
             <EncryptedKey xmlns=\"{enc}\">\
             <EncryptionMethod Algorithm=\"{ns}#ecc256\"></EncryptionMethod>\
             <KeyInfo xmlns=\"{dsig}\"><KeyName>WMRMServer</KeyName></KeyInfo>\
             <CipherData><CipherValue>{wrmdata}</CipherValue></CipherData>\
             </EncryptedKey></KeyInfo>\
             <CipherData><CipherValue>{cdata}</CipherValue></CipherData>\
             </EncryptedData></LA>",
            ns = NS_PROTOCOLS,
            ver = wrm_protocol_version(wrm_header),
            wrm = wrm_header,
            cv = Self::CLIENT_VERSION,
            nonce = B64.encode(nonce),
            now = now,
            enc = NS_XMLENC,
            dsig = NS_XMLDSIG,
            wrmdata = B64.encode(elgamal_encrypt(&xml_key_point, &wmrm)?),
            cdata = B64.encode(self.client_data(session)?),
        );

        let digest = {
            use sha2::Digest;
            sha2::Sha256::digest(la.as_bytes())
        };

        let signed_info = format!(
            "<SignedInfo xmlns=\"{dsig}\">\
             <CanonicalizationMethod Algorithm=\"http://www.w3.org/TR/2001/REC-xml-c14n-20010315\"></CanonicalizationMethod>\
             <SignatureMethod Algorithm=\"{ns}#ecdsa-sha256\"></SignatureMethod>\
             <Reference URI=\"#SignedData\">\
             <DigestMethod Algorithm=\"{ns}#sha256\"></DigestMethod>\
             <DigestValue>{digest}</DigestValue>\
             </Reference></SignedInfo>",
            dsig = NS_XMLDSIG,
            ns = NS_PROTOCOLS,
            digest = B64.encode(digest),
        );

        let signature_value = {
            use p256::ecdsa::{signature::Signer, Signature, SigningKey};
            let sk = SigningKey::from(
                p256::SecretKey::from_bytes(&self.device.signing_key.to_repr())
                    .map_err(|_| err("llave de firma inválida"))?,
            );
            let sig: Signature = sk.sign(signed_info.as_bytes());
            B64.encode(sig.to_bytes())
        };

        let challenge = format!(
            "<AcquireLicense xmlns=\"{ns}\"><challenge><Challenge xmlns=\"{msgs}\">\
             {la}\
             <Signature xmlns=\"{dsig}\">{si}<SignatureValue>{sv}</SignatureValue>\
             <KeyInfo xmlns=\"{dsig}\"><KeyValue><ECCKeyValue><PublicKey>{pk}</PublicKey>\
             </ECCKeyValue></KeyValue></KeyInfo></Signature>\
             </Challenge></challenge></AcquireLicense>",
            ns = NS_PROTOCOLS,
            msgs = NS_MESSAGES,
            la = la,
            dsig = NS_XMLDSIG,
            si = signed_info,
            sv = signature_value,
            pk = B64.encode(&self.signing_public),
        );

        // El SOAP lleva la declaración a mano y el WRMHEADER viaja como XML
        // literal dentro del ContentHeader. Sin las dos cosas el servidor
        // responde DRM_E_SERVER_INVALID_MESSAGE (0x8004C601).
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <soap:Envelope xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" \
             xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\">\
             <soap:Body>{challenge}</soap:Body></soap:Envelope>"
        ))
    }

    /// Saca la llave de contenido de la respuesta SOAP del servidor.
    pub fn parse_license(&self, _session: &Session, soap: &str) -> Result<Vec<u8>> {
        if soap.trim().is_empty() {
            return Err(err("la respuesta de licencia está vacía"));
        }
        if let Some(detail) = extract_tag(soap, "faultstring") {
            return Err(err(format!("el servidor devolvió un SOAP Fault: {detail}")));
        }
        let lic_b64 = extract_tag(soap, "License")
            .ok_or_else(|| err("la respuesta no contiene ninguna licencia"))?;
        let raw = B64
            .decode(lic_b64.trim())
            .map_err(|e| err(format!("la licencia no es base64 válido: {e}")))?;
        let xmr = Xmr::parse(&raw)?;
        let (_kid, key) = xmr.content_key(&self.device.encryption_key, &self.encryption_public)?;
        Ok(key)
    }
}

/// Primer contenido de `<tag>…</tag>`, ignorando prefijos de espacio de nombres.
fn extract_tag(xml: &str, tag: &str) -> Option<String> {
    let open_end = xml.match_indices(&format!("<{tag}>")).next().map(|(i, s)| i + s.len());
    let start = match open_end {
        Some(v) => v,
        None => {
            // con prefijo, p. ej. <s:faultstring>
            let needle = format!(":{tag}>");
            let i = xml.find(&needle)? + needle.len();
            i
        }
    };
    let rest = &xml[start..];
    let end = rest.find(&format!("</{tag}>")).or_else(|| {
        rest.find(&format!(":{tag}>"))
            .map(|i| rest[..i].rfind('<').unwrap_or(i))
    })?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elgamal_ida_y_vuelta() {
        let d = random_scalar();
        let pubp = ProjectivePoint::GENERATOR * d;
        let msg = ProjectivePoint::GENERATOR * random_scalar();
        let cifrado = elgamal_encrypt(&msg, &pubp).unwrap();
        assert_eq!(cifrado.len(), 128, "el ciphertext debe ser de 128 bytes");
        let x = elgamal_decrypt(&cifrado, &d).unwrap();
        assert_eq!(x, point_xy(&msg).unwrap().0, "no se recupera la X");
    }

    #[test]
    fn las_coordenadas_son_siempre_de_32_bytes() {
        // El bug latente de upstream: una X con byte inicial cero salía más corta.
        let (x, y) = point_xy(&(ProjectivePoint::GENERATOR * random_scalar())).unwrap();
        assert_eq!(x.len(), 32);
        assert_eq!(y.len(), 32);
    }

    #[test]
    fn el_punto_del_servidor_wmrm_esta_en_la_curva() {
        point_from_xy(&WMRM_X, &WMRM_Y).expect("la pública del WMRM debe ser válida");
    }

    #[test]
    fn un_prd_con_marca_mala_da_error_claro() {
        let e = Device::parse(&[b'X', b'X', b'X', 3, 0, 0, 0, 0]).unwrap_err().to_string();
        assert!(e.contains("PRD"), "el mensaje no explica el problema: {e}");
    }

    #[test]
    fn la_version_de_protocolo_sale_del_wrmheader() {
        assert_eq!(wrm_protocol_version("<WRMHEADER version=\"4.3.0.0\">"), 5);
        assert_eq!(wrm_protocol_version("<WRMHEADER version=\"4.2.0.0\">"), 4);
        assert_eq!(wrm_protocol_version("<WRMHEADER version=\"4.0.0.0\">"), 1);
    }

    #[test]
    fn del_playready_object_sale_el_wrm_header() {
        // El mismo PRO que sirve Apple en la playlist del tier c2.
        let pro = concat!(
            "vgEAAAEAAQC0ATwAVwBSAE0ASABFAEEARABFAFIAIAB4AG0AbABuAHMAPQAiAGgAdAB0AHAAOgAvAC8A",
            "cwBjAGgAZQBtAGEAcwAuAG0AaQBjAHIAbwBzAG8AZgB0AC4AYwBvAG0ALwBEAFIATQAvADIAMAAwADcA",
            "LwAwADMALwBQAGwAYQB5AFIAZQBhAGQAeQBIAGUAYQBkAGUAcgAiACAAdgBlAHIAcwBpAG8AbgA9ACIA",
            "NAAuADMALgAwAC4AMAAiAD4APABEAEEAVABBAD4APABQAFIATwBUAEUAQwBUAEkATgBGAE8APgA8AEsA",
            "SQBEAFMAPgA8AEsASQBEACAAQQBMAEcASQBEAD0AIgBBAEUAUwBDAEIAQwAiACAAVgBBAEwAVQBFAD0A",
            "IgBBAEEAQQBBAEEASwBWAEsAQgBaAGgAagBNAGkAQQBnAEkAQwBBAGcASQBBAD0APQAiAD4APAAvAEsA",
            "SQBEAD4APAAvAEsASQBEAFMAPgA8AC8AUABSAE8AVABFAEMAVABJAE4ARgBPAD4APAAvAEQAQQBUAEEA",
            "PgA8AC8AVwBSAE0ASABFAEEARABFAFIAPgA="
        );
        let wrm = wrm_header_from_pssh(pro).unwrap();
        assert!(wrm.starts_with("<WRMHEADER"), "no parece un WRMHEADER: {wrm:.40}");
        assert!(wrm.contains("AAAAAKVKBZhjMiAgICAgIA=="), "se perdió el KID");
        assert_eq!(wrm_protocol_version(&wrm), 5);
    }

    #[test]
    fn un_pssh_que_no_es_de_playready_se_rechaza() {
        assert!(wrm_header_from_pssh(&B64.encode([0u8; 40])).is_err());
    }

    #[test]
    fn el_aes_ecb_cuadra_con_el_vector_conocido() {
        // FIPS-197 apéndice C.1.
        let key = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let pt = hex::decode("00112233445566778899aabbccddeeff").unwrap();
        assert_eq!(
            hex::encode(aes_ecb_encrypt(&key, &pt).unwrap()),
            "69c4e0d86a7b0430d8cdb78070b4c55a"
        );
    }

    #[test]
    fn el_cmac_cuadra_con_el_vector_conocido() {
        // RFC 4493, ejemplo 1 (mensaje vacío).
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        assert_eq!(
            hex::encode(aes_cmac(&key, b"").unwrap()),
            "bb1d6929e95937287fa37d129b756746"
        );
    }

    #[test]
    fn el_fault_de_soap_se_reconoce() {
        let fault = "<s:Envelope><s:Body><s:Fault><faultstring>algo se rompió</faultstring>\
                     </s:Fault></s:Body></s:Envelope>";
        assert_eq!(extract_tag(fault, "faultstring").as_deref(), Some("algo se rompió"));
    }
}
