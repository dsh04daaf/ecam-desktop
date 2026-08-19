// En release no se abre una consola detrás de la ventana.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Carcasa de ECAM Desktop. Aquí no hay lógica de descarga: todo eso vive en
//! `ecam-core`, que se prueba en Linux. Esto solo traduce entre la ventana y el
//! motor, y vigila el wrapper.

use ecam_core::{
    amp::Amp,
    collection,
    config::{Config, Quality},
    runtime::{Backend, Event, Runtime},
    track::TrackOutcome,
    wrapper::Wrapper,
};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};

struct AppState {
    cfg: Mutex<Config>,
    /// El cliente de Apple se construye una vez: trae el bearer cacheado y el
    /// pool de conexiones. Rehacerlo por comando tira los dos.
    amp: tokio::sync::Mutex<Option<Amp>>,
    /// Proceso del wrapper, para poder matarlo al cerrar.
    child: Mutex<Option<tokio::process::Child>>,
    /// Descargas vivas → su bandera de cancelación.
    jobs: Mutex<std::collections::HashMap<u64, Arc<AtomicBool>>>,
    seq: AtomicU64,
}

impl AppState {
    fn runtime(&self) -> Runtime {
        let cfg = self.cfg.lock().unwrap();
        Runtime::new(Backend::default(), cfg.decrypt_port.clone())
    }
}

#[derive(Serialize)]
struct WrapperState {
    distro_installed: bool,
    has_session: bool,
    listening: bool,
    account: Option<serde_json::Value>,
}

/// Qué pantalla toca: instalar, login, o dentro.
#[tauri::command]
async fn wrapper_state(state: State<'_, AppState>) -> Result<WrapperState, String> {
    let rt = state.runtime();
    let port = state.cfg.lock().unwrap().decrypt_port.clone();
    let listening = Wrapper::probe(&port);
    Ok(WrapperState {
        distro_installed: rt.distro_installed().await,
        has_session: rt.has_session().await,
        listening,
        account: if listening { ecam_core::amp::wrapper_account(&port).await } else { None },
    })
}

#[tauri::command]
async fn install_distro(state: State<'_, AppState>, tarball: String) -> Result<(), String> {
    let rt = state.runtime();
    let target = dirs_local().join("ECAM").join("distro");
    rt.import_distro(std::path::Path::new(&tarball), &target)
        .await
        .map_err(|e| e.to_string())
}

fn dirs_local() -> std::path::PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// Arranca el wrapper. Con `creds` hace login; sin ellas usa la sesión guardada.
///
/// Los estados salen por el evento `wrapper` para que la UI cambie de pantalla
/// sola — sobre todo el del 2FA, que tiene una ventana de 60 s.
#[tauri::command]
async fn start_wrapper(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    user: Option<String>,
    password: Option<String>,
) -> Result<(), String> {
    let rt = state.runtime();
    let creds = match (&user, &password) {
        (Some(u), Some(p)) if !u.is_empty() => Some((u.as_str(), p.as_str())),
        _ => None,
    };

    let (child, mut rx) = rt.start(creds).await.map_err(|e| e.to_string())?;
    *state.child.lock().unwrap() = Some(child);

    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            // Los códigos de Apple se traducen aquí para que la UI no tenga
            // que saber de números.
            let payload = match &ev {
                Event::AuthError { code } => serde_json::json!({
                    "type": "auth_error",
                    "value": { "code": code, "message": ecam_core::runtime::auth_error_message(code) }
                }),
                other => serde_json::to_value(other).unwrap_or_default(),
            };
            let _ = app.emit("wrapper", payload);
        }
    });
    Ok(())
}

#[tauri::command]
async fn submit_two_factor(state: State<'_, AppState>, code: String) -> Result<(), String> {
    state.runtime().submit_two_factor(&code).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn sign_out(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(mut c) = state.child.lock().unwrap().take() {
        let _ = c.start_kill();
    }
    state.runtime().sign_out().await.map_err(|e| e.to_string())
}

/// Cliente de Apple, construido una sola vez. La primera llamada detecta tienda,
/// idioma y token de usuario, y los deja escritos en el config.
async fn amp(state: &State<'_, AppState>) -> Result<Amp, String> {
    let mut slot = state.amp.lock().await;
    if let Some(a) = slot.as_ref() {
        return Ok(a.clone());
    }
    let mut cfg = state.cfg.lock().unwrap().clone();
    let a = Amp::autoconfigure(&mut cfg).await.map_err(|e| e.to_string())?;
    *state.cfg.lock().unwrap() = cfg;
    *slot = Some(a.clone());
    Ok(a)
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<Config, String> {
    Ok(state.cfg.lock().unwrap().clone())
}

#[tauri::command]
async fn set_config(state: State<'_, AppState>, cfg: Config) -> Result<(), String> {
    let mut current = cfg;
    // La ruta del archivo no viaja a la UI: se conserva la que ya había.
    current.source_path = state.cfg.lock().unwrap().source_path.clone();
    current.persist().map_err(|e| e.to_string())?;
    *state.cfg.lock().unwrap() = current;
    // Tienda o idioma pueden haber cambiado: que se reconstruya el cliente.
    *state.amp.lock().await = None;
    Ok(())
}

#[derive(Serialize)]
struct SearchHit {
    id: String,
    kind: String,
    name: String,
    artist: String,
    artwork: String,
}

#[tauri::command]
async fn search(state: State<'_, AppState>, term: String) -> Result<Vec<SearchHit>, String> {
    let amp = amp(&state).await?;
    let v = amp.search(&term, 25).await.map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for kind in ["albums", "songs", "music-videos"] {
        let Some(items) = v["results"][kind]["data"].as_array() else { continue };
        for it in items {
            let a = &it["attributes"];
            out.push(SearchHit {
                id: it["id"].as_str().unwrap_or("").into(),
                kind: kind.trim_end_matches('s').into(),
                name: a["name"].as_str().unwrap_or("").into(),
                artist: a["artistName"].as_str().unwrap_or("").into(),
                artwork: a["artwork"]["url"]
                    .as_str()
                    .unwrap_or("")
                    .replace("{w}", "200")
                    .replace("{h}", "200"),
            });
        }
    }
    Ok(out)
}

#[derive(Serialize, Clone)]
struct TrackDone {
    job: u64,
    index: usize,
    total: usize,
    ok: bool,
    name: String,
    detail: String,
}

#[tauri::command]
async fn download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    url: String,
    quality: String,
) -> Result<u64, String> {
    let amp = amp(&state).await?;
    let cfg = state.cfg.lock().unwrap().clone();
    let quality = match quality.as_str() {
        "aac" => Quality::Aac,
        "atmos" => Quality::Atmos,
        "binaural" => Quality::Binaural,
        _ => Quality::Alac,
    };

    let job = state.seq.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    state.jobs.lock().unwrap().insert(job, cancel.clone());

    let app2 = app.clone();
    let progress = Arc::new(move |n: u64| {
        let _ = app2.emit("progress", serde_json::json!({ "job": job, "bytes": n }));
    });
    let app3 = app.clone();
    let on_track = Arc::new(move |i: usize, total: usize, r: &Result<TrackOutcome, ecam_core::Error>| {
        let payload = match r {
            Ok(o) => TrackDone {
                job, index: i, total, ok: true,
                name: o.name.clone(),
                detail: if o.skipped { "ya estaba".into() } else { o.quality_label.clone() },
            },
            Err(e) => TrackDone { job, index: i, total, ok: false, name: String::new(), detail: e.to_string() },
        };
        let _ = app3.emit("track", payload);
    });

    tauri::async_runtime::spawn(async move {
        let res = collection::download_url(&cfg, &amp, &url, quality, Some(progress), Some(on_track)).await;
        let payload = match res {
            Ok(r) => serde_json::json!({
                "job": job, "ok": true, "done": r.done.len(), "failed": r.failed.len(),
                "path": r.done.first().and_then(|d| d.path.parent().map(|p| p.display().to_string())),
            }),
            Err(e) => serde_json::json!({ "job": job, "ok": false, "error": e.to_string() }),
        };
        let _ = app.emit("finished", payload);
    });

    Ok(job)
}

#[tauri::command]
fn cancel(state: State<'_, AppState>, job: u64) {
    if let Some(flag) = state.jobs.lock().unwrap().get(&job) {
        flag.store(true, Ordering::Relaxed);
    }
}

fn main() {
    let cfg = Config::load_or_create().unwrap_or_default();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState {
            cfg: Mutex::new(cfg),
            amp: tokio::sync::Mutex::new(None),
            child: Mutex::new(None),
            jobs: Mutex::new(Default::default()),
            seq: AtomicU64::new(1),
        })
        .invoke_handler(tauri::generate_handler![
            wrapper_state, install_distro, start_wrapper, submit_two_factor, sign_out,
            get_config, set_config, search, download, cancel
        ])
        .on_window_event(|window, event| {
            // Al cerrar: matar el wrapper y apagar la distro. Si no, la VM de WSL
            // se queda encendida comiendo RAM hasta que el usuario cierre sesión.
            if let tauri::WindowEvent::Destroyed = event {
                let state = window.state::<AppState>();
                if let Some(mut c) = state.child.lock().unwrap().take() {
                    let _ = c.start_kill();
                }
                let rt = state.runtime();
                tauri::async_runtime::block_on(rt.shutdown());
            }
        })
        .run(tauri::generate_context!())
        .expect("no se pudo arrancar ECAM");
}
