//! Prueba de instalación de credenciales de vídeo: `wvload <archivo> <archivo>`
fn main() {
    let paths: Vec<std::path::PathBuf> = std::env::args().skip(1).map(Into::into).collect();
    match ecam_core::mv::widevine::install_credentials(&paths) {
        Ok(()) => println!("instaladas · presentes = {}", ecam_core::mv::widevine::credentials_present()),
        Err(e) => println!("error: {e}"),
    }
}
