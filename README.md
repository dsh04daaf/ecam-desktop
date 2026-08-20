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

## Vista previa de la UI (sin compilar Tauri)

Tauri necesita el toolchain de Windows, así que la interfaz se prueba en el
navegador con el mismo `src/app.js` que corre en la app — igual que se hizo con
ECBP Desktop antes de publicarlo:

```bash
cargo build --example rpc      # el puente con el core
npm install && npm run preview # http://127.0.0.1:3026
```

`dev-server.js` emula los comandos de Tauri llamando al core **de verdad**
(`core/examples/rpc.rs`), así que lo que se ve es lo que hace la app. En la VPS
usa el wrapper que ya tiene sesión iniciada: no hay login que pasar para probar
el resto. Lo que toca disco de la máquina (instalar la distro, cerrar sesión)
responde con un error a propósito.

## macOS (Apple Silicon)

Rama `macos-arm`, workflow `build-macos.yml` (`macos-14`, arm64). Sale un
`.dmg` y un `.zip` con el `.app`.

**El motor no viaja dentro.** El wrapper es un ELF **x86-64** de Android y en Mac
no hay WSL, así que la app arranca en modo `external`: la primera pantalla pide
la dirección `host:puerto` de un motor que corra en otro sitio (la VPS, por
ejemplo) y de ese mismo host se deriva el 30020 de la cuenta. Empaquetar el
motor en el Mac necesita una de estas dos, y ninguna está hecha:

- Docker Desktop / Colima con `linux/amd64` bajo Rosetta.
- Wrapper recompilado para aarch64 con las `.so` `arm64-v8a` del APK, dentro de
  una VM ligera. **Sin verificar** que exista `libCoreLSKD.so` en arm64.

**El puerto 10020 no lleva cifrado ni autenticación.** Por él viajan las llaves
de FairPlay y el audio en claro, así que **no se expone a internet**: se saca por
un túnel SSH (`ssh -L 10020:127.0.0.1:10020 …`, y otro para el 30020) o por una
red privada tipo Tailscale, y en la app se pone `127.0.0.1:10020`. Además, el
descifrado va por lotes con una ventana de 256 KB en vuelo: con latencia de WAN
va bastante más lento que en local, y eso es de esperar, no un fallo.

**Gatekeeper.** Sin Apple Developer ID la app va firmada solo ad-hoc. Arranca
(en arm64 un binario sin firma ni siquiera se ejecuta), pero al bajarla del
navegador macOS la marca en cuarentena y dice que está dañada. Se quita con:

```sh
xattr -dr com.apple.quarantine /Applications/ECAM.app
```

o abriéndola una vez desde Ajustes → Privacidad y seguridad → «Abrir de todos modos».
