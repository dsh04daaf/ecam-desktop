//! Comprueba el CDM de PlayReady contra una verdad de referencia real.
//!
//!   cargo run --example prkey -- <adam_id de un music video con 4K>
//!
//! El tier c1 (≤1080p) lo sirven **Widevine y PlayReady con la misma llave de
//! contenido**, así que se piden las dos y se exige que coincidan byte a byte.
//! Si nuestra criptografía se tuerce, esto lo caza; un valor grabado a mano, no.
//! Después pide la del tier SL3000, que es la que Widevine ya no da.
//!
//! No es un test de `cargo test` a propósito: necesita red, la cuenta y el
//! wrapper, y cada llamada abre una sesión de reproducción en Apple.

use ecam_core::{amp::Amp, config::Config};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adam_id = std::env::args().nth(1).unwrap_or_else(|| "1869285675".into());

    let mut cfg = Config::load_or_create()?;
    let amp = Amp::autoconfigure(&mut cfg).await?;
    println!("tienda {} · vídeo {adam_id}", amp.storefront);

    let report = ecam_core::mv::debug_key_paths(&cfg, &amp, &adam_id).await?;

    println!("\ntier bajo  (Widevine)  : {}", report.widevine_low);
    println!("tier bajo  (PlayReady) : {}", report.playready_low);
    if report.widevine_low == report.playready_low {
        println!("  ==> ✅ COINCIDEN: el CDM de PlayReady es correcto");
    } else {
        println!("  ==> ❌ NO COINCIDEN");
        std::process::exit(1);
    }

    match report.playready_high {
        Some(k) => println!("\ntier SL3000 (PlayReady): {k}  ✅"),
        None => println!("\n(este vídeo no tiene variantes SL3000)"),
    }
    Ok(())
}
