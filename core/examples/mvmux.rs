//! Une un vídeo y un audio ya descifrados: `mvmux video.mp4 audio.mp4 salida.mp4`
use ecam_core::mv::mux;
use std::io::BufWriter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut sources = Vec::new();
    for path in &a[..2] {
        let mut f = std::fs::File::open(path)?;
        for t in mux::read_tracks(&mut f)? {
            println!(
                "track {} · {} samples · {}x{} · timescale {}",
                String::from_utf8_lossy(&t.kind), t.samples.len(), t.width, t.height, t.timescale
            );
            sources.push((t, std::fs::File::open(path)?));
        }
    }
    let out = std::fs::File::create(&a[2])?;
    let mut w = BufWriter::new(out);
    mux::mux(&mut sources, &mut w)?;
    Ok(())
}
