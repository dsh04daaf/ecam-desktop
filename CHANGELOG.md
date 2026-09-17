# Changelog

Las versiones de Windows llevan tag `v*` y las de macOS `mac-v*`; el número es el
mismo y el core es compartido, así que casi todo lo de abajo vale para las dos.

## 0.2.4 — 2026-09-17

### El key server se detecta pidiéndole la plantilla, no abriendo un socket

La detección era un `TcpStream::connect` al puerto del key server. **Docker acepta
la conexión en un puerto publicado aunque dentro no escuche nadie**, así que al
publicar el 40020 sobre la imagen arm64 —que no trae key server— la app creía que
sí lo había, elegía el motor local y la descarga moría con `el key server no
responde`. Ahora se le pide la plantilla de **prefetch** (`adamId=0`, que no gasta
licencia) y sólo cuenta si contesta con ella. El resultado se cachea por motor,
porque la comprobación corre en cada pista.

- **macOS: ya no se publica el 40020.** Publicar un puerto que nadie escucha es
  peor que no publicarlo: convierte un camino alternativo que funcionaba en un
  fallo duro. Hay una prueba que exige que no esté.

## 0.2.3 — 2026-09-17

### «Hay sesión» se mide por la cuenta cacheada, no por las bases del wrapper

En macOS se iniciaba sesión, se entraba a la pantalla principal y la app volvía al
login de inmediato; al reabrirla, login otra vez.

Medido lanzando el motor igual que lo lanza la app: **el wrapper crea todas sus
bases (`accounts`, `cookies`, `httpcache`, `kvs`) en el primer arranque, sin cuenta
y sin haber hecho login** — la distro de Windows bajo `files/mpl_db/` y la imagen
arm64 directamente bajo `files/`. Así que `kvs.sqlitedb` no probaba nada: mirar
sólo `mpl_db/` daba «no hay sesión» en macOS, y mirar las dos rutas daba «hay
sesión» en cuanto el motor arrancaba una vez (y entonces el wrapper subía sin
credenciales y el `login failed` devolvía al login igual).

Ahora se miran `STOREFRONT_ID` y `MUSIC_TOKEN` (no vacíos), que el wrapper escribe
sólo **después** de cachear la cuenta y **antes** de abrir sus puertos, así que
cuando la app recibe el «ya escucho» ya están. `sign_out` los borra también.

## 0.2.2 — 2026-09-17

Intento de arreglar lo anterior mirando las dos rutas de la base de datos.
**No sirvió** (ver 0.2.3): cambiaba un falso negativo por un falso positivo.

## 0.2.1 — 2026-09-17

### Si el motor no arranca, la app lo dice

Cuando el motor se cerraba al arrancar, nadie miraba su salida: la pantalla se
quedaba esperando un «ya escucho» que no iba a llegar, sin nada que leer. Ahora se
emite el motivo, traducido a los tres idiomas: sin privilegios (el botón *Run* de
Docker Desktop lanza sin `--privileged`), falta la imagen, puerto ocupado, Docker
apagado, permisos del socket, sin espacio y arquitectura equivocada. Si ya salió un
mensaje mejor —código caducado, contraseña incorrecta, aviso de Apple— el genérico
se calla.

## 0.2.0 — 2026-09-17

- **Catálogo viejo (AAC 256 kbps) por Widevine.** Las pistas sin `enhancedHls` ya
  no se saltan: Apple las licencia por Widevine con la misma cuenta.
- **Los videoclips de un álbum se bajan con el álbum**, numerados en su carpeta.
- **Omitidas aparte de los errores.** Lo que el catálogo ya decía que no existe
  (fuera de la tienda, o sin la versión pedida) se marca `↷`, no `✗`.
- Primer Release de Windows.
