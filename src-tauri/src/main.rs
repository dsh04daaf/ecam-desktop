// En release no se abre una consola detrás de la ventana.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Carcasa de ECAM Desktop. Aquí no hay lógica de descarga: todo eso vive en
//! `ecam-core`, que se prueba en Linux. Esto solo traduce entre la ventana y el
//! motor, y vigila el wrapper.

use ecam_core::{
    history,
    preview::Preview,
    amp::{search_hits, Amp, Browse, SearchHit},
    cancel::Cancel,
    collection,
    config::{Config, Quality},
    runtime::{Backend, Event, Runtime},
    track::TrackOutcome,
    wrapper::Wrapper,
};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};

struct AppState {
    cfg: Mutex<Config>,
    /// El cliente de Apple se construye una vez: trae el bearer cacheado y el
    /// pool de conexiones. Rehacerlo por comando tira los dos.
    amp: tokio::sync::Mutex<Option<Amp>>,
    /// Proceso del wrapper, para poder matarlo al cerrar.
    child: Mutex<Option<tokio::process::Child>>,
    /// Descargas vivas → su cancelador. Se quitan al terminar: si no, el mapa
    /// crece toda la sesión con trabajos muertos.
    jobs: Mutex<std::collections::HashMap<u64, Cancel>>,
    /// Últimas líneas del wrapper. La pestaña del motor las enseña: cuando algo
    /// se cae, lo que dijo el wrapper es la única pista real.
    logs: Mutex<std::collections::VecDeque<String>>,
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

    let app_logs = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            {
                let state = app_logs.state::<AppState>();
                let mut logs = state.logs.lock().unwrap();
                if logs.len() >= 500 {
                    logs.pop_front();
                }
                logs.push_back(format!("{ev:?}"));
            }
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
async fn set_config(state: State<'_, AppState>, cfg: serde_json::Value) -> Result<(), String> {
    // Se MEZCLA sobre el config actual en vez de sustituirlo: si la ventana no
    // manda un ajuste (porque es nuevo o porque falló al pintarlo), ese ajuste
    // no puede volver a su valor por defecto solo. Y el guardado es atómico.
    let merged = {
        let current = state.cfg.lock().unwrap();
        current.merge_patch(&cfg).map_err(|e| e.to_string())?
    };
    merged.persist().map_err(|e| e.to_string())?;
    *state.cfg.lock().unwrap() = merged;
    // Tienda o idioma pueden haber cambiado: que se reconstruya el cliente.
    *state.amp.lock().await = None;
    Ok(())
}

#[tauri::command]
async fn search(state: State<'_, AppState>, term: String) -> Result<Vec<SearchHit>, String> {
    let amp = amp(&state).await?;
    let v = amp.search(&term, 25).await.map_err(|e| e.to_string())?;
    // La conversión vive en el core y la comparte la vista previa: si cada uno
    // aplanara los resultados a su manera, probar una no diría nada de la otra.
    Ok(search_hits(&v))
}

#[derive(Serialize, Clone)]
struct TrackDone {
    job: u64,
    index: usize,
    total: usize,
    ok: bool,
    name: String,
    detail: String,
    /// `true` = la sesión del wrapper está muerta; reintentar no sirve.
    fatal: bool,
}

/// Instala las credenciales de los music videos desde donde el usuario las
/// tenga. Antes había que crear una carpeta a mano en %APPDATA% y adivinar los
/// nombres de los archivos.
/// Instala las credenciales de los music videos desde donde el usuario las
/// tenga. Reconoce cuál es cuál POR CONTENIDO, así que da igual cómo se llamen.
#[tauri::command]
async fn import_widevine(paths: Vec<String>) -> Result<String, String> {
    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(Into::into).collect();
    ecam_core::mv::widevine::install_credentials(&paths).map_err(|e| e.to_string())?;
    Ok(ecam_core::mv::widevine::credentials_dir().display().to_string())
}

/// ¿Están puestas las credenciales de vídeo?
#[tauri::command]
fn widevine_ready() -> bool {
    ecam_core::mv::widevine::credentials_present()
}

// ── historial ──────────────────────────────────────────────────────────────

#[tauri::command]
fn history_list() -> Vec<history::Entry> {
    history::load()
}

#[tauri::command]
fn history_clear() {
    history::clear();
}

#[tauri::command]
fn history_remove(id: String) {
    history::remove(&id);
}

/// Abre la carpeta en el explorador. Sin esto hay que ir a buscarla a mano.
#[tauri::command]
fn open_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

// ── motor ──────────────────────────────────────────────────────────────────

/// Relanza el wrapper y espera a que el puerto vuelva a aceptar conexiones.
///
/// Igual que el bot: se relanza CONSERVANDO la sesión (no se borra nada), así
/// que no hay 2FA de por medio. 90 s de margen, que es lo que tarda en malos días.
async fn relaunch_and_wait(app: &tauri::AppHandle) -> bool {
    let (port, backend) = {
        let state = app.state::<AppState>();
        if let Some(mut c) = state.child.lock().unwrap().take() {
            let _ = c.start_kill();
        }
        let cfg = state.cfg.lock().unwrap();
        (cfg.decrypt_port.clone(), Backend::default())
    };

    let rt = Runtime::new(backend, port.clone());
    let Ok((child, mut rx)) = rt.start(None).await else { return false };
    app.state::<AppState>().child.lock().unwrap().replace(child);

    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = app2.emit("wrapper", serde_json::to_value(&ev).unwrap_or_default());
        }
    });

    for _ in 0..90 {
        if Wrapper::probe(&port) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    false
}

/// Relanza el wrapper con la sesión guardada. Es lo que hay que hacer cuando la
/// sesión FairPlay se muere: reintentar la descarga no arregla nada.
#[tauri::command]
async fn restart_wrapper(app: tauri::AppHandle) -> Result<bool, String> {
    Ok(relaunch_and_wait(&app).await)
}

/// Lo que el wrapper ha ido diciendo, para la pestaña del motor.
#[tauri::command]
fn wrapper_logs(state: State<'_, AppState>) -> Vec<String> {
    state.logs.lock().unwrap().iter().cloned().collect()
}

/// Card de un link pegado: qué es, si se puede bajar y qué avisos hay.
#[tauri::command]
async fn preview(state: State<'_, AppState>, url: String) -> Result<Preview, String> {
    let amp = amp(&state).await?;
    amp.preview(&url).await.map_err(|e| e.to_string())
}

/// Abre una entidad (álbum, playlist, artista) para poder verla por dentro.
#[tauri::command]
async fn browse(state: State<'_, AppState>, kind: String, id: String) -> Result<Browse, String> {
    let amp = amp(&state).await?;
    let mut b = amp.browse(&kind, &id).await.map_err(|e| e.to_string())?;
    // Con esto cada pista puede enseñar su calidad real y ofrecer bajarla en
    // ALAC, Atmos, AAC o binaural sin tocar el selector general.
    amp.resolve_qualities(&mut b).await;
    Ok(b)
}

/// Descarga por tipo e id, sin que la UI tenga que inventarse una URL.
///
/// Antes la ventana construía `music.apple.com/us/...` con la tienda clavada;
/// la metadata se pedía a la tienda equivocada y solo funcionaba de rebote por
/// el respaldo. La tienda buena la sabe el core.
#[tauri::command]
async fn download_item(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    kind: String,
    id: String,
    quality: String,
) -> Result<u64, String> {
    let amp = amp(&state).await?;
    let url = collection::url_for(&amp.storefront, &kind, &id);
    download(app, state, url, quality).await
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

    let quality_name = quality.display().to_string();
    let job = state.seq.fetch_add(1, Ordering::Relaxed);
    let cancel = Cancel::new();
    state.jobs.lock().unwrap().insert(job, cancel.clone());

    // El progreso se acumula y se manda como mucho cuatro veces por segundo.
    // Emitir un evento por trozo eran miles de mensajes IPC por track para
    // pintar una barra que se mueve 60 veces por segundo como mucho.
    let acc = Arc::new(AtomicU64::new(0));
    let last = Arc::new(Mutex::new(std::time::Instant::now()));
    let app2 = app.clone();
    let (acc2, last2) = (acc.clone(), last.clone());
    let progress: ecam_core::track::Progress = Arc::new(move |st| {
        use ecam_core::track::Stage;
        let payload = match st {
            Stage::Downloading(n) => {
                let total = acc2.fetch_add(n, Ordering::Relaxed) + n;
                // Solo esta fase se limita: las otras son un puñado de avisos.
                let mut l = last2.lock().unwrap();
                if l.elapsed() < std::time::Duration::from_millis(250) {
                    return;
                }
                *l = std::time::Instant::now();
                serde_json::json!({ "job": job, "stage": "downloading", "bytes": total })
            }
            Stage::Decrypting { done, total } => {
                serde_json::json!({ "job": job, "stage": "decrypting", "done": done, "total": total })
            }
            Stage::Tagging => serde_json::json!({ "job": job, "stage": "tagging" }),
        };
        let _ = app2.emit("progress", payload);
    });

    let app3 = app.clone();
    let on_track = Arc::new(move |i: usize, total: usize, r: &Result<TrackOutcome, ecam_core::Error>| {
        let payload = match r {
            Ok(o) => TrackDone {
                job, index: i, total, ok: true,
                name: o.name.clone(),
                detail: if o.skipped {
                    "ya estaba".into()
                } else {
                    // Con el desglose a la vista, si algo va lento se ve DÓNDE
                    // en vez de tener que suponerlo.
                    format!(
                        "{} · {:.1}s ({:.1} baja / {:.1} descifra)",
                        o.quality_label, o.secs_total, o.secs_download, o.secs_decrypt
                    )
                },
                fatal: false,
            },
            Err(e) => TrackDone {
                job, index: i, total, ok: false,
                name: String::new(),
                detail: e.to_string(),
                // El core distingue "este track no se pudo" de "la sesión está
                // muerta". Lo segundo NO se arregla reintentando: hay que
                // relanzar el wrapper, y la UI necesita saberlo.
                fatal: matches!(e, ecam_core::Error::Track(t) if t.kind == ecam_core::error::FailKind::WrapperDead)
                    || matches!(e, ecam_core::Error::DecryptionCorrupted(_)),
            },
        };
        let _ = app3.emit("track", payload);
    });

    let jobs = app.state::<AppState>();
    let _ = jobs; // el estado se recupera dentro de la tarea

    let app_restart = app.clone();
    let restart: collection::Restart = Arc::new(move || {
        let app = app_restart.clone();
        Box::pin(async move { relaunch_and_wait(&app).await })
    });

    tauri::async_runtime::spawn(async move {
        let started = std::time::Instant::now();
        let ctx = collection::Ctx {
            cfg: &cfg, amp: &amp, quality,
            progress: Some(progress), on_track: Some(on_track),
            cancel: cancel.clone(), restart: Some(restart),
        };
        let res = collection::download_url(&ctx, &url).await;

        // Al historial pase lo que pase: saber qué falló ayer es justo lo que se
        // perdía al cerrar la app.
        let entry = match &res {
            Ok(r) => history::Entry {
                id: job.to_string(),
                at: history::now(),
                name: r.done.first().map(|d| d.album.clone()).unwrap_or_else(|| url.clone()),
                kind: quality_name.clone(),
                quality: r.done.first().map(|d| d.quality_label.clone()).unwrap_or_default(),
                ok: r.done.iter().filter(|d| !d.skipped).count(),
                skipped: r.done.iter().filter(|d| d.skipped).count(),
                folder: r.done.first().and_then(|d| d.path.parent().map(|p| p.display().to_string())).unwrap_or_default(),
                failed: r.failed.iter().map(|(n, e)| history::Failure { name: n.clone(), reason: e.to_string() }).collect(),
                cancelled: cancel.is_cancelled(),
                seconds: started.elapsed().as_secs_f32(),
            },
            Err(e) => history::Entry {
                id: job.to_string(), at: history::now(), name: url.clone(),
                kind: quality_name.clone(), quality: String::new(),
                ok: 0, skipped: 0, folder: String::new(),
                failed: vec![history::Failure { name: url.clone(), reason: e.to_string() }],
                cancelled: matches!(e, ecam_core::Error::Cancelled),
                seconds: started.elapsed().as_secs_f32(),
            },
        };
        history::append(entry);

        let payload = match res {
            Ok(r) => serde_json::json!({
                "job": job, "ok": true, "done": r.done.len(), "failed": r.failed.len(),
                "cancelled": cancel.is_cancelled(),
                "path": r.done.first().and_then(|d| d.path.parent().map(|p| p.display().to_string())),
            }),
            Err(ecam_core::Error::Cancelled) => serde_json::json!({ "job": job, "ok": true, "cancelled": true, "done": 0, "failed": 0 }),
            Err(e) => serde_json::json!({ "job": job, "ok": false, "error": e.to_string() }),
        };
        let _ = app.emit("finished", payload);
        // El trabajo ya no existe: fuera del mapa.
        app.state::<AppState>().jobs.lock().unwrap().remove(&job);
    });

    Ok(job)
}

#[tauri::command]
fn cancel(state: State<'_, AppState>, job: u64) {
    if let Some(c) = state.jobs.lock().unwrap().get(&job) {
        c.cancel();
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
            logs: Mutex::new(Default::default()),
            seq: AtomicU64::new(1),
        })
        .invoke_handler(tauri::generate_handler![
            wrapper_state, install_distro, start_wrapper, submit_two_factor, sign_out,
            get_config, set_config, search, browse, download, download_item, cancel,
            import_widevine, widevine_ready,
            history_list, history_clear, history_remove, open_folder, restart_wrapper,
            wrapper_logs, preview
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
