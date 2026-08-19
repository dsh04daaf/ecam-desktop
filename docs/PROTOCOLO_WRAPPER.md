# Runtime del wrapper en Windows — protocolo verificado

Todo lo de aquí está **comprobado en el VPS el 2026-08-19**, no es diseño en el aire.
Fuente: `/srv/bots/apple/wrapper_wol` (WOL, commit 0d4823d + fix DNS nuestro).

## 1. Por qué WSL y no un .exe

- `rootfs/system/bin/main` = ELF **x86-64 de Android**, se carga con `linker64` y 99 `.so`
  del APK de Apple Music (116 MB).
- `wrapper.c` hace `mount(bind /dev/urandom)` → `chroot()` → `unshare(CLONE_NEWPID)` →
  `mount proc`. Syscalls de Linux; en Windows no existen.
- `wrapper` externo solo depende de `libc` (verificado con `ldd`) → basta un rootfs mínimo.

## 2. La distro

| | |
|---|---|
| Base | busybox-static + glibc (libc.so.6 + ld-linux) + libnss/libresolv |
| Carga útil | `/app/wrapper`, `/app/rootfs/system` (Android), `/app/rootfs/data` (sesión) |
| Tamaño | **49 MB** comprimido / 120 MB en disco (la imagen Docker completa serían 94 MB) |
| Import | `wsl --import ECAM %LOCALAPPDATA%\ECAM\distro rootfs.tar.gz --version 2` |

`wsl.conf` incluido: automount off, interop off, `generateResolvConf=true` (hace falta DNS),
usuario por defecto root (el wrapper necesita CAP_SYS_ADMIN para el `mount`).

**Ojo con el cwd**: `wrapper.c` usa rutas relativas `./rootfs`, así que hay que lanzarlo
con `cd /app` siempre.

## 3. Estados que imprime el wrapper (stderr) → pantallas de la app

Capturado en vivo con credenciales falsas:

```
[+] starting...                                       → "Preparando…"
[+] initializing ctx...
[+] logging in...                                     → "Entrando a Apple Music…"
[.] credentialHandler: {…, 2FA: true|false}           → si true: MOSTRAR pantalla de código
[!] Enter your 2FA code into rootfs/…/2fa.txt
[!] Waiting for input...                              → ventana abierta: 20 sondeos × 3 s
[!] Code file detected! Logging in...                 → código aceptado
[!] Failed to get 2FA Code in 60s. Exiting...         → venció: relanzar login solo
[!] server message: <texto de Apple, ya traducido>    → mostrarlo tal cual
[!] auth error: code=928084600, message=…             → 928084600 = credenciales incorrectas
[!] login failed
[!] listening 0.0.0.0:10020 / :20020 / :30020         → 3 líneas = wrapper listo
```

## 4. Login sin comandos

El flag **`-F` / `--code-from-file`** (`wrapper.ggo`) cambia el `scanf` de stdin por un
sondeo de archivo (`main.c:203-226`). Eso es lo que permite una pantalla de login de verdad:

1. ¿Existe `…/files/mpl_db/kvs.sqlitedb`? → **sí: lanzar sin `-L`, sin re-login.**
2. No → pantalla usuario/clave → `cd /app && ./wrapper -L 'user:pass' -F -H 0.0.0.0`
3. Al ver `2FA: true` → pantalla de 6 dígitos (montarla **antes**, la ventana es de 60 s)
4. Escribir el código en `/app/rootfs/data/data/com.apple.android.music/files/2fa.txt`
5. 3 × `listening` → adentro. El **puerto 30020 devuelve el JSON de la cuenta** → mostrar
   "sesión iniciada como X · suscripción activa".

El mismo proceso que loguea es el que queda sirviendo: no hay dos arranques.

## 5. Supervisor (lo que hará la app)

- arranque: ¿WSL? → ¿distro? → ¿sesión? → lanzar → esperar `listening` (o TCP a 127.0.0.1:10020,
  WSL2 reenvía localhost solo si el wrapper bindea `0.0.0.0`)
- caída a media descarga → relanzar sin re-login y reintentar el track
- cierre de la app → matar el hijo + `wsl --terminate ECAM` (no dejar la VM comiendo RAM)
- "cerrar sesión" = borrar `kvs.sqlitedb`

## 6. Pendiente de verificar en Windows real

- [ ] `wsl --import` del tar slim y que el wrapper arranque ahí (probado solo bajo Docker)
- [ ] reenvío de localhost 10020/20020/30020 hacia Windows
- [ ] login completo con 2FA de punta a punta
- [ ] consumo de RAM de la distro en reposo y con descarga larga
