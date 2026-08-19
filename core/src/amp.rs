//! Cliente de la API de Apple Music (amp-api) y del bearer token.
//!
//! El bearer no se pide a ningún sitio: se saca del bundle JS de music.apple.com,
//! igual que hace el reproductor web. El `media-user-token` sí es del usuario y
//! es lo que distingue "ver el catálogo" de "poder bajar y leer letras".

use crate::config::Config;
use crate::error::{Error, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
const AMP: &str = "https://amp-api.music.apple.com";
/// 12 h. El token del bundle dura bastante más, pero refrescarlo es barato y
/// evita el fallo silencioso de un token caducado a media descarga larga.
const BEARER_TTL: Duration = Duration::from_secs(43_200);

#[derive(Serialize, Deserialize)]
struct CachedBearer {
    token: String,
    ts: u64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn bearer_cache_path() -> PathBuf {
    Config::config_dir().join("bearer_cache.json")
}

/// Cliente compartido: crear uno por petición tira el pool de conexiones y el
/// handshake TLS se paga en cada segmento.
static HTTP: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent(UA)
        .timeout(Duration::from_secs(60))
        .build()
        .expect("no se pudo construir el cliente HTTP")
});

pub fn http() -> &'static reqwest::Client {
    &HTTP
}

/// Saca el bearer del bundle JS del reproductor web.
///
/// El bundle trae varios JWT; el bueno es el emitido por **AMPWebPlay**. Coger
/// el primero que aparezca funciona casi siempre y falla justo cuando Apple
/// reordena el bundle, así que se busca por emisor.
pub async fn bearer_token() -> Result<String> {
    if let Ok(text) = std::fs::read_to_string(bearer_cache_path()) {
        if let Ok(c) = serde_json::from_str::<CachedBearer>(&text) {
            if now_secs().saturating_sub(c.ts) < BEARER_TTL.as_secs() && !c.token.is_empty() {
                return Ok(c.token);
            }
        }
    }

    let home = HTTP.get("https://music.apple.com").send().await?.text().await?;
    static JS: Lazy<Regex> = Lazy::new(|| Regex::new(r#"/assets/index[^"']+\.js"#).unwrap());
    let path = JS
        .find(&home)
        .ok_or_else(|| Error::Api("no se encontró el bundle JS de Apple Music".into()))?
        .as_str()
        .to_string();

    let js = HTTP.get(format!("https://music.apple.com{path}")).send().await?.text().await?;
    static JWT: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"eyJ[A-Za-z0-9\-_]+\.[A-Za-z0-9\-_]+\.[A-Za-z0-9\-_]+").unwrap());

    let mut fallback = None;
    let mut token = None;
    for m in JWT.find_iter(&js) {
        let cand = m.as_str();
        if fallback.is_none() {
            fallback = Some(cand.to_string());
        }
        if let Some(payload) = cand.split('.').nth(1) {
            use base64::Engine;
            let padded = payload.to_string();
            if let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(padded) {
                if String::from_utf8_lossy(&decoded).contains("AMPWebPlay") {
                    token = Some(cand.to_string());
                    break;
                }
            }
        }
    }

    let token = token
        .or(fallback)
        .ok_or_else(|| Error::Api("no había ningún bearer en el bundle".into()))?;

    let path = bearer_cache_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(&path, serde_json::to_string(&CachedBearer { token: token.clone(), ts: now_secs() }).unwrap_or_default()).ok();
    Ok(token)
}

/// Cliente ya autenticado. Se construye una vez por sesión de la app.
#[derive(Clone)]
pub struct Amp {
    pub bearer: String,
    pub media_user_token: String,
    pub storefront: String,
    pub language: String,
}

impl Amp {
    /// Detecta lo que se pueda y **lo escribe en el config**, para que el
    /// usuario no tenga que teclear nada de esto.
    ///
    /// Qué se guarda y qué no, y por qué:
    ///   * tienda e idioma **sí** se guardan: son estables y así quedan a la
    ///     vista para poder cambiarlos a mano si alguien quiere otra tienda.
    ///   * el token de usuario **no**: caduca y rota. Guardarlo llevaría a que
    ///     un día las letras dejen de salir sin ningún error visible, que es
    ///     justo el tipo de fallo silencioso que queremos evitar. Se pide al
    ///     wrapper en cada arranque, que para eso lo publica.
    pub async fn autoconfigure(cfg: &mut Config) -> Result<Self> {
        // El wrapper ya tiene un token de usuario válido de la sesión iniciada:
        // no hay ninguna razón para que el usuario lo copie de un navegador.
        if cfg.media_user_token.trim().len() < 20 {
            if let Some(tk) = wrapper_music_token(&cfg.decrypt_port).await {
                if tk.trim().len() >= 20 {
                    tracing::info!("token de usuario tomado del wrapper");
                    cfg.media_user_token = tk;
                }
            }
        }

        let me = Self::new(cfg).await?;

        let mut changed = false;
        if cfg.storefront != me.storefront {
            cfg.storefront = me.storefront.clone();
            changed = true;
        }
        if cfg.language != me.language {
            cfg.language = me.language.clone();
            changed = true;
        }
        if changed {
            if let Err(e) = cfg.persist() {
                // Que no se pueda escribir el config no impide descargar.
                tracing::warn!("no se pudo guardar el config detectado: {e}");
            } else {
                tracing::info!("config actualizado: tienda {} · idioma {}", cfg.storefront, cfg.language);
            }
        }
        Ok(me)
    }

    /// Construye el cliente resolviendo tienda e idioma **de la cuenta**.
    ///
    /// El original traía `nz` clavado en el config porque la cuenta es de Nueva
    /// Zelanda, y eso obliga a reconfigurar a mano en cuanto la cuenta cambia (o
    /// se usa otra). Apple lo dice solo en `/v1/me/storefront`, así que se
    /// pregunta: `storefront: auto` (el default) detecta, y poner un código
    /// concreto lo sigue respetando.
    pub async fn new(cfg: &Config) -> Result<Self> {
        let bearer = bearer_token().await?;
        let mut me = Self {
            bearer,
            media_user_token: cfg.media_user_token.clone(),
            storefront: cfg.storefront.clone(),
            language: cfg.language.clone(),
        };

        if cfg.storefront.trim().is_empty() || cfg.storefront.eq_ignore_ascii_case("auto") {
            let (sf, langs) = me.detect_storefront(&cfg.decrypt_port).await;
            me.storefront = sf;
            // La tienda solo acepta ciertos idiomas: pedir uno que no está hace
            // que Apple devuelva la metadata en el idioma por defecto sin avisar,
            // y luego "faltan" las letras sin motivo aparente.
            if !langs.is_empty() && !langs.iter().any(|l| l.eq_ignore_ascii_case(&me.language)) {
                let fallback = langs
                    .iter()
                    .find(|l| l.split('-').next() == me.language.split('-').next())
                    .cloned()
                    .unwrap_or_else(|| langs[0].clone());
                tracing::warn!(
                    "la tienda {} no soporta el idioma {}: se usa {fallback} (soporta {})",
                    me.storefront, me.language, langs.join(", ")
                );
                me.language = fallback;
            }
        }
        Ok(me)
    }

    /// Devuelve (código de tienda, idiomas soportados).
    ///
    /// Tres intentos, de más fiable a menos: la propia cuenta, el `storefront_id`
    /// que ya publica el wrapper, y `us` como último recurso.
    async fn detect_storefront(&self, decrypt_port: &str) -> (String, Vec<String>) {
        if self.media_user_token.trim().len() >= 20 {
            if let Ok(v) = self.get("/v1/me/storefront", &[], true).await {
                if let Some(id) = v["data"][0]["id"].as_str() {
                    let langs = v["data"][0]["attributes"]["supportedLanguageTags"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    tracing::info!("tienda detectada por la cuenta: {id}");
                    return (id.to_string(), langs);
                }
            }
        }
        if let Some(acct) = wrapper_account(decrypt_port).await {
            if let Some(raw) = acct["storefront_id"].as_str() {
                // Llega como "143461-27,31": el número de delante es la tienda.
                let numeric = raw.split(['-', ',']).next().unwrap_or("");
                if let Some(code) = storefront_code(numeric) {
                    tracing::info!("tienda detectada por el wrapper: {code} ({numeric})");
                    return (code.to_string(), Vec::new());
                }
                tracing::warn!("el wrapper reporta la tienda {numeric}, que no está en la tabla");
            }
        }
        tracing::warn!("no se pudo detectar la tienda de la cuenta: se usa us");
        ("us".to_string(), Vec::new())
    }

    fn headers(&self, with_user: bool) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, ORIGIN};
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", self.bearer)) {
            h.insert(AUTHORIZATION, v);
        }
        h.insert(ORIGIN, HeaderValue::from_static("https://music.apple.com"));
        if with_user && !self.media_user_token.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&self.media_user_token) {
                h.insert("Music-User-Token", v);
            }
        }
        h
    }

    pub(crate) async fn get(&self, path: &str, params: &[(&str, &str)], with_user: bool) -> Result<Value> {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{AMP}{path}") };
        let r = HTTP.get(url).headers(self.headers(with_user)).query(params).send().await?;
        match r.status().as_u16() {
            200 => Ok(r.json().await?),
            404 => Err(Error::NotFound),
            401 | 403 => Err(Error::NeedsUserToken),
            code => Err(Error::Api(format!("amp-api respondió {code}"))),
        }
    }

    /// Sigue el `next` de una relación hasta agotarla. Sin esto, las playlists
    /// y los álbumes largos se cortan en la primera página y nadie se entera.
    async fn paginate(&self, rel: &Value, extra: &[(&str, &str)]) -> Result<Vec<Value>> {
        let mut out: Vec<Value> = rel
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut next = rel.get("next").and_then(Value::as_str).map(String::from);
        while let Some(n) = next {
            let page = self.get(&format!("{AMP}{n}"), extra, false).await?;
            if let Some(arr) = page.get("data").and_then(Value::as_array) {
                out.extend(arr.iter().cloned());
            }
            next = page.get("next").and_then(Value::as_str).map(String::from);
        }
        Ok(out)
    }

    pub async fn album(&self, id: &str) -> Result<Value> {
        let mut data = self
            .get(
                &format!("/v1/catalog/{}/albums/{id}", self.storefront),
                &[
                    ("omit[resource]", "autos"),
                    ("include", "tracks,artists,record-labels"),
                    ("include[songs]", "artists"),
                    ("extend", "extendedAssetUrls,editorialVideo"),
                    ("l", &self.language),
                ],
                false,
            )
            .await?;

        let rel = data["data"][0]["relationships"]["tracks"].clone();
        let tracks = self
            .paginate(&rel, &[("omit[resource]", "autos"), ("include", "artists"), ("extend", "extendedAssetUrls")])
            .await?;
        data["data"][0]["relationships"]["tracks"]["data"] = Value::Array(tracks);
        Ok(data)
    }

    pub async fn song(&self, id: &str) -> Result<Value> {
        self.get(
            &format!("/v1/catalog/{}/songs/{id}", self.storefront),
            &[("include", "albums,artists"), ("extend", "extendedAssetUrls"), ("l", &self.language)],
            false,
        )
        .await
    }

    pub async fn music_video(&self, id: &str) -> Result<Value> {
        self.get(
            &format!("/v1/catalog/{}/music-videos/{id}", self.storefront),
            &[("include", "artists"), ("extend", "extendedAssetUrls"), ("l", &self.language)],
            false,
        )
        .await
    }

    /// Devuelve (nombre, tracks) de una playlist, ya paginada.
    pub async fn playlist(&self, id: &str) -> Result<(String, Vec<Value>)> {
        let data = self
            .get(
                &format!("/v1/catalog/{}/playlists/{id}", self.storefront),
                &[("include", "tracks"), ("extend", "extendedAssetUrls"), ("l", &self.language)],
                false,
            )
            .await?;
        let item = &data["data"][0];
        let name = item["attributes"]["name"].as_str().unwrap_or(id).to_string();
        let tracks = self
            .paginate(&item["relationships"]["tracks"], &[("extend", "extendedAssetUrls"), ("l", &self.language)])
            .await?;
        Ok((name, tracks))
    }

    /// Devuelve (nombre del artista, ids de sus álbumes).
    pub async fn artist_albums(&self, id: &str) -> Result<(String, Vec<String>)> {
        let data = self
            .get(
                &format!("/v1/catalog/{}/artists/{id}", self.storefront),
                &[("include", "albums"), ("l", &self.language)],
                false,
            )
            .await?;
        let item = &data["data"][0];
        let name = item["attributes"]["name"].as_str().unwrap_or(id).to_string();
        let albums = self.paginate(&item["relationships"]["albums"], &[]).await?;
        let ids = albums
            .iter()
            .filter_map(|a| a["id"].as_str().map(String::from))
            .collect();
        Ok((name, ids))
    }

    /// Una "room" editorial (páginas curadas). Vive bajo `/editorial`, no bajo
    /// `/catalog`, y necesita `platform=web` también al paginar.
    pub async fn room(&self, id: &str) -> Result<(String, Vec<(String, String)>)> {
        let data = self
            .get(
                &format!("/v1/editorial/{}/rooms/{id}", self.storefront),
                &[("platform", "web"), ("include", "contents"), ("l", &self.language)],
                false,
            )
            .await?;
        let item = &data["data"][0];
        let title = item["attributes"]["title"].as_str().unwrap_or(id).to_string();
        let contents = self.paginate(&item["relationships"]["contents"], &[("platform", "web")]).await?;
        let items = contents
            .iter()
            .filter_map(|it| {
                Some((it["type"].as_str()?.to_string(), it["id"].as_str()?.to_string()))
            })
            .collect();
        Ok((title, items))
    }

    /// Búsqueda en el catálogo. `types` fija el orden en que Apple los devuelve.
    pub async fn search(&self, term: &str, limit: u32) -> Result<Value> {
        let limit = limit.clamp(1, 25).to_string();
        self.get(
            &format!("/v1/catalog/{}/search", self.storefront),
            &[
                ("term", term),
                ("types", "albums,songs,music-videos,playlists,artists"),
                ("limit", &limit),
                ("l", &self.language),
            ],
            false,
        )
        .await
    }

    /// Rellena el bit depth y el sample rate reales del álbum abierto.
    ///
    /// Las pistas de un mismo álbum comparten formato casi siempre, así que se
    /// mira UNA (la primera con stream) y se aplica a las demás — el bot hace lo
    /// mismo. Una llamada por álbum en vez de una por pista.
    pub async fn resolve_qualities(&self, browse: &mut Browse) {
        if browse.kind == "artist" {
            return;
        }
        let Some(first) = browse.items.iter().find(|i| i.playable).map(|i| i.id.clone()) else { return };

        let Ok(song) = self.song(&first).await else { return };
        let Some(hls) = song["data"][0]["attributes"]["extendedAssetUrls"]["enhancedHls"].as_str() else { return };
        let Ok(resp) = http().get(hls).header("User-Agent", UA).send().await else { return };
        let Ok(master) = resp.text().await else { return };

        let q = crate::hls::parse_qualities(&master);
        for item in browse.items.iter_mut().filter(|i| i.playable) {
            let mut copia = q.clone();
            // El Atmos sí varía pista a pista dentro de un mismo álbum, y eso lo
            // dice el catálogo sin pedir nada más.
            copia.atmos = item.traits.iter().any(|t| t.contains("atmos") || t.contains("spatial"));
            item.quality = Some(copia);
        }
    }

    /// Letras. Se intenta primero línea a línea y luego sílaba a sílaba, que es
    /// el orden en el que Apple las publica.
    pub async fn lyrics_ttml(&self, song_id: &str) -> Result<Option<String>> {
        if self.media_user_token.trim().len() < 20 {
            return Ok(None); // sin token de usuario no hay letras, y no es un error
        }
        for kind in ["lyrics", "syllable-lyrics"] {
            let path = format!("/v1/catalog/{}/songs/{song_id}/{kind}", self.storefront);
            match self.get(&path, &[("l", &self.language)], true).await {
                Ok(v) => {
                    if let Some(ttml) = v["data"][0]["attributes"]["ttml"].as_str() {
                        return Ok(Some(ttml.to_string()));
                    }
                }
                Err(Error::NotFound) | Err(Error::NeedsUserToken) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }
}

/// Un resultado de búsqueda, ya normalizado para la UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    /// `album`, `song`, `music-video`, `playlist` o `artist`.
    pub kind: String,
    pub name: String,
    pub artist: String,
    pub artwork: String,
    /// Bajar esto de una pulsación puede ser una discografía entera: la UI
    /// pregunta antes.
    pub bulk: bool,
}

/// Aplana la respuesta de búsqueda. **Un solo sitio**: la app y la vista previa
/// tienen que enseñar exactamente lo mismo, o probar una no dice nada de la otra.
pub fn search_hits(v: &Value) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for kind in ["albums", "songs", "music-videos", "playlists", "artists"] {
        let Some(items) = v["results"][kind]["data"].as_array() else { continue };
        for it in items {
            let a = &it["attributes"];
            let singular = kind.trim_end_matches('s');
            out.push(SearchHit {
                id: it["id"].as_str().unwrap_or_default().to_string(),
                kind: if kind == "music-videos" { "music-video".into() } else { singular.to_string() },
                name: a["name"].as_str().unwrap_or_default().to_string(),
                artist: a["artistName"].as_str().unwrap_or_default().to_string(),
                artwork: a["artwork"]["url"]
                    .as_str()
                    .unwrap_or_default()
                    .replace("{w}", "300")
                    .replace("{h}", "300"),
                bulk: matches!(kind, "artists" | "playlists"),
            });
        }
    }
    out
}

/// Un elemento dentro de una entidad abierta (un track de un álbum, un álbum de
/// un artista…).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseItem {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub artist: String,
    pub extra: String,
    pub artwork: String,
    /// `audioTraits` del catálogo: lossless, hi-res-lossless, atmos, spatial.
    /// Es lo barato: viene con la metadata del álbum, sin llamadas extra.
    #[serde(default)]
    pub traits: Vec<String>,
    /// Bit depth y sample rate REALES, del master playlist. Solo se resuelve
    /// cuando hace falta, porque cuesta una llamada.
    #[serde(default)]
    pub quality: Option<crate::hls::TrackQuality>,
    /// Si no tiene stream, no se ofrece bajarlo.
    #[serde(default = "yes")]
    pub playable: bool,
}

fn yes() -> bool {
    true
}

/// Lo que hay dentro de una entidad, para poder navegarla en vez de solo bajarla.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Browse {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub artist: String,
    pub artwork: String,
    pub items: Vec<BrowseItem>,
}

fn dur(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

impl Amp {
    /// Abre una entidad y devuelve lo que contiene.
    ///
    /// Sin esto la app solo sabía buscar y bajar a ciegas: no se podía ver qué
    /// trae un álbum ni qué publicó un artista.
    pub async fn browse(&self, kind: &str, id: &str) -> Result<Browse> {
        match kind {
            "album" => {
                let data = self.album(id).await?;
                let a = &data["data"][0]["attributes"];
                let items = data["data"][0]["relationships"]["tracks"]["data"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .map(|t| {
                                let ta = &t["attributes"];
                                BrowseItem {
                                    id: t["id"].as_str().unwrap_or_default().into(),
                                    kind: "song".into(),
                                    name: ta["name"].as_str().unwrap_or_default().into(),
                                    artist: ta["artistName"].as_str().unwrap_or_default().into(),
                                    extra: dur(ta["durationInMillis"].as_u64().unwrap_or(0)),
                                    artwork: String::new(),
                                    traits: ta["audioTraits"].as_array().map(|a| {
                                        a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
                                    }).unwrap_or_default(),
                                    quality: None,
                                    playable: ta["extendedAssetUrls"]["enhancedHls"].is_string()
                                        || ta["playParams"]["id"].is_string(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(Browse {
                    kind: "album".into(),
                    id: id.into(),
                    name: a["name"].as_str().unwrap_or_default().into(),
                    artist: a["artistName"].as_str().unwrap_or_default().into(),
                    artwork: a["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "400").replace("{h}", "400"),
                    items,
                })
            }
            "playlist" => {
                let (name, tracks) = self.playlist(id).await?;
                let artwork = tracks
                    .first()
                    .and_then(|t| t["attributes"]["artwork"]["url"].as_str())
                    .unwrap_or_default()
                    .replace("{w}", "400")
                    .replace("{h}", "400");
                let items = tracks
                    .iter()
                    .map(|t| {
                        let ta = &t["attributes"];
                        BrowseItem {
                            id: t["id"].as_str().unwrap_or_default().into(),
                            kind: "song".into(),
                            name: ta["name"].as_str().unwrap_or_default().into(),
                            artist: ta["artistName"].as_str().unwrap_or_default().into(),
                            extra: dur(ta["durationInMillis"].as_u64().unwrap_or(0)),
                            artwork: String::new(),
                            traits: ta["audioTraits"].as_array().map(|a| {
                                a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
                            }).unwrap_or_default(),
                            quality: None,
                            playable: ta["extendedAssetUrls"]["enhancedHls"].is_string()
                                || ta["playParams"]["id"].is_string(),
                        }
                    })
                    .collect();
                Ok(Browse { kind: "playlist".into(), id: id.into(), name, artist: String::new(), artwork, items })
            }
            "artist" => {
                // Se piden los álbumes con su metadata, no solo los ids: si no,
                // la vista del artista sería una lista de números.
                let data = self
                    .get(
                        &format!("/v1/catalog/{}/artists/{id}", self.storefront),
                        &[("include", "albums"), ("l", &self.language)],
                        false,
                    )
                    .await?;
                let item = &data["data"][0];
                let rel = &item["relationships"]["albums"];
                let albums = self.paginate(rel, &[("l", &self.language)]).await?;
                let items = albums
                    .iter()
                    .map(|al| {
                        let aa = &al["attributes"];
                        BrowseItem {
                            id: al["id"].as_str().unwrap_or_default().into(),
                            kind: "album".into(),
                            name: aa["name"].as_str().unwrap_or_default().into(),
                            artist: aa["artistName"].as_str().unwrap_or_default().into(),
                            extra: aa["releaseDate"].as_str().unwrap_or_default().chars().take(4).collect(),
                            artwork: aa["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "300").replace("{h}", "300"),
                            traits: aa["audioTraits"].as_array().map(|a| {
                                a.iter().filter_map(|x| x.as_str().map(String::from)).collect()
                            }).unwrap_or_default(),
                            quality: None,
                            playable: true,
                        }
                    })
                    .collect();
                Ok(Browse {
                    kind: "artist".into(),
                    id: id.into(),
                    name: item["attributes"]["name"].as_str().unwrap_or_default().into(),
                    artist: String::new(),
                    artwork: item["attributes"]["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "400").replace("{h}", "400"),
                    items,
                })
            }
            other => Err(Error::Other(format!("no se puede abrir un {other}"))),
        }
    }
}

/// El token de música que tiene el wrapper (puerto 30020). Hace falta para el
/// `webPlayback` de los tracks legacy y para los music videos.
pub async fn wrapper_music_token(decrypt_port: &str) -> Option<String> {
    let host = decrypt_port.rsplit_once(':').map(|(h, _)| h).unwrap_or("127.0.0.1");
    let v: Value = HTTP
        .get(format!("http://{host}:30020/"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("music_token").and_then(Value::as_str).map(String::from)
}

/// Info de la cuenta que sirve el wrapper: sirve para la pantalla de sesión.
pub async fn wrapper_account(decrypt_port: &str) -> Option<Value> {
    let host = decrypt_port.rsplit_once(':').map(|(h, _)| h).unwrap_or("127.0.0.1");
    HTTP.get(format!("http://{host}:30020/"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

/// Los identificadores numéricos de tienda de Apple. Solo hacen falta para
/// traducir lo que reporta el wrapper cuando no hay `media-user-token`.
pub fn storefront_code(id: &str) -> Option<&'static str> {
    Some(match id {
        "143441" => "us", "143442" => "fr", "143443" => "de", "143444" => "gb",
        "143445" => "at", "143446" => "be", "143447" => "fi", "143448" => "gr",
        "143449" => "ie", "143450" => "it", "143451" => "lu", "143452" => "nl",
        "143453" => "pt", "143454" => "es", "143455" => "ca", "143456" => "se",
        "143457" => "no", "143458" => "dk", "143459" => "ch", "143460" => "au",
        "143461" => "nz", "143462" => "jp", "143463" => "hk", "143464" => "sg",
        "143465" => "cn", "143466" => "kr", "143467" => "in", "143468" => "mx",
        "143469" => "ru", "143470" => "tw", "143471" => "vn", "143472" => "za",
        "143473" => "my", "143474" => "ph", "143475" => "th", "143476" => "id",
        "143477" => "pk", "143478" => "pl", "143479" => "sa", "143480" => "tr",
        "143481" => "ae", "143482" => "hu", "143483" => "cl", "143484" => "il",
        "143485" => "za", "143486" => "co", "143487" => "cr", "143489" => "ar",
        "143495" => "br", "143501" => "pe", "143502" => "do", "143503" => "ec",
        "143504" => "gt", "143505" => "hn", "143506" => "jm", "143508" => "ni",
        "143509" => "pa", "143510" => "py", "143511" => "sv", "143512" => "uy",
        "143513" => "ve", "143523" => "ro", "143524" => "cz", "143525" => "sk",
        "143537" => "ua", "143538" => "kz",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traduce_el_id_numerico_del_wrapper() {
        assert_eq!(storefront_code("143461"), Some("nz"));
        assert_eq!(storefront_code("143441"), Some("us"));
        assert_eq!(storefront_code("999999"), None);
    }
}
