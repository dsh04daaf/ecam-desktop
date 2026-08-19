//! Cliente del wrapper de FairPlay (puerto 10020).
//!
//! El protocolo es binario y sin framing de texto (sacado de `runv2.go`):
//!
//! | Operación     | Bytes                                             |
//! |---------------|---------------------------------------------------|
//! | SwitchKeys    | `00 00 00 00`                                     |
//! | SendString    | `[1 byte largo][cadena]` — adam_id y luego el URI  |
//! | DecryptChunk  | `[u32 LE largo][datos]` → devuelve N bytes         |
//! | Close         | `00 00 00 00 00`                                  |
//!
//! Es **síncrono a propósito**: cada tramo cifrado es una ida y vuelta que hay
//! que esperar, y el orden importa porque el wrapper mantiene el estado de la
//! cadena AES. Envolverlo en async no ganaría nada y escondería esa restricción;
//! el orquestador lo corre dentro de `spawn_blocking`.

use crate::error::{Error, Result, TrackError};
use crate::mp4::frag::Decryptor;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// URI de la llave de prefetch. Cuando aparece, el adam_id que espera el wrapper
/// es `"0"` y no el del track.
pub const PREFETCH_KEY: &str = "skd://itunes.apple.com/P000000000/s1/e1";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Wrapper {
    sock: TcpStream,
    key_ever_sent: bool,
    last_key_uri: Option<String>,
}

impl Wrapper {
    pub fn connect(addr: &str) -> Result<Self> {
        let sock_addr = addr
            .to_socket_addrs()
            .map_err(|_| Error::WrapperUnreachable(addr.to_string()))?
            .next()
            .ok_or_else(|| Error::WrapperUnreachable(addr.to_string()))?;

        let sock = TcpStream::connect_timeout(&sock_addr, CONNECT_TIMEOUT)
            .map_err(|_| Error::WrapperUnreachable(addr.to_string()))?;
        // Sin esto, Nagle mete 40 ms en cada ida y vuelta: con miles de tramos
        // por track eso son minutos regalados.
        sock.set_nodelay(true).ok();
        sock.set_read_timeout(Some(IO_TIMEOUT)).ok();
        sock.set_write_timeout(Some(IO_TIMEOUT)).ok();

        Ok(Self { sock, key_ever_sent: false, last_key_uri: None })
    }

    /// ¿Está vivo el wrapper? Solo abre y cierra: no toca la sesión.
    pub fn probe(addr: &str) -> bool {
        addr.to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok())
            .unwrap_or(false)
    }

    /// Manda la llave **solo si el URI cambió**.
    ///
    /// Esto no es una optimización: mandarla por fragmento hacía que el wrapper
    /// re-pidiera la licencia FairPlay a Apple ~14 veces por track y agotara la
    /// sesión en unos 8 minutos, con la cascada de -42786 detrás. Un track de
    /// Apple Music usa una sola llave para todos sus fragmentos.
    pub fn ensure_key(&mut self, adam_id: &str, key_uri: &str) -> Result<()> {
        if self.last_key_uri.as_deref() == Some(key_uri) {
            return Ok(());
        }
        let id = if key_uri == PREFETCH_KEY { "0" } else { adam_id };

        let mut msg = Vec::with_capacity(2 + id.len() + key_uri.len());
        if self.key_ever_sent {
            msg.extend_from_slice(&[0, 0, 0, 0]); // SwitchKeys
        }
        push_string(&mut msg, id)?;
        push_string(&mut msg, key_uri)?;

        self.sock.write_all(&msg).map_err(dead)?;
        self.key_ever_sent = true;
        self.last_key_uri = Some(key_uri.to_string());
        Ok(())
    }
}

fn push_string(buf: &mut Vec<u8>, s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    if bytes.len() > u8::MAX as usize {
        return Err(Error::Other(format!("cadena demasiado larga para el wrapper: {} bytes", bytes.len())));
    }
    buf.push(bytes.len() as u8);
    buf.extend_from_slice(bytes);
    Ok(())
}

/// Cualquier corte de la conexión con el wrapper es sesión muerta, no un fallo
/// de red del que se pueda reintentar el track: hay que relanzarlo.
fn dead(e: std::io::Error) -> Error {
    Error::Track(TrackError::wrapper_dead(format!("el wrapper cortó la conexión: {e}")))
}

impl Decryptor for Wrapper {
    fn decrypt(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        // El wrapper solo procesa bloques completos de 16 bytes; la cola suelta
        // va en claro y se devuelve tal cual.
        let full = data.len() & !0xF;
        if full == 0 {
            return Ok(data.to_vec());
        }

        let mut req = Vec::with_capacity(4 + full);
        req.extend_from_slice(&(full as u32).to_le_bytes());
        req.extend_from_slice(&data[..full]);
        self.sock.write_all(&req).map_err(dead)?;

        let mut out = vec![0u8; data.len()];
        read_exact_or_dead(&mut self.sock, &mut out[..full])?;
        out[full..].copy_from_slice(&data[full..]);
        Ok(out)
    }
}

fn read_exact_or_dead(sock: &mut TcpStream, buf: &mut [u8]) -> Result<()> {
    let mut pos = 0;
    while pos < buf.len() {
        match sock.read(&mut buf[pos..]) {
            Ok(0) => {
                return Err(Error::Track(TrackError::wrapper_dead(
                    "el wrapper cerró la conexión a mitad de la respuesta",
                )))
            }
            Ok(n) => pos += n,
            Err(e) => return Err(dead(e)),
        }
    }
    Ok(())
}

impl Drop for Wrapper {
    fn drop(&mut self) {
        // Señal de cierre limpia: si no se manda, el wrapper deja el socket
        // colgado y acaba quedándose sin descriptores.
        let _ = self.sock.write_all(&[0, 0, 0, 0, 0]);
    }
}

/// Errores del log del wrapper que significan **sesión muerta**. Reintentar el
/// track con estos no sirve de nada: hay que relanzar el proceso.
pub const FATAL_ERRORS: &[&str] = &[
    "Invalid CKC",
    "catched an exception",
    "Error connecting to device",
    "Error reading response from device",
    "Error writing length to device",
    "-42786",
];

pub fn is_fatal_log(line: &str) -> bool {
    FATAL_ERRORS.iter().any(|e| line.contains(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_los_errores_que_matan_la_sesion() {
        assert!(is_fatal_log("[!] Invalid CKC error"));
        assert!(is_fatal_log("code=-42786"));
        assert!(!is_fatal_log("[+] listening 0.0.0.0:10020"));
    }
}
