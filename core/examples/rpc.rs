//! Puente de línea de comandos para el servidor de vista previa.
//!
//! Un proceso por llamada, JSON por stdout. Existe para que `dev-server.js`
//! pueda ejercitar el core DE VERDAD desde el navegador sin compilar Tauri,
//! igual que se hizo con ECBP antes de publicarlo.
//!
//!   rpc state | rpc config | rpc search <término> | rpc download <url> <calidad>

use ecam_core::{amp::Amp, cancel::Cancel, collection, config::Config, wrapper::Wrapper, Quality};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("state");

    let mut cfg = match Config::load_or_create() {
        Ok(c) => c,
        Err(e) => return out(json!({ "error": e.to_string() })),
    };
    if let Ok(dir) = std::env::var("ECAM_OUT") {
        cfg.output_dir = dir.into();
    }

    match cmd {
        "state" => {
            let listening = Wrapper::probe(&cfg.decrypt_port);
            // Del JSON de la cuenta solo sale la tienda: el resto son tokens
            // vivos y esto se sirve a un navegador.
            let account = if listening {
                ecam_core::amp::wrapper_account(&cfg.decrypt_port)
                    .await
                    .and_then(|a| a["storefront_id"].as_str().map(|s| json!({ "storefront_id": s })))
            } else {
                None
            };
            out(json!({
                // En la vista previa el motor ya está corriendo con la sesión
                // real: no hay pantalla de instalación ni de login que pasar.
                "distro_installed": true,
                "has_session": listening,
                "listening": listening,
                "account": account,
            }));
        }
        "config" => out(serde_json::to_value(&cfg).unwrap_or_default()),
        "search" => {
            let term = args.get(1).cloned().unwrap_or_default();
            match Amp::autoconfigure(&mut cfg).await {
                Ok(amp) => match amp.search(&term, 25).await {
                    Ok(v) => out(serde_json::to_value(ecam_core::amp::search_hits(&v)).unwrap_or_default()),
                    Err(e) => out(json!({ "error": e.to_string() })),
                },
                Err(e) => out(json!({ "error": e.to_string() })),
            }
        }
        "browse" => {
            let kind = args.get(1).cloned().unwrap_or_default();
            let id = args.get(2).cloned().unwrap_or_default();
            match Amp::autoconfigure(&mut cfg).await {
                Ok(amp) => match amp.browse(&kind, &id).await {
                    Ok(b) => out(serde_json::to_value(b).unwrap_or_default()),
                    Err(e) => out(json!({ "error": e.to_string() })),
                },
                Err(e) => out(json!({ "error": e.to_string() })),
            }
        }
        "url" => {
            let kind = args.get(1).cloned().unwrap_or_default();
            let id = args.get(2).cloned().unwrap_or_default();
            let sf = match Amp::autoconfigure(&mut cfg).await {
                Ok(a) => a.storefront,
                Err(_) => cfg.storefront.clone(),
            };
            out(json!({ "url": collection::url_for(&sf, &kind, &id) }));
        }
        "download" => {
            let url = args.get(1).cloned().unwrap_or_default();
            let quality = match args.get(2).map(String::as_str) {
                Some("aac") => Quality::Aac,
                Some("atmos") => Quality::Atmos,
                Some("binaural") => Quality::Binaural,
                _ => Quality::Alac,
            };
            let amp = match Amp::autoconfigure(&mut cfg).await {
                Ok(a) => a,
                Err(e) => return out(json!({ "event": "finished", "ok": false, "error": e.to_string() })),
            };
            // Una línea de JSON por suceso: el servidor las reenvía tal cual.
            let on_track = Arc::new(|i: usize, total: usize, r: &Result<ecam_core::track::TrackOutcome, ecam_core::Error>| {
                let v = match r {
                    Ok(o) => json!({ "event": "track", "index": i, "total": total, "ok": true,
                                     "name": o.name, "detail": if o.skipped { "ya estaba".into() } else { o.quality_label.clone() } }),
                    Err(e) => json!({ "event": "track", "index": i, "total": total, "ok": false, "detail": e.to_string() }),
                };
                println!("{v}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
            });
            match collection::download_url(&cfg, &amp, &url, quality, None, Some(on_track), &Cancel::new()).await {
                Ok(r) => out(json!({ "event": "finished", "ok": true, "done": r.done.len(), "failed": r.failed.len() })),
                Err(e) => out(json!({ "event": "finished", "ok": false, "error": e.to_string() })),
            }
        }
        other => out(json!({ "error": format!("comando desconocido: {other}") })),
    }
}

fn out(v: serde_json::Value) {
    println!("{v}");
}
