//! CLI mínima para ejercitar el core sin la carcasa de Tauri.
//!
//!   cargo run --example ecam -- <url> [alac|aac|atmos|binaural]
//!
//! No pretende ser la interfaz de nadie: es la forma de probar el motor entero
//! en Linux, que es justo para lo que el core no depende de Tauri.

use ecam_core::{amp::Amp, cancel::Cancel, collection, config::Config, Quality};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber_init();

    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or("uso: ecam <url de Apple Music> [calidad]")?;
    let quality = match args.next().as_deref() {
        Some("aac") => Quality::Aac,
        Some("atmos") => Quality::Atmos,
        Some("binaural") => Quality::Binaural,
        _ => Quality::Alac,
    };

    let mut cfg = Config::load_or_create()?;
    if let Ok(dir) = std::env::var("ECAM_OUT") {
        cfg.output_dir = dir.into();
    }
    if let Ok(port) = std::env::var("ECAM_WRAPPER") {
        cfg.decrypt_port = port;
    }

    // Detecta tienda, idioma y token de usuario, y lo deja escrito en el config.
    let amp = Amp::autoconfigure(&mut cfg).await?;
    println!("tienda {} · idioma {} · salida {}", amp.storefront, amp.language, cfg.output_dir.display());

    let downloaded = Arc::new(AtomicU64::new(0));
    let d = downloaded.clone();
    let progress = Arc::new(move |st: ecam_core::track::Stage| {
        if let ecam_core::track::Stage::Downloading(n) = st {
            d.fetch_add(n, Ordering::Relaxed);
        }
    });

    let on_track = Arc::new(|i: usize, total: usize, r: &Result<ecam_core::track::TrackOutcome, ecam_core::Error>| {
        match r {
            Ok(o) if o.skipped => println!("[{i}/{total}] ya estaba: {}", o.name),
            Ok(o) => println!(
                "[{i}/{total}] ✓ {} [{}] · {:.1}s ({:.1} baja / {:.1} descifra)",
                o.name, o.quality_label, o.secs_total, o.secs_download, o.secs_decrypt
            ),
            Err(e) => println!("[{i}/{total}] ✗ {e}"),
        }
    });

    let report = collection::download_url(&cfg, &amp, &url, quality, Some(progress), Some(on_track), &Cancel::new()).await?;

    println!(
        "\n{} listos, {} con problemas, {:.1} MB bajados",
        report.done.len(),
        report.failed.len(),
        downloaded.load(Ordering::Relaxed) as f64 / 1_048_576.0
    );
    for (name, e) in &report.failed {
        println!("  ✗ {name}: {e}");
    }
    Ok(())
}

fn tracing_subscriber_init() {
    // Sin dependencias extra: basta con que los `tracing::warn!` se vean.
    struct Simple;
    impl tracing::Subscriber for Simple {
        fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
            *m.level() <= tracing::Level::INFO
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct V;
            impl tracing::field::Visit for V {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        println!("  · {v:?}");
                    }
                }
            }
            event.record(&mut V);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    let _ = tracing::subscriber::set_global_default(Simple);
}
