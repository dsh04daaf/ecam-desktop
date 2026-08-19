//! La "card" que se enseña antes de bajar: qué es, si se puede bajar, en qué
//! calidad, y qué avisos hay.
//!
//! Es el equivalente de lo que hace el bot al pegarle un link. Los avisos no son
//! adorno: son los casos que el bot fue descubriendo en producción —
//! disponibilidad parcial en la tienda de la cuenta, lossless que no está y
//! acaba bajando AAC, Atmos que no viene marcado en el catálogo, y la versión
//! del álbum que sí existe cuando la del link no.

use crate::amp::Amp;
use crate::collection::{parse_url, Target};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Todo el contenido tiene stream en la tienda de la cuenta.
    Available,
    /// Está, pero le faltan pistas.
    Partial,
    /// No está en la tienda de la cuenta.
    Unavailable,
}

/// Un aviso para el usuario, antes de que le dé a bajar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    /// `partial`, `no_lossless`, `no_atmos`, `other_version`, `no_animated`.
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork: String,
    /// La carátula al tamaño que Apple reporta (3000x3000, 6000x6000…), para
    /// poder ofrecerla tal cual como hace la card del bot.
    pub artwork_hq: String,
    pub track_count: usize,
    pub availability: Availability,
    pub reason: String,
    /// `audioTraits` tal cual los publica Apple: lossless, hi-res-lossless,
    /// atmos, spatial…
    pub qualities: Vec<String>,
    pub has_lossless: bool,
    pub has_atmos: bool,
    pub has_animated_artwork: bool,
    /// Nombres de las pistas sin stream, para poder decir cuáles faltan.
    pub missing: Vec<String>,
    pub warnings: Vec<Warning>,
    /// Otras ediciones del mismo álbum que SÍ están, cuando la del link no.
    pub alternatives: Vec<Alternative>,
    /// Calidad del vídeo, si es un music video: 4K, HDR o HD.
    pub video_quality: Vec<String>,
    /// Bit depth y sample rate REALES, leídos del master playlist. Es lo que la
    /// card enseña como "ALAC [24B-44.1kHz] · AAC [256kbps]".
    pub real_quality: Option<crate::hls::TrackQuality>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alternative {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub year: String,
    pub artwork: String,
}

/// Nombre comparable: sin mayúsculas, sin puntuación y sin los sufijos de
/// edición, que es justo lo que distingue una versión de otra.
fn normalize(s: &str) -> String {
    let base = s.to_lowercase();
    let base = base
        .replace(" - single", "")
        .replace(" - ep", "")
        .replace("(deluxe)", "")
        .replace("(remastered)", "");
    base.chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect::<String>().trim().to_string()
}

/// El trocito legible de una URL de Apple Music: `.../album/<slug>/<id>`.
fn slug_of(url: &str) -> String {
    url.split('/')
        .rev()
        .nth(1)
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn traits_of(attrs: &Value) -> Vec<String> {
    attrs["audioTraits"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Una pista es descargable si trae `extendedAssetUrls.enhancedHls` o, al menos,
/// `playParams` (que es lo que mira el bot para el catálogo viejo).
fn track_playable(attrs: &Value) -> bool {
    attrs["extendedAssetUrls"]["enhancedHls"].is_string() || attrs["playParams"]["id"].is_string()
}

impl Amp {
    /// Arma la card de un link pegado.
    pub async fn preview(&self, url: &str) -> Result<Preview> {
        let mut p = self.preview_inner(url).await?;
        self.fill_quality_and_art(&mut p).await;
        Ok(p)
    }

    /// Rellena la carátula a resolución nativa y las calidades reales.
    ///
    /// Es una llamada más, pero es la diferencia entre "lossless" a secas y
    /// "ALAC [24B-44.1kHz]", que es lo que de verdad quiere saber quien va a
    /// bajar algo.
    async fn fill_quality_and_art(&self, p: &mut Preview) {
        if !p.artwork.is_empty() {
            // La plantilla es la misma; solo cambia el tamaño pedido.
            p.artwork_hq = p.artwork.replace("400x400", "3000x3000");
        }
        if p.availability == Availability::Unavailable || p.kind == "artist" || p.kind == "room" {
            return;
        }
        let track_id = match p.kind.as_str() {
            "song" => Some(p.id.clone()),
            "album" | "playlist" => {
                let Ok(b) = self.browse(&p.kind, &p.id).await else { return };
                b.items.iter().find(|i| i.playable).map(|i| i.id.clone())
            }
            _ => None,
        };
        let Some(id) = track_id else { return };
        let Ok(song) = self.song(&id).await else { return };
        let Some(hls) = song["data"][0]["attributes"]["extendedAssetUrls"]["enhancedHls"].as_str() else { return };
        let Ok(resp) = crate::amp::http().get(hls).header("User-Agent", crate::amp::UA).send().await else { return };
        let Ok(master) = resp.text().await else { return };

        let mut q = crate::hls::parse_qualities(&master);
        q.atmos = p.has_atmos;
        p.real_quality = Some(q);
    }

    async fn preview_inner(&self, url: &str) -> Result<Preview> {
        let target = parse_url(url).ok_or_else(|| Error::Other(format!("URL no reconocida: {url}")))?;
        match target {
            Target::Album { id, only_song, .. } => {
                self.preview_album_named(&id, only_song.as_deref(), &slug_of(url)).await
            }
            Target::Song { id, .. } => self.preview_song(&id).await,
            Target::MusicVideo { id, .. } => self.preview_mv(&id).await,
            Target::Playlist { id, .. } => {
                let (name, tracks) = self.playlist(&id).await?;
                let missing: Vec<String> = tracks
                    .iter()
                    .filter(|t| !track_playable(&t["attributes"]))
                    .filter_map(|t| t["attributes"]["name"].as_str().map(String::from))
                    .collect();
                let artwork = tracks
                    .first()
                    .and_then(|t| t["attributes"]["artwork"]["url"].as_str())
                    .unwrap_or_default()
                    .replace("{w}", "400")
                    .replace("{h}", "400");
                let mut p = Preview {
                    kind: "playlist".into(), id, title: name, artist: String::new(),
                    album: String::new(), artwork, artwork_hq: String::new(),
                    track_count: tracks.len(),
                    availability: Availability::Available, reason: String::new(),
                    qualities: vec![], has_lossless: false, has_atmos: false,
                    has_animated_artwork: false, missing, warnings: vec![],
                    alternatives: vec![], video_quality: vec![],
                    real_quality: None,
                };
                p.finish_partial();
                Ok(p)
            }
            Target::Artist { id, .. } => {
                let (name, albums) = self.artist_albums(&id).await?;
                Ok(Preview {
                    kind: "artist".into(), id, title: name, artist: String::new(),
                    album: String::new(), artwork: String::new(), track_count: albums.len(),
                    availability: Availability::Available,
                    reason: format!("{} álbumes", albums.len()),
                    qualities: vec![], has_lossless: false, has_atmos: false,
                    has_animated_artwork: false, missing: vec![], warnings: vec![],
                    alternatives: vec![], video_quality: vec![],
                    artwork_hq: String::new(),
                    real_quality: None,
                })
            }
            Target::Room { id, .. } => {
                let (title, items) = self.room(&id).await?;
                Ok(Preview {
                    kind: "room".into(), id, title, artist: String::new(), album: String::new(),
                    artwork: String::new(), track_count: items.len(),
                    availability: Availability::Available,
                    reason: format!("{} elementos", items.len()),
                    qualities: vec![], has_lossless: false, has_atmos: false,
                    has_animated_artwork: false, missing: vec![], warnings: vec![],
                    alternatives: vec![], video_quality: vec![],
                    artwork_hq: String::new(),
                    real_quality: None,
                })
            }
        }
    }

    /// `slug` es el trocito legible de la URL (`turn-up-the-bass-single`).
    /// Cuando el álbum no está en la tienda no hay metadata de la que sacar el
    /// nombre, así que sin él no se podría buscar ninguna otra edición.
    async fn preview_album_named(&self, album_id: &str, only_song: Option<&str>, slug: &str) -> Result<Preview> {
        let data = match self.album(album_id).await {
            Ok(d) => d,
            // No está en la tienda de la cuenta: en vez de un "404" seco, se
            // buscan otras ediciones que SÍ estén. Es lo que hace el bot.
            Err(Error::NotFound) => {
                let nombre = slug.replace('-', " ");
                let alternatives = self.other_versions(&nombre, "", album_id).await;
                return Ok(Preview {
                    alternatives,
                    kind: "album".into(), id: album_id.into(),
                    title: String::new(), artist: String::new(), album: String::new(),
                    artwork: String::new(), track_count: 0,
                    availability: Availability::Unavailable,
                    reason: format!("No está en el catálogo de {}", self.storefront.to_uppercase()),
                    qualities: vec![], has_lossless: false, has_atmos: false,
                    has_animated_artwork: false, missing: vec![], warnings: vec![],
                    video_quality: vec![],
                    artwork_hq: String::new(),
                    real_quality: None,
                })
            }
            Err(e) => return Err(e),
        };

        let item = &data["data"][0];
        let attrs = &item["attributes"];
        let tracks: Vec<Value> = item["relationships"]["tracks"]["data"].as_array().cloned().unwrap_or_default();
        let selected: Vec<&Value> = match only_song {
            Some(sid) => tracks.iter().filter(|t| t["id"].as_str() == Some(sid)).collect(),
            None => tracks.iter().collect(),
        };

        let missing: Vec<String> = selected
            .iter()
            .filter(|t| !track_playable(&t["attributes"]))
            .filter_map(|t| t["attributes"]["name"].as_str().map(String::from))
            .collect();

        let qualities = traits_of(attrs);
        let mut p = Preview {
            kind: "album".into(),
            id: album_id.into(),
            title: attrs["name"].as_str().unwrap_or_default().into(),
            artist: attrs["artistName"].as_str().unwrap_or_default().into(),
            album: attrs["name"].as_str().unwrap_or_default().into(),
            artwork: attrs["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "400").replace("{h}", "400"),
            artwork_hq: String::new(),
            track_count: selected.len(),
            availability: Availability::Available,
            reason: String::new(),
            has_lossless: qualities.iter().any(|q| q.contains("lossless")),
            has_atmos: qualities.iter().any(|q| q.contains("atmos") || q.contains("spatial")),
            has_animated_artwork: attrs["editorialVideo"].is_object(),
            qualities,
            missing,
            warnings: vec![],
            alternatives: vec![],
            video_quality: vec![],
            real_quality: None,
        };
        p.finish_partial();
        p.quality_warnings();

        // Las otras ediciones se enseñan SIEMPRE, no solo cuando algo falla: el
        // link que llega suele ser el single cuando se quería el álbum, o la
        // versión limpia cuando se quería la explícita.
        p.alternatives = self.other_versions_api(album_id).await;
        if p.alternatives.is_empty() && p.availability != Availability::Available {
            p.alternatives = self.other_versions(&p.title, &p.artist, album_id).await;
        }
        Ok(p)
    }

    async fn preview_song(&self, song_id: &str) -> Result<Preview> {
        let data = self.song(song_id).await?;
        let attrs = &data["data"][0]["attributes"];
        let playable = track_playable(attrs);
        let qualities = traits_of(attrs);
        let mut p = Preview {
            kind: "song".into(),
            id: song_id.into(),
            title: attrs["name"].as_str().unwrap_or_default().into(),
            artist: attrs["artistName"].as_str().unwrap_or_default().into(),
            album: attrs["albumName"].as_str().unwrap_or_default().into(),
            artwork: attrs["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "400").replace("{h}", "400"),
            artwork_hq: String::new(),
            track_count: 1,
            availability: if playable { Availability::Available } else { Availability::Unavailable },
            reason: if playable { String::new() } else { format!("Sin stream en {}", self.storefront.to_uppercase()) },
            has_lossless: qualities.iter().any(|q| q.contains("lossless")),
            has_atmos: qualities.iter().any(|q| q.contains("atmos") || q.contains("spatial")),
            has_animated_artwork: false,
            qualities,
            missing: vec![],
            warnings: vec![],
            alternatives: vec![],
            video_quality: vec![],
            real_quality: None,
        };
        p.quality_warnings();
        Ok(p)
    }

    async fn preview_mv(&self, mv_id: &str) -> Result<Preview> {
        let data = self.music_video(mv_id).await?;
        let attrs = &data["data"][0]["attributes"];
        let playable = attrs["playParams"]["id"].is_string();
        let mut video_quality = Vec::new();
        if attrs["has4K"].as_bool().unwrap_or(false) {
            video_quality.push("4K".to_string());
        }
        if attrs["hasHDR"].as_bool().unwrap_or(false) {
            video_quality.push("HDR".to_string());
        }
        if video_quality.is_empty() {
            video_quality.push("HD".to_string());
        }
        Ok(Preview {
            kind: "music-video".into(),
            id: mv_id.into(),
            title: attrs["name"].as_str().unwrap_or_default().into(),
            artist: attrs["artistName"].as_str().unwrap_or_default().into(),
            album: attrs["albumName"].as_str().unwrap_or_default().into(),
            artwork: attrs["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "400").replace("{h}", "400"),
            artwork_hq: String::new(),
            track_count: 1,
            availability: if playable { Availability::Available } else { Availability::Unavailable },
            reason: if playable { String::new() } else { format!("No disponible en {}", self.storefront.to_uppercase()) },
            qualities: vec![],
            has_lossless: false,
            has_atmos: false,
            has_animated_artwork: attrs["editorialVideo"].is_object(),
            missing: vec![],
            warnings: vec![],
            alternatives: vec![],
            video_quality,
            real_quality: None,
        })
    }

    /// Otras ediciones del mismo álbum, por el camino oficial.
    ///
    /// Apple las publica en `view/other-versions`: es exactamente lo que la
    /// gente busca cuando el álbum del link es la edición equivocada (deluxe,
    /// remasterizada, explícita vs limpia…). El bot usa este mismo endpoint.
    async fn other_versions_api(&self, album_id: &str) -> Vec<Alternative> {
        let path = format!("/v1/catalog/{}/albums/{album_id}/view/other-versions", self.storefront);
        let Ok(v) = self.get(&path, &[("limit", "10"), ("l", &self.language)], false).await else {
            return Vec::new();
        };
        v["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|a| {
                        let at = &a["attributes"];
                        Alternative {
                            id: a["id"].as_str().unwrap_or_default().into(),
                            name: at["name"].as_str().unwrap_or_default().into(),
                            artist: at["artistName"].as_str().unwrap_or_default().into(),
                            year: at["releaseDate"].as_str().unwrap_or_default().chars().take(4).collect(),
                            artwork: at["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "200").replace("{h}", "200"),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Busca otras ediciones del mismo álbum en la tienda de la cuenta.
    ///
    /// Es el respaldo de `other_versions_api`: sirve cuando el álbum del link ni
    /// siquiera está en la tienda, porque entonces no hay id al que preguntarle.
    async fn other_versions(&self, title: &str, artist: &str, exclude_id: &str) -> Vec<Alternative> {
        if title.is_empty() {
            return Vec::new();
        }
        let term = format!("{artist} {title}");
        let Ok(v) = self.search(&term, 10).await else { return Vec::new() };
        // La búsqueda devuelve lo que se le parece, no lo que ES: sin este
        // filtro salían álbumes de otros artistas presentados como "otras
        // versiones", que es peor que no enseñar nada.
        let quiere = normalize(title);
        v["results"]["albums"]["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter(|a| a["id"].as_str() != Some(exclude_id))
                    .filter(|a| {
                        let n = normalize(a["attributes"]["name"].as_str().unwrap_or_default());
                        n.contains(&quiere) || quiere.contains(&n)
                    })
                    .map(|a| {
                        let at = &a["attributes"];
                        Alternative {
                            id: a["id"].as_str().unwrap_or_default().into(),
                            name: at["name"].as_str().unwrap_or_default().into(),
                            artist: at["artistName"].as_str().unwrap_or_default().into(),
                            year: at["releaseDate"].as_str().unwrap_or_default().chars().take(4).collect(),
                            artwork: at["artwork"]["url"].as_str().unwrap_or_default().replace("{w}", "200").replace("{h}", "200"),
                        }
                    })
                    .take(5)
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Preview {
    /// Decide disponibilidad total/parcial/ninguna a partir de lo que falta.
    fn finish_partial(&mut self) {
        if self.track_count == 0 {
            return;
        }
        if self.missing.len() >= self.track_count {
            self.availability = Availability::Unavailable;
            self.reason = "Ninguna pista tiene stream disponible".into();
        } else if !self.missing.is_empty() {
            self.availability = Availability::Partial;
            self.reason = format!("{} de {} pistas sin stream", self.missing.len(), self.track_count);
            let lista = self.missing.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            self.warnings.push(Warning {
                code: "partial".into(),
                detail: format!("No están: {lista}"),
            });
        }
    }

    /// Avisos de calidad. Son los del bot: si el catálogo no marca lossless, lo
    /// que baja es AAC aunque se pida ALAC, y más vale decirlo antes.
    fn quality_warnings(&mut self) {
        if self.availability == Availability::Unavailable {
            return;
        }
        if !self.has_lossless {
            self.warnings.push(Warning {
                code: "no_lossless".into(),
                detail: format!(
                    "El catálogo no marca lossless{}: la descarga saldrá en AAC",
                    if self.qualities.is_empty() { String::new() } else { format!(" ({})", self.qualities.join(", ")) }
                ),
            });
        }
        if !self.has_atmos {
            self.warnings.push(Warning {
                code: "no_atmos".into(),
                detail: "Sin Atmos marcado en el catálogo".into(),
            });
        }
        if !self.has_animated_artwork {
            self.warnings.push(Warning {
                code: "no_animated".into(),
                detail: "Sin artwork animado".into(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Preview {
        Preview {
            kind: "album".into(), id: "1".into(), title: "X".into(), artist: "Y".into(),
            album: "X".into(), artwork: String::new(), track_count: 10,
            availability: Availability::Available, reason: String::new(),
            qualities: vec![], has_lossless: true, has_atmos: true,
            has_animated_artwork: true, missing: vec![], warnings: vec![],
            alternatives: vec![], video_quality: vec![],
            artwork_hq: String::new(), real_quality: None,
        }
    }

    #[test]
    fn el_respaldo_no_cuela_albumes_de_otro_artista() {
        // "In Between Dreams" traía de "otras versiones" un disco de Boyz II Men
        // porque la búsqueda devuelve lo que se PARECE.
        assert_eq!(normalize("Break Through the Silence - Single"), "break through the silence");
        assert_eq!(normalize("In Between Dreams (Bonus Track Version)"), "in between dreams bonus track version");
        let a = normalize("Discovery");
        let b = normalize("CooleyHighHarmony (Bonus Track Version)");
        assert!(!(a.contains(&b) || b.contains(&a)), "no se parecen y no deben colarse");
    }

    #[test]
    fn el_nombre_sale_de_la_url_cuando_el_album_no_esta() {
        // Sin esto, un álbum que no está en la tienda no tiene nombre con el que
        // buscar otras ediciones, y el aviso se queda en "no disponible" a secas.
        assert_eq!(slug_of("https://music.apple.com/nz/album/turn-up-the-bass-single/678"), "turn-up-the-bass-single");
        assert_eq!(slug_of("https://music.apple.com/jp/album/x/111?i=222"), "x");
    }

    #[test]
    fn faltando_algunas_pistas_es_parcial_y_dice_cuales() {
        let mut p = base();
        p.missing = vec!["A".into(), "B".into()];
        p.finish_partial();
        assert_eq!(p.availability, Availability::Partial);
        assert!(p.reason.contains("2 de 10"));
        assert!(p.warnings.iter().any(|w| w.code == "partial" && w.detail.contains("A, B")));
    }

    #[test]
    fn faltando_todas_no_esta_disponible() {
        let mut p = base();
        p.track_count = 2;
        p.missing = vec!["A".into(), "B".into()];
        p.finish_partial();
        assert_eq!(p.availability, Availability::Unavailable);
    }

    #[test]
    fn sin_lossless_se_avisa_de_que_bajara_aac() {
        let mut p = base();
        p.has_lossless = false;
        p.qualities = vec!["lossy-stereo".into()];
        p.quality_warnings();
        let w = p.warnings.iter().find(|w| w.code == "no_lossless").expect("debe avisar");
        assert!(w.detail.contains("AAC"));
        assert!(w.detail.contains("lossy-stereo"));
    }

    #[test]
    fn lo_que_no_esta_disponible_no_lleva_avisos_de_calidad() {
        let mut p = base();
        p.availability = Availability::Unavailable;
        p.has_lossless = false;
        p.quality_warnings();
        assert!(p.warnings.is_empty(), "primero se resuelve la disponibilidad");
    }
}
