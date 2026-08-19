//! Descifra un fMP4 de music video ya bajado. Solo para comparar contra el
//! original: `cargo run --example mvdec -- entrada.enc salida.mp4 <key-hex>`
use std::io::BufWriter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut src = std::fs::File::open(&a[0])?;
    let out = std::fs::File::create(&a[1])?;
    let mut w = BufWriter::new(out);
    ecam_core::mv::cbcs::decrypt_file(&mut src, &mut w, a[2].trim())?;
    Ok(())
}
