# ECAM Desktop

App de escritorio de Apple Music para Windows, con **la cuenta del propio
usuario**. Mismo planteamiento que ECBP Desktop: motor en Rust que se compila y
se prueba en Linux, carcasa de Tauri encima.

## Estado

| Pieza | Estado |
|---|---|
| Runtime del wrapper (distro WSL propia, 49 MB) | probado en Windows |
| Login con pantalla (usuario/clave + 2FA) | diseñado y documentado |
| Core: audio ALAC/AAC/Atmos/Binaural | **probado end-to-end, salida idéntica al original** |
| Core: letras, carátulas, artwork animado, etiquetas | hecho |
| Core: music videos (Widevine, cbcs, mux propio) | hecho, falta probar con un vídeo real |
| Carcasa Tauri + UI | pendiente |

## Qué es cada carpeta

```
core/          Motor en Rust: API de Apple, HLS, descifrado, MP4, etiquetas,
               music videos. NO depende de Tauri, así que compila y se prueba
               en Linux.
core/examples/ CLI mínima para ejercitar el motor sin la carcasa.
docs/          Inventario del comportamiento heredado del bot y protocolo del
               wrapper. Se lee ANTES de tocar el core.
```

## Probarlo

```bash
cargo test -p ecam-core
ECAM_OUT=/tmp/salida cargo run -q --example ecam -- "<url de Apple Music>" alac
```

Necesita el wrapper escuchando en `127.0.0.1:10020` (en Windows, dentro de la
distro WSL; ver `docs/PROTOCOLO_WRAPPER.md`).

## Verificación

El motor se comparó contra `apple-music-downloader` (Python) con el mismo track:
**el `mdat` sale byte a byte idéntico y el PCM decodificado tiene el mismo MD5.**
Esa es la prueba que hay que repetir cuando se toque el camino de descifrado.

## Reglas de la casa

- El comportamiento heredado del bot **no se "moderniza"**: cada rareza está
  documentada en `docs/INVENTARIO_CORE.md` con su motivo. Si el código y un
  comentario se contradicen, gana el código.
- Lo único que cambia a propósito es la memoria: el original cargaba el track
  entero en RAM (4 GB con un mix de una hora); aquí todo va en streaming.
- **Ninguna credencial dentro del binario**: la llave de dispositivo de Widevine
  y el resto se leen de disco. El token de usuario se le pide al wrapper.
