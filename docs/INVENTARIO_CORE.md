# Inventario del core — qué debe conservar el port a Rust

Regla de trabajo: **si está así, es por algo.** Nada se "moderniza" ni se simplifica sin
que aquí quede escrito por qué existía. Este documento se completa antes de escribir el motor.

Fuentes: `/srv/repos/apple-music-downloader/downloader.py` (3.859 líneas, canónico: tiene 10
funciones más que la copia del bot — artwork, portadas, formatos de nombre),
`/srv/bots/apple/mibot2_v6.py` (4.422 líneas), `/srv/bots/apple/config.yaml`.

## A. Arreglos que NO se pueden perder (verificados en el código)

| # | Qué | Dónde | Por qué existe |
|---|---|---|---|
| A1 | La llave FairPlay se manda **solo cuando cambia el URI**, no por fragmento | `downloader.py:1927-1946` (`last_key_uri`) | Re-mandarla por fragmento producía el error **-42786** |
| A2 | Se descartan las líneas `#EXT-X-KEY` que no sean `streamingkeydelivery` | `:349-359`, `:3212-3222` | La playlist trae **tres** llaves (FairPlay skd://, PlayReady, Widevine); quedarse con la equivocada rompe el descifrado |
| A3 | `PREFETCH_KEY` usa `adam_id = "0"` | `:1942` | Caso especial de la llave de prefetch |
| A4 | Se reconstruye `stsd` limpio y se arregla `moov` (stts vacío, duración mala en el init) | `:1816`, `:404` | ffmpeg **no detecta** un `stsd` sin limpiar (incidente 2026-07-31) |
| A5 | `moof` limpio: quitar `senc`/`saiz` y recalcular `trun data_offset` | `:1416` | Varios `trun`/`traf`/`trak` por fragmento |
| A6 | El tamaño del `moov` debe ser idéntico entre pasadas (`stco` de tamaño fijo) | `:838-882` | Si cambia, todos los offsets quedan corridos |
| A7 | UA de Chrome para MV | (port MV nativo) | Sin él Apple entrega 1080p en vez de la máxima |
| A8 | Legacy AAC `cbc2` sin `senc` | ver memoria `apple_legacy_aac` | Fix 2026-06-08; álbum 377826006 sigue sin ser descifrable |

## B. Problema conocido que el port **debe** arreglar (no replicar)

- **B1 — Un track largo se come 4 GB de RAM.** `chunk_payloads` acumula todos los bytes en
  memoria y luego hace `b"".join(...)` (`downloader.py:728-889`): ~6 copias completas del
  track. Un continuous mix de 1 h tumba el VPS entero y se oye como tirones en radio-discord.
  En Rust esto se escribe **en streaming a disco**, y es de las razones fuertes para portarlo.
  `max-memory-limit: 256` del config.yaml no lo contiene.

## C. Settings del config.yaml que son decisiones, no defaults

- `get-m3u8-mode: hires` — no `all`.
- `alac-max: 192000`, `atmos-max: 2768`, `aac-type: aac`, `mv-max: 2160`, `mv-audio-type: atmos`.
- `storefront: nz` y `language: es-MX` (la cuenta es de Nueva Zelanda; si no coinciden,
  la mayoría de las letras fallan).
- `media-user-token` es obligatorio **para letras y AAC-LC**; el `authorization-token` se saca solo.
- Plantillas: `album-folder-format {AlbumName}`, `song-file-format "{SongNumer}. {SongName}"`,
  `artist-folder-format {UrlArtistName}`, etiquetas `[E]` / `[C]` / `[M]`.
- Carpetas separadas por códec (alac / atmos / aac / mv).
- Conversión: apagada por defecto; `convert-skip-lossy-to-lossless: true` y
  `convert-warn-lossy-to-lossless: true` — **no convertir un AAC a FLAC** y avisar.
- `limit-max: 200`.

## D. Comportamiento del bot que la app debe heredar

- **Caché primero**: lo ya cacheado se adelanta a toda la cola (prio -1) en vez de esperar
  detrás de descargas largas.
- Cola con estado persistente (`queue_dump.json`, `tqueue.binlog`).
- Elección de formato **por descarga** (botones inline), no preferencia guardada por usuario.
- `/status` del wrapper, `/dc` de caché, `/queue`, `/dq`.
- Listas `allowed_users1.txt` y `blacklist.txt` (control de acceso, no ajustes).
- Estadísticas por usuario/día (`apple_bot_cache.db`), caché por hash de archivo.
- El ZIP **nunca** debe empaquetar la carpeta base (bug 2026-07-30: se llevaba descargas de
  otros usuarios). En la app de escritorio esto es carpeta por descarga.

## E. Barrido del downloader (hecho 2026-08-19) — decisiones que el port debe respetar

Referencias a `/srv/repos/apple-music-downloader/downloader.py` (3.859 líneas).

### E1. Protocolo con el wrapper (`WrapperConn`, :1148-1224)

TCP crudo contra el 10020, sin framing de texto:

| Operación | Bytes |
|---|---|
| SwitchKeys | `00 00 00 00` |
| SendString | `[1 byte len][string]` — se manda adam_id y luego el key_uri |
| DecryptChunk | `[uint32 LE len][datos]` → recibe N bytes descifrados |
| Close | `00 00 00 00 00` |

- `TCP_NODELAY` activado: sin él cada fragmento paga el retardo de Nagle.
- Se descifra **truncando a múltiplo de 16** (`len & ~0xF`); la cola de <16 bytes pasa en claro.
- `decrypt_bulk` manda el mdat entero en **una** ida y vuelta, no por sample.
- `adam_id = "0"` cuando el URI es `PREFETCH_KEY` (`skd://itunes.apple.com/P000000000/s1/e1`).

### E2. ⚠️ Comentario obsoleto en el código — el que manda es el código

En `:1919-1924` hay un comentario viejo que dice *"Mirror runv2.go exactly: send SwitchKeys+key_info
before every fragment"*. **Eso ya no es lo que hace el código** y no se debe portar: dos líneas
abajo el código real solo remanda la llave `if key_uri != last_key_uri`, con su propio comentario
explicando por qué (re-mandarla por fragmento pedía la licencia FairPlay ~14 veces por track y
agotaba la sesión en ~8 min → cascada -42786).

**Regla para el port: si un comentario y el código se contradicen, gana el código**, y se
verifica contra el historial antes de tocar nada.

### E3. Corrupción silenciosa (`_validate_decrypted_sample`, :1327-1345)

Una sesión FairPlay muerta **no da error**: devuelve basura. Por eso se valida la firma del códec
sample por sample y se lanza `DecryptionCorruptedError`:

- **ALAC**: el primer byte debe cumplir `(b & 0xC0) == 0x00` (SCE/CPE). Datos AES aleatorios
  fallan ~75 % de las veces por sample.
- **EC-3 / AC-3**: sync word `0x0B 0x77`.
- **AAC-LC**: sin firma fiable → no se valida (documentado, no olvidado).

Del lado del bot (`mibot2_v6.py:101`) hay una lista de errores que significan *sesión muerta*:
`Invalid CKC`, `catched an exception`, `Error connecting to device`,
`Error reading response from device`, `Error writing length to device`.
En la app esto no es "reintentar el track": es **relanzar el wrapper** y avisar.

### E4. Selección de calidad (`select_media_url`, :282-345)

- Las variantes se ordenan por **ancho de banda descendente** y gana la primera que quepa bajo el
  tope → `alac-max` / `atmos-max` no son "la que pidas", son un techo.
- ALAC: el sample rate sale del penúltimo campo del grupo de audio; la etiqueta se arma con bit depth.
- **Atmos, rareza real**: si el bitrate del nombre tiene 4 dígitos y empieza con `2`, se le quita
  ese `2` (`2768` → `768`) antes de comparar. Si no se hace, ninguna variante pasa el filtro.
- `ac-3` se acepta como alternativa cuando se pidió atmos.
- AAC: se **saltan** las variantes `binaural` y `downmix`; `aac-lc` se normaliza a `aac`.
- Si nada pasa el filtro devuelve `None` → error "No <quality> stream available", no un fallback silencioso.

### E5. Descarga y RAM (confirmado, es el B1)

`:1882` `enc_buf = io.BytesIO()` — **el track cifrado entero vive en RAM**, y luego
`_defragment_mp4` arma otra copia completa. Encadenado, es el reventón de 4 GB.
Dato extra: **`max-memory-limit: 256` del config.yaml no lo lee nadie** (viene del tool en Go);
es un setting muerto, no una protección. En Rust: streaming a disco de punta a punta.

Detalle a conservar: los segmentos se **deduplican por URL** (`seen_urls`) porque el HLS de Apple
puede venir como un solo archivo repetido en todos los `#EXTINF` o como un `.m4a` por fragmento;
el dedup cubre los dos casos con el mismo código.

### E6. Orden del post-proceso (`download_track`, :1990-2000) — no se puede alterar

1. carátula (usa la pre-bajada si el álbum ya la trajo, si no la baja)
2. letras (solo si hay `media-user-token`) → `.lrc` aparte + embebidas
3. `write_tags` (incluye `©lyr`)
4. `_fix_fragmented_mp4_duration` — el fMP4 de Apple trae `stts` vacío y duración mala en el init
5. `_defragment_mp4` — fMP4 → MP4 clásico, **requerido por qaac y decoders tradicionales**

Los pasos 4 y 5 van **después** de tagear, no antes.

### E7. Otros

- `get_bearer_token`: se scrapea y se cachea **12 h**, en memoria y en `.bearer_cache.json`.
- `MAX_PARALLEL_TRACKS = 1`: descarga serializada a propósito (una sesión FairPlay, un socket).
- `_normalize_path` ya traduce `C:\...` → `/mnt/c/...` — pensado para correr bajo WSL.
- Si el descifrado falla, **se borra el archivo a medias** (`os.unlink`) y se devuelve error; nunca
  queda un .m4a corrupto en la carpeta.
- MV va por **Widevine**, no FairPlay: CDM propio en Python (`_WidevineCDM`, :2352) con device key
  embebida, licencia en `acquireWebPlaybackLicense`, descifrado `cbcs` y remux propio
  (`_mv_mux`, :2993). Es un subsistema completo aparte del audio.

## F. Reconstrucción del MP4 (lo más delicado del port)

### F1. `transform_init` / `_transform_stsd` (:909-1020) — quitar el cifrado del moov

- `enca` → códec real, que sale del `frma` dentro del `sinf`. Se conservan los **28 bytes** de
  cabecera de audio y los hijos que no sean `sinf`.
- **El `stsd` de Apple trae DOS entradas idénticas** tras descifrar → se queda solo la primera y
  se fuerza `entry_count = 1`. (Esto es el A4: sin esto ffmpeg no reconoce el stream.)
- Se eliminan `sbgp`/`sgpd` cuyo `grouping_type` sea `seig` o `seam`, y los `pssh`.
- El `tenc` (de `sinf/schi/tenc`) es de donde sale el `iv_size` y el códec para todo lo demás.

### F2. `_fix_fragmented_mp4_duration` (:499-685)

El init de Apple trae `stts` **vacío** y la duración solo del primer fragmento. Se reconstruye:

- `stts` se rearma recorriendo **todos** los `trun` de **todos** los `moof`.
- La duración por sample sale del `tfhd`; si el `tfhd` la omite, se usa el
  `default_sample_duration` del **`trex`** como respaldo.
- Se parchan `mdhd`, `tkhd`, `mvhd`, y el `elst`: si el primer entry tiene `segment_duration = 0`
  y `media_time > 0`, se corrige a `(total - media_time) × movie_ts / media_ts`.
- Multi-`trun`: todos los `trun` de un mismo `traf` apuntan al mdat que sigue inmediatamente.

### F3. `_defragment_mp4` (:685-909)

fMP4 → MP4 clásico, **obligatorio para qaac y reproductores tradicionales**.

- Si no hay `mvex`, no hace nada (ya está desfragmentado).
- **La marca del `ftyp` depende del códec**: `alac`/`mp4a` → `M4A `; `ec-3`/`ac-3` → `mp42`.
  No es cosmético, algunos decoders se guían por ahí.
- Se elimina `mvex` y se reconstruyen `stco` / `stsz` / `stsc` (un chunk por `moof`).
- **Dos pasadas**: la primera con `stco` de ceros para medir el tamaño del moov, la segunda con los
  offsets reales — y un `assert` de que el tamaño no cambió entre pasadas. Si cambiara, todos los
  offsets quedarían corridos (es el A6).

## G. Letras, portadas y artwork animado

- **TTML → LRC** (`_ttml_to_lrc`, :1490): el atributo `itunes:timing` puede ser `Line`, `Word` o
  `None`; con `None` se guarda texto plano sin marcas. Centésimas = `ms[:3] // 10`.
  Se intentan **dos endpoints en orden**: `/lyrics` y luego `/syllable-lyrics`.
  Requiere `media-user-token` de al menos 20 caracteres, si no ni se intenta.
- **Portada a resolución nativa** (`_cover_url_full_res`): usa el `width`/`height` que **reporta
  Apple** en el objeto artwork (no un tamaño fijo); el `cover-size` del config solo aplica a la
  carátula embebida.
- **Artwork animado** (:1670-1748): sale de `editorialVideo`, variantes `motionSquareVideo1x1` (1:1)
  y `motionTallVideo3x4` (3:4), con `motionDetailSquare`/`motionDetailTall` como respaldo.
  Truco clave: **el `EXT-X-MAP` de la variante ya ES el MP4 completo**, no hay que pegar segmentos.
  Se remuxea con `ffmpeg -c copy -movflags +faststart`. Es lo único del audio que necesita ffmpeg.

## H. Etiquetas (`write_tags`, :1588-1640)

Mapa completo: `©nam ©ART aART ©alb ©day(4 dígitos) ©wrt ©gen(solo el primero) trkn(n, total del
álbum) disk(n, 0) cprt covr ©lyr`, más freeform `com.apple.iTunes:LABEL / UPC / ISRC`, y
`rtng = 1` explicit / `2` clean. Se hace con mutagen sobre el archivo ya escrito.

## I. Music Video — subsistema aparte (~1.100 líneas, :2184-3432)

**No usa FairPlay ni el wrapper: usa Widevine**, con un CDM propio en Python (`_WidevineCDM`,
:2352) y device key embebida. Licencia en `acquireWebPlaybackLicense`, descifrado `cbcs` y
**muxer propio** (`_mv_mux`, :2993) — sin mp4decrypt ni MP4Box.

- El master sale de `webPlayback` con `salableAdamId`; si no hay `hls-playlist-url` el mensaje es
  "media-user-token may be wrong or expired" (es el síntoma real, no un error de red).
- **UA de Chrome obligatorio** (`_MV_UA`): con otro, Apple entrega menos resolución.
- Video: variantes por `AVERAGE-BANDWIDTH` desc, primera cuya **altura** quepa en `mv-max`; el
  tamaño se saca del propio nombre del URI (`_1920x1080`).
- Audio: prioridad por `GROUP-ID` (`audio-atmos` → `audio-ac3` → `audio-stereo-256`), desempate por
  el `_grN_` más alto. **Mejora nuestra sobre el original en Go**: los MV viejos solo publican
  `audio-stereo-128` o `audio-HE-stereo-64`, que no están en ninguna lista de prioridad; el binario
  Go se tragaba el error, bajaba el video entero y moría en el mux con un mensaje confuso. Aquí hay
  respaldo al mejor audio que el video ofrezca, y el HE-AAC se ordena **por debajo** de un estéreo
  del mismo bitrate nominal (`kbps - 1`) porque es paramétrico.
- **Tres `#EXT-X-KEY`** en la playlist (FairPlay `skd://`, PlayReady en UTF-16, Widevine): se elige
  la de Widevine **explícitamente por `KEYFORMAT`**, no por orden.

## J. Del bot (`mibot2_v6.py`) — lo que es producto, no infraestructura

- **Timeout por inactividad de 1200 s** (`read_process_output`, :1490): no es timeout total, es
  "no imprimió nada en 20 min" → matar. Un álbum largo legítimo puede tardar más que eso en total.
- **`_shutdown_requested`**: si la sesión FairPlay se cae a media descarga, se marca y se **deja de
  tomar trabajo nuevo** en vez de fallar track por track.
- **Prioridades de cola**: `PRIO_CACHED = -1` (ya cacheado, se adelanta a todo), `PRIO_PREMIUM = 0`,
  `PRIO_NORMAL = 1`.
- **Búsqueda**: mezcla amp-api + iTunes, deduplica por `nombre|artista` normalizado (quita
  `(feat. …)` y `[…]`) y **prefiere la versión que NO sea DJ mix** — si ya había una entrada de DJ
  mix, la reemplaza. Patrones: `[mixed]`, `dj mix`, `tomorrowland`, `club mix`, `festival mix`,
  `live set`, `at <lugar> <año>`.
- **Proxy de respaldo** (Webshare) que se prueba por conexión TCP antes de usarse; hay uno fijo
  hardcodeado como último recurso.
- Detección de sesión muerta por log: `-42786` o `Invalid CKC`.

## K. Diferencia entre las dos copias del downloader — resuelta

`/srv/repos/apple-music-downloader/downloader.py` es **superset estricto** de
`/srv/bots/apple/downloader.py`: mismas funciones más 10 (`_normalize_path`, `_apply_format`,
`_codec_display`, `_fmt_sample_rate`, `_cover_url_full_res`, `_extract_animated_artwork`,
`_resolve_animated_mp4`, `download_animated_artwork`, `save_cover_art`, `_tool`) — o sea
carátulas, artwork animado y helpers de formato. **Ninguna función del bot falta en el repo.**
→ **El canónico para el port es el del repo.**

## L. Lo único que queda fuera del inventario

- El bug del encoder ALAC (spike/drop en el offset 4095 de frames, ver [[project-alac-bug-research]]):
  es un defecto del **encoder de Apple**, no del downloader — no hay nada que portar, pero conviene
  decidir si la app trae el detector.
- Detalles finos del muxer de MV (`_mv_mux`, tablas stts/ctts/stsz/stsc/stco/stss): documentados a
  nivel de decisiones, no línea por línea. Se releen cuando se ataque la fase de MV.
