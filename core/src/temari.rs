//! Descifrado FairPlay **en este proceso**, con el wrapper fuera de la ruta de datos.
//!
//! El camino de siempre ([`crate::wrapper`]) manda cada byte cifrado por un
//! socket TCP abierto durante toda la pista: si el wrapper tropieza a la mitad,
//! la descarga se parte. Aquí sólo le pedimos la **plantilla de descifrado** por
//! HTTP a su key server (40020) y el AES lo hacemos nosotros con
//! [Temari](https://github.com/WorldObservationLog/Temari) (MIT).
//!
//! La plantilla son tres cosas, capturadas por el wrapper con un hook en la
//! entrada de la ronda R1 de `libCoreLSKD`: el contexto (32 KB), el estado
//! inicial (8 KB) y los registros de entrada. Temari reinicia el estado desde la
//! plantilla en **cada** llamada, así que descifrar es sin estado y el troceado
//! por muestras es idéntico al del wrapper: se descifra el prefijo alineado a 16
//! y la cola suelta pasa tal cual.
//!
//! **Red fuera del hilo bloqueante.** `decrypt_to_file` corre dentro de un
//! `spawn_blocking`, y los URIs de llave de una pista se conocen de antemano
//! (vienen del m3u8). Por eso [`TemariDecrypter::prepare`] pide todas las
//! plantillas distintas *antes*, en async, y luego `ensure_key` sólo cambia cuál
//! está activa, sin tocar la red.
//!
//! **Una licencia por pista.** El prefetch (`adamId=0`) reutiliza el contexto ya
//! calentado del wrapper, así que no gasta licencia; la de contenido gasta una.
//! Mismo consumo que el camino antiguo — que es justo lo que hay que cuidar: en
//! junio, remandar la llave por fragmento disparó las peticiones a ~14 por pista
//! y agotó la sesión FairPlay.

use crate::error::{Error, Result, TrackError};
use crate::mp4::frag::Decryptor;
use crate::wrapper::{KeyedDecryptor, PREFETCH_KEY};
use std::collections::HashMap;
use temari::rounds::{self, Template};
use temari::template::template_from_json;

/// Descifrador local. Lleva ya cargadas las plantillas de la pista.
pub struct TemariDecrypter {
    /// key_uri -> plantilla ya parseada.
    plantillas: HashMap<String, Template>,
    activa: Option<String>,
}

impl TemariDecrypter {
    /// Pide al key server las plantillas de todos los URIs distintos de la pista.
    ///
    /// `addr` es el `host:puerto` del wrapper (se le cambia el puerto por
    /// `key_port`, que es donde escucha el key server).
    pub async fn prepare(
        addr: &str,
        key_port: u16,
        adam_id: &str,
        key_uris: &[Option<String>],
    ) -> Result<Self> {
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        let base = format!("http://{host}:{key_port}/");

        let mut distintos: Vec<&str> = Vec::new();
        for u in key_uris.iter().flatten() {
            if !distintos.iter().any(|d| *d == u.as_str()) {
                distintos.push(u.as_str());
            }
        }
        if distintos.is_empty() {
            return Err(Error::Other("la pista no trae ningún URI de llave".into()));
        }

        let cliente = reqwest::Client::new();
        let mut plantillas = HashMap::new();
        for uri in distintos {
            // Con el URI de prefetch el wrapper espera "0", no el adam de la pista.
            let id = if uri == PREFETCH_KEY { "0" } else { adam_id };
            let url = format!(
                "{base}?adamId={}&uri={}",
                urlencode(id),
                urlencode(uri)
            );
            let resp = cliente
                .get(&url)
                .send()
                .await
                .map_err(|e| muerto(format!("el key server no responde: {e}")))?;
            if !resp.status().is_success() {
                return Err(muerto(format!(
                    "el key server devolvió {} para {id}",
                    resp.status()
                )));
            }
            let cuerpo = resp
                .text()
                .await
                .map_err(|e| muerto(format!("no se pudo leer la plantilla: {e}")))?;
            // Sin ctx/state no hay plantilla: suele significar que el hook R1 no
            // se disparó en el wrapper. Es un fallo de sesión, no de red.
            if !cuerpo.contains("\"ctx\"") || !cuerpo.contains("\"state\"") {
                return Err(muerto(format!(
                    "plantilla incompleta para {id}: el wrapper no capturó ctx/state"
                )));
            }
            let t = template_from_json(&cuerpo)
                .map_err(|e| muerto(format!("plantilla inválida para {id}: {e}")))?;
            plantillas.insert(uri.to_string(), t);
        }

        Ok(Self { plantillas, activa: None })
    }

    /// ¿Hay key server escuchando? Para decidir si se puede usar este motor.
    pub async fn probe(addr: &str, key_port: u16) -> bool {
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        tokio::net::TcpStream::connect((host, key_port)).await.is_ok()
    }

    fn activa(&self) -> Result<&Template> {
        let uri = self
            .activa
            .as_deref()
            .ok_or_else(|| Error::Other("descifrado sin llave seleccionada".into()))?;
        self.plantillas
            .get(uri)
            .ok_or_else(|| Error::Other(format!("no hay plantilla para {uri}")))
    }
}

impl KeyedDecryptor for TemariDecrypter {
    /// No hay red aquí: las plantillas ya están cargadas, sólo se cambia la activa.
    fn ensure_key(&mut self, _adam_id: &str, key_uri: &str) -> Result<()> {
        if self.activa.as_deref() == Some(key_uri) {
            return Ok(());
        }
        if !self.plantillas.contains_key(key_uri) {
            return Err(Error::Other(format!(
                "el m3u8 trae un URI de llave que no se pidió al preparar: {key_uri}"
            )));
        }
        self.activa = Some(key_uri.to_string());
        Ok(())
    }
}

impl Decryptor for TemariDecrypter {
    fn decrypt(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        // Mismo contrato que el wrapper: sólo bloques completos de 16 bytes; la
        // cola suelta va en claro. Temari ya lo hace igual, pero el atajo evita
        // el viaje por la FFI cuando no hay nada que descifrar.
        if data.len() & !0xF == 0 {
            return Ok(data.to_vec());
        }
        Ok(rounds::decrypt(self.activa()?, data))
    }

    /// La tanda va en paralelo. Con el wrapper esto era imposible: su puerto de
    /// descifrado atiende a un cliente cada vez.
    fn decrypt_batch(&mut self, ranges: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        if ranges.is_empty() {
            return Ok(Vec::new());
        }
        let t = self.activa()?;
        Ok(rounds::decrypt_par(t, ranges))
    }
}

/// Un fallo del key server deja la pista sin llave: es sesión muerta, igual que
/// cuando el wrapper corta el socket, y el orquestador debe relanzarlo.
fn muerto(msg: String) -> Error {
    Error::Track(TrackError::wrapper_dead(msg))
}

/// Codifica lo justo para una query: los URI de llave llevan `:` y `/`.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codifica_los_uri_de_llave() {
        assert_eq!(
            urlencode("skd://itunes.apple.com/p1/c6"),
            "skd%3A%2F%2Fitunes.apple.com%2Fp1%2Fc6"
        );
        assert_eq!(urlencode("0"), "0");
    }

    #[test]
    fn sin_llave_seleccionada_no_descifra() {
        let mut d = TemariDecrypter { plantillas: HashMap::new(), activa: None };
        // 16 bytes para que no entre por el atajo de la cola suelta.
        assert!(d.decrypt(&[0u8; 16]).is_err());
    }

    #[test]
    fn la_cola_suelta_pasa_tal_cual_sin_llave() {
        // Menos de 16 bytes no se descifra, así que ni siquiera necesita llave:
        // mismo contrato que el wrapper.
        let mut d = TemariDecrypter { plantillas: HashMap::new(), activa: None };
        assert_eq!(d.decrypt(&[1, 2, 3]).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn un_uri_no_preparado_es_error_y_no_silencio() {
        // Si el m3u8 trae una llave que no se pidió al preparar, hay que fallar:
        // seguir con la plantilla anterior daría audio corrupto sin avisar.
        let mut d = TemariDecrypter { plantillas: HashMap::new(), activa: None };
        assert!(d.ensure_key("123", "skd://itunes.apple.com/p1/c6").is_err());
    }
}
