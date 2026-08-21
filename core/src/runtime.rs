//! Arranque y vigilancia del wrapper: es la máquina de estados del login.
//!
//! En Windows el wrapper vive dentro de una distro WSL propia (ver
//! `docs/PROTOCOLO_WRAPPER.md`) y se lanza como **proceso hijo**, así que su
//! stderr llega directo aquí. Eso es lo que permite una pantalla de login de
//! verdad: la app lee lo que el wrapper va diciendo y va cambiando de pantalla.

use crate::error::{Error, Result};
use serde::Serialize;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// Ruta dentro de la distro donde vive la sesión.
const DATA_DIR: &str = "/app/rootfs/data/data/com.apple.android.music/files";

/// Lo que se monta como volumen en Docker: `<data_dir>` del host va a parar a
/// `/app/rootfs/data`. Todo lo que hay debajo se puede tocar desde el host sin
/// que el contenedor esté encendido, que es justo lo que hace falta para saber
/// si hay sesión ANTES de arrancar nada.
const VOLUME_MOUNT: &str = "/app/rootfs/data";
const DATA_SUBPATH: &str = "data/com.apple.android.music/files";

/// Dónde corre el wrapper.
#[derive(Debug, Clone)]
pub enum Backend {
    /// Ya está corriendo y solo hay que hablarle (el del VPS, o uno a mano).
    External,
    /// Windows: distro WSL propia.
    Wsl { distro: String },
    /// Linux o desarrollo: el binario en una carpeta.
    Local { dir: PathBuf },
    /// macOS: contenedor `linux/arm64` **nativo**. Upstream publica build de
    /// aarch64 (wrapper, linker64 y las 99 `.so`, `libCoreLSKD` incluida), así
    /// que aquí no hay emulación ni Rosetta de por medio.
    Docker { image: String, container: String, data_dir: PathBuf },
}

impl Default for Backend {
    fn default() -> Self {
        if cfg!(windows) {
            Backend::Wsl { distro: "ECAM".into() }
        } else if cfg!(target_os = "macos") {
            // En macOS no hay Linux integrado como WSL: el motor va en un
            // contenedor. La sesión vive en el host, dentro del volumen, para
            // que sobreviva a borrar y recrear el contenedor.
            Backend::Docker {
                image: "ecam:arm64".into(),
                container: "ecam".into(),
                data_dir: default_data_dir(),
            }
        } else {
            // Linux/desarrollo: se le habla por TCP a uno que ya esté corriendo.
            Backend::External
        }
    }
}

impl Backend {
    /// Nombre corto para la ventana. La UI decide con esto qué pantalla enseñar:
    /// con `external` no hay nada que instalar ni ningún login que hacer desde
    /// aquí, solo un motor al que conectarse.
    pub fn kind(&self) -> &'static str {
        match self {
            Backend::External => "external",
            Backend::Wsl { .. } => "wsl",
            Backend::Local { .. } => "local",
            Backend::Docker { .. } => "docker",
        }
    }
}

/// Dónde guarda el host la carpeta que se monta dentro del contenedor.
fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ECAM")
        .join("wrapper-data")
}

/// Lo que la UI necesita saber para decidir qué pantalla enseñar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum Event {
    /// Arrancando el proceso.
    Starting,
    /// Entrando con usuario y contraseña.
    LoggingIn,
    /// **Hay que enseñar la pantalla del código YA**: la ventana es de 60 s.
    NeedsTwoFactor,
    TwoFactorAccepted,
    /// Se pasaron los 60 s y el wrapper se cerró solo: hay que reintentar.
    TwoFactorExpired,
    /// Mensaje de Apple, ya traducido por ellos. Se enseña tal cual.
    ServerMessage(String),
    AuthError { code: String },
    LoginFailed,
    /// Uno de los tres puertos ya escucha.
    Listening(u16),
    /// Los tres puertos escuchan: dentro.
    Ready,
    /// La sesión murió (ver `wrapper::FATAL_ERRORS`): hay que relanzar.
    SessionDead(String),
    /// Cualquier otra línea, para el log de diagnóstico.
    Log(String),
}

/// Traduce una línea del wrapper a un estado de la UI.
///
/// Los textos salen de `main.c`; están aquí en un solo sitio para que se vea de
/// un vistazo qué se está esperando y para poder probarlo sin lanzar nada.
pub fn parse_line(line: &str) -> Event {
    let l = line.trim();
    if l.contains("2FA: true") {
        return Event::NeedsTwoFactor;
    }
    if l.contains("Code file detected") {
        return Event::TwoFactorAccepted;
    }
    if l.contains("Failed to get 2FA Code") {
        return Event::TwoFactorExpired;
    }
    if let Some(msg) = l.split("server message: ").nth(1) {
        return Event::ServerMessage(msg.trim().to_string());
    }
    if let Some(rest) = l.split("auth error: code=").nth(1) {
        let code = rest.split(',').next().unwrap_or("").trim().to_string();
        return Event::AuthError { code };
    }
    if l.contains("login failed") {
        return Event::LoginFailed;
    }
    if l.contains("logging in") {
        return Event::LoggingIn;
    }
    if l.contains("starting") {
        return Event::Starting;
    }
    if l.contains("listening") {
        // "[!] listening 0.0.0.0:10020" o "listening m3u8 request on 0.0.0.0:20020"
        if let Some(port) = l.rsplit(':').next().and_then(|p| p.trim().parse::<u16>().ok()) {
            return Event::Listening(port);
        }
    }
    if crate::wrapper::is_fatal_log(l) {
        return Event::SessionDead(l.to_string());
    }
    Event::Log(l.to_string())
}

/// Mensaje en cristiano para los códigos de error de Apple que ya conocemos.
pub fn auth_error_message(code: &str) -> &'static str {
    match code {
        "928084600" => "Usuario o contraseña incorrectos.",
        "1112" | "-1112" => "La cuenta necesita verificación en un dispositivo de confianza.",
        "2034" | "-2034" => "Contraseña incorrecta.",
        _ => "Apple rechazó el inicio de sesión.",
    }
}

/// Sin esto, cada llamada a `wsl.exe` abre una consola negra encima de la app.
/// Es la bandera CREATE_NO_WINDOW de Windows; en Linux no existe y no hace nada.
#[cfg(windows)]
fn no_console(cmd: &mut tokio::process::Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000);
}
#[cfg(not(windows))]
fn no_console(_cmd: &mut tokio::process::Command) {}

pub struct Runtime {
    pub backend: Backend,
    /// `host:puerto` del puerto de descifrado, para comprobar que responde.
    pub decrypt_port: String,
}

impl Runtime {
    pub fn new(backend: Backend, decrypt_port: impl Into<String>) -> Self {
        Self { backend, decrypt_port: decrypt_port.into() }
    }

    /// Orden que lanza el wrapper. Se construye aparte para poder probarla.
    ///
    /// Siempre con `cd /app`: `wrapper.c` usa rutas relativas (`./rootfs`) y
    /// desde otro directorio no encuentra nada.
    pub fn launch_command(&self, creds: Option<(&str, &str)>) -> (String, Vec<String>) {
        let inner = match creds {
            // `-F` hace que el 2FA se lea de un archivo en vez de stdin: es lo
            // que permite pedirlo por pantalla.
            Some((user, pass)) => format!("cd /app && exec ./wrapper -L '{user}:{pass}' -F -H 0.0.0.0"),
            None => "cd /app && exec ./wrapper -H 0.0.0.0".to_string(),
        };
        match &self.backend {
            Backend::Wsl { distro } => (
                "wsl.exe".into(),
                vec!["-d".into(), distro.clone(), "-u".into(), "root".into(), "--".into(), "/bin/sh".into(), "-c".into(), inner],
            ),
            Backend::Local { dir } => (
                "/bin/sh".into(),
                vec!["-c".into(), inner.replace("cd /app", &format!("cd {}", dir.display()))],
            ),
            Backend::External => ("true".into(), vec![]),
            // Los puertos se publican SOLO en 127.0.0.1: por el 10020 viajan las
            // llaves de FairPlay y el audio en claro, sin cifrado ni
            // autenticación, así que no puede quedar escuchando a la red.
            // `--privileged` no es opcional: el wrapper hace chroot, unshare de
            // PID y monta /proc.
            Backend::Docker { image, container, data_dir } => (
                "docker".into(),
                vec![
                    "run".into(),
                    "--rm".into(),
                    "--name".into(),
                    container.clone(),
                    "--privileged".into(),
                    "-p".into(), "127.0.0.1:10020:10020".into(),
                    "-p".into(), "127.0.0.1:20020:20020".into(),
                    "-p".into(), "127.0.0.1:30020:30020".into(),
                    "-v".into(),
                    format!("{}:{VOLUME_MOUNT}", data_dir.display()),
                    image.clone(),
                    "/bin/sh".into(),
                    "-c".into(),
                    inner,
                ],
            ),
        }
    }

    /// Ruta EN EL HOST de un archivo de la sesión. Solo tiene sentido con
    /// Docker, donde esa carpeta está montada como volumen.
    fn host_data_file(&self, name: &str) -> Option<PathBuf> {
        let Backend::Docker { data_dir, .. } = &self.backend else { return None };
        debug_assert!(DATA_DIR.ends_with(DATA_SUBPATH), "las dos rutas tienen que casar");
        Some(data_dir.join(DATA_SUBPATH).join(name))
    }

    /// Un contenedor con el mismo nombre parado de una sesión anterior hace que
    /// `docker run --name` falle en seco. Se limpia antes de cada arranque.
    async fn drop_stale_container(&self) {
        let Backend::Docker { container, .. } = &self.backend else { return };
        let mut cmd = tokio::process::Command::new("docker");
        cmd.args(["rm", "-f", container])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        no_console(&mut cmd);
        let _ = cmd.status().await;
    }

    /// Corre un comando dentro de la distro y devuelve si salió bien.
    async fn run_in_distro(&self, script: &str) -> Result<bool> {
        let (program, mut args) = match &self.backend {
            Backend::Wsl { distro } => (
                "wsl.exe".to_string(),
                vec!["-d".into(), distro.clone(), "-u".into(), "root".into(), "--".into(), "/bin/sh".into(), "-c".into()],
            ),
            Backend::Local { .. } => ("/bin/sh".to_string(), vec!["-c".to_string()]),
            Backend::Docker { container, .. } => (
                "docker".to_string(),
                vec!["exec".into(), container.clone(), "/bin/sh".into(), "-c".into()],
            ),
            Backend::External => return Ok(false),
        };
        args.push(script.to_string());
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        no_console(&mut cmd);
        let status = cmd.status().await?;
        Ok(status.success())
    }

    /// ¿Existe ya la distro?
    pub async fn distro_installed(&self) -> bool {
        match &self.backend {
            Backend::Wsl { distro } => tokio::process::Command::new("wsl.exe")
                .args(["-l", "-q"])
                .output()
                .await
                .map(|o| {
                    // `wsl -l -q` sale en UTF-16: se quitan los nulos antes de mirar.
                    let text: String = String::from_utf8_lossy(&o.stdout).chars().filter(|c| *c != '\0').collect();
                    text.lines().any(|l| l.trim() == distro)
                })
                .unwrap_or(false),
            // Con Docker lo que hace falta es la imagen. Si Docker no está
            // instalado o no está arrancado, esto también sale false y la app
            // manda a la pantalla de preparar el motor, que es donde se explica.
            Backend::Docker { image, .. } => {
                let mut cmd = tokio::process::Command::new("docker");
                cmd.args(["image", "inspect", image])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                no_console(&mut cmd);
                cmd.status().await.map(|s| s.success()).unwrap_or(false)
            }
            _ => true,
        }
    }

    /// ¿Está Docker instalado y respondiendo? Sirve para distinguir «falta la
    /// imagen» de «no hay Docker», que se arreglan de formas muy distintas.
    pub async fn docker_ready(&self) -> bool {
        if !matches!(self.backend, Backend::Docker { .. }) {
            return true;
        }
        let mut cmd = tokio::process::Command::new("docker");
        cmd.args(["info", "--format", "{{.ServerVersion}}"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        no_console(&mut cmd);
        cmd.status().await.map(|s| s.success()).unwrap_or(false)
    }

    /// Importa la distro desde el `.tar.gz`. No hace falta ser administrador.
    pub async fn import_distro(&self, tarball: &std::path::Path, target_dir: &std::path::Path) -> Result<()> {
        if let Backend::Docker { data_dir, .. } = &self.backend {
            if !self.docker_ready().await {
                return Err(Error::Other(
                    "Docker no responde. Instala Docker Desktop o Colima y déjalo arrancado."
                        .into(),
                ));
            }
            // La carpeta del volumen tiene que existir ANTES del primer `run`, o
            // Docker la crea él como root y luego la app no puede escribir el
            // 2fa.txt dentro.
            tokio::fs::create_dir_all(data_dir.join(DATA_SUBPATH)).await?;
            let mut cmd = tokio::process::Command::new("docker");
            cmd.arg("load").arg("-i").arg(tarball);
            no_console(&mut cmd);
            let out = cmd.output().await?;
            if !out.status.success() {
                return Err(Error::Other(format!(
                    "no se pudo cargar la imagen: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            return Ok(());
        }
        let Backend::Wsl { distro } = &self.backend else { return Ok(()) };
        tokio::fs::create_dir_all(target_dir).await?;
        let status = tokio::process::Command::new("wsl.exe")
            .arg("--import")
            .arg(distro)
            .arg(target_dir)
            .arg(tarball)
            .args(["--version", "2"])
            .status()
            .await?;
        if !status.success() {
            return Err(Error::Other(
                "no se pudo importar la distro. ¿Está WSL instalado? (`wsl --install --no-distribution`)".into(),
            ));
        }
        Ok(())
    }

    /// ¿Hay sesión guardada? Es lo que decide entre pedir login o entrar directo.
    pub async fn has_session(&self) -> bool {
        // Con Docker se mira el archivo EN EL HOST: el contenedor no está
        // encendido todavía cuando hay que decidir entre login y entrar.
        if let Some(db) = self.host_data_file("mpl_db/kvs.sqlitedb") {
            return db.exists();
        }
        match self.backend {
            Backend::External => crate::wrapper::Wrapper::probe(&self.decrypt_port),
            _ => self
                .run_in_distro(&format!("[ -f {DATA_DIR}/mpl_db/kvs.sqlitedb ]"))
                .await
                .unwrap_or(false),
        }
    }

    /// Cierra la sesión borrando la base de cuentas.
    pub async fn sign_out(&self) -> Result<()> {
        if let Some(db) = self.host_data_file("mpl_db/kvs.sqlitedb") {
            self.drop_stale_container().await;
            // Que no exista ya no es un error: es justo lo que se buscaba.
            match tokio::fs::remove_file(&db).await {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e.into()),
            }
        }
        self.run_in_distro(&format!("rm -f {DATA_DIR}/mpl_db/kvs.sqlitedb")).await?;
        Ok(())
    }

    /// Entrega el código de 2FA. El wrapper lo sondea cada 3 s durante 60 s.
    pub async fn submit_two_factor(&self, code: &str) -> Result<()> {
        let code: String = code.chars().filter(|c| c.is_ascii_digit()).take(6).collect();
        if code.len() != 6 {
            return Err(Error::Other("el código son 6 dígitos".into()));
        }
        // Por el volumen, no por `docker exec`: el wrapper sondea ese archivo
        // cada 3 s durante 60 s, y escribirlo desde el host es una llamada menos
        // que puede fallar dentro de una ventana que ya es corta.
        if let Some(path) = self.host_data_file("2fa.txt") {
            if let Some(dir) = path.parent() {
                tokio::fs::create_dir_all(dir).await?;
            }
            tokio::fs::write(&path, code.as_bytes()).await?;
            return Ok(());
        }
        let ok = self
            .run_in_distro(&format!("printf '%s' '{code}' > {DATA_DIR}/2fa.txt"))
            .await?;
        if !ok {
            return Err(Error::Other("no se pudo entregar el código al wrapper".into()));
        }
        Ok(())
    }

    /// Apaga la distro para que no siga comiendo RAM al cerrar la app.
    pub async fn shutdown(&self) {
        if matches!(self.backend, Backend::Docker { .. }) {
            self.drop_stale_container().await;
            return;
        }
        if let Backend::Wsl { distro } = &self.backend {
            let _ = tokio::process::Command::new("wsl.exe").args(["--terminate", distro]).status().await;
        }
    }

    /// Lanza el wrapper y va mandando por el canal lo que dice.
    ///
    /// Devuelve el proceso hijo para poder matarlo al cerrar. Si `creds` es
    /// `None` se arranca con la sesión guardada: **no se re-loguea si no hace
    /// falta**, igual que hace el bot.
    pub async fn start(
        &self,
        creds: Option<(&str, &str)>,
    ) -> Result<(tokio::process::Child, mpsc::Receiver<Event>)> {
        self.drop_stale_container().await;
        let (program, args) = self.launch_command(creds);
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        no_console(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("no se pudo lanzar el wrapper: {e}")))?;

        let stderr = child.stderr.take().ok_or_else(|| Error::Other("sin stderr del wrapper".into()))?;
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut listening = 0;
            while let Ok(Some(line)) = lines.next_line().await {
                let ev = parse_line(&line);
                let ready = matches!(ev, Event::Listening(_)) && {
                    listening += 1;
                    listening >= 3
                };
                if tx.send(ev).await.is_err() {
                    break;
                }
                if ready && tx.send(Event::Ready).await.is_err() {
                    break;
                }
            }
        });

        Ok((child, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docker_backend() -> Runtime {
        Runtime::new(
            Backend::Docker {
                image: "ecam:arm64".into(),
                container: "ecam".into(),
                data_dir: PathBuf::from("/tmp/ecam-data"),
            },
            "127.0.0.1:10020",
        )
    }

    #[test]
    fn docker_publica_los_puertos_solo_en_localhost() {
        let (prog, args) = docker_backend().launch_command(None);
        assert_eq!(prog, "docker");
        let publicados: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| *i > 0 && args[i - 1] == "-p")
            .map(|(_, a)| a)
            .collect();
        assert_eq!(publicados.len(), 3, "faltan puertos: {args:?}");
        for p in publicados {
            // Por el 10020 viajan las llaves de FairPlay y el audio en claro. Si
            // esto se publica en 0.0.0.0, queda abierto a toda la red.
            assert!(p.starts_with("127.0.0.1:"), "puerto abierto a la red: {p}");
        }
    }

    #[test]
    fn docker_necesita_privileged_y_el_volumen() {
        let (_, args) = docker_backend().launch_command(None);
        // El wrapper hace chroot, unshare de PID y monta /proc: sin esto no
        // arranca, y el error que da no lo dice.
        assert!(args.iter().any(|a| a == "--privileged"), "{args:?}");
        assert!(
            args.iter().any(|a| a == &format!("/tmp/ecam-data:{VOLUME_MOUNT}")),
            "sin volumen la sesión se pierde al recrear el contenedor: {args:?}"
        );
    }

    #[test]
    fn docker_pide_el_2fa_por_archivo_al_loguearse() {
        let (_, args) = docker_backend().launch_command(Some(("u@x.com", "p")));
        let inner = args.last().unwrap();
        // `-F` es lo que hace que el código se lea de un archivo en vez de stdin:
        // sin eso no hay pantalla de 2FA posible.
        assert!(inner.contains("-F"), "{inner}");
        assert!(inner.starts_with("cd /app &&"), "el wrapper usa rutas relativas: {inner}");
    }

    #[test]
    fn la_ruta_de_la_sesion_en_el_host_casa_con_la_de_dentro() {
        // Si alguien toca una de las dos constantes y no la otra, la app busca
        // la sesión donde no está y manda a re-loguear sin motivo.
        assert!(DATA_DIR.ends_with(DATA_SUBPATH));
        assert_eq!(DATA_DIR, format!("{VOLUME_MOUNT}/{DATA_SUBPATH}"));
        let rt = docker_backend();
        assert_eq!(
            rt.host_data_file("mpl_db/kvs.sqlitedb").unwrap(),
            PathBuf::from("/tmp/ecam-data/data/com.apple.android.music/files/mpl_db/kvs.sqlitedb")
        );
    }

    #[test]
    fn sin_docker_no_hay_rutas_de_host() {
        let rt = Runtime::new(Backend::Wsl { distro: "ECAM".into() }, "127.0.0.1:10020");
        assert!(rt.host_data_file("2fa.txt").is_none());
        assert_eq!(rt.backend.kind(), "wsl");
        assert_eq!(docker_backend().backend.kind(), "docker");
    }

    #[test]
    fn traduce_las_lineas_del_wrapper() {
        assert_eq!(parse_line("[+] starting..."), Event::Starting);
        assert_eq!(
            parse_line("[.] credentialHandler: {title: , message: , 2FA: true}"),
            Event::NeedsTwoFactor
        );
        assert_eq!(parse_line("[!] Code file detected! Logging in..."), Event::TwoFactorAccepted);
        assert_eq!(parse_line("[!] Failed to get 2FA Code in 60s. Exiting..."), Event::TwoFactorExpired);
        assert_eq!(
            parse_line("[!] server message: Check the account information you entered and try again."),
            Event::ServerMessage("Check the account information you entered and try again.".into())
        );
        assert_eq!(
            parse_line("[!] auth error: code=928084600, message=iTunesStoreErrorCategory"),
            Event::AuthError { code: "928084600".into() }
        );
        assert_eq!(parse_line("[!] login failed"), Event::LoginFailed);
        assert_eq!(parse_line("[!] listening 0.0.0.0:10020"), Event::Listening(10020));
        assert_eq!(
            parse_line("[!] listening m3u8 request on 0.0.0.0:20020"),
            Event::Listening(20020)
        );
        assert_eq!(parse_line("[!] Invalid CKC error"), Event::SessionDead("[!] Invalid CKC error".into()));
    }

    #[test]
    fn el_2fa_falso_no_se_confunde_con_el_de_verdad() {
        assert_ne!(
            parse_line("[.] credentialHandler: {title: , message: , 2FA: false}"),
            Event::NeedsTwoFactor
        );
    }

    #[test]
    fn el_comando_de_login_lleva_el_flag_del_archivo_de_codigo() {
        let rt = Runtime::new(Backend::Wsl { distro: "ECAM".into() }, "127.0.0.1:10020");
        let (prog, args) = rt.launch_command(Some(("a@b.com", "clave")));
        assert_eq!(prog, "wsl.exe");
        let script = args.last().unwrap();
        assert!(script.starts_with("cd /app &&"), "el wrapper usa rutas relativas");
        assert!(script.contains("-L 'a@b.com:clave'"));
        assert!(script.contains(" -F "), "sin -F el 2FA se pediría por stdin");
        assert!(script.contains("-H 0.0.0.0"), "hay que bindear fuera para que Windows llegue");
    }

    #[test]
    fn sin_credenciales_se_arranca_sin_reloguear() {
        let rt = Runtime::new(Backend::Wsl { distro: "ECAM".into() }, "127.0.0.1:10020");
        let (_, args) = rt.launch_command(None);
        let script = args.last().unwrap();
        assert!(!script.contains("-L"), "con sesión guardada NO se vuelve a loguear");
    }

    #[test]
    fn los_codigos_de_apple_tienen_mensaje_en_cristiano() {
        assert_eq!(auth_error_message("928084600"), "Usuario o contraseña incorrectos.");
        assert!(auth_error_message("999").contains("Apple"));
    }
}
