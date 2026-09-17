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

/// Qué prueba que hay una cuenta dentro, **relativo a `files/`**.
///
/// ⚠️ NO vale mirar `kvs.sqlitedb`: **medido**, el wrapper crea todas sus bases
/// (`accounts`, `cookies`, `httpcache`, `kvs`) en el primer arranque, sin
/// cuenta y sin haber hecho login — aquí (x86) en `files/mpl_db/` y el arm64 de
/// la versión de Mac en `files/`. Servía para nada: falso negativo en Mac
/// (login en bucle) y falso positivo si se miran las dos rutas (la app cree que
/// hay sesión, lanza el wrapper sin credenciales y el `login failed` devuelve al
/// login igual).
///
/// Estos dos los escribe el wrapper **sólo tras cachear la cuenta**
/// (`write_storefront_id`/`write_music_token`), en `files/` e igual en los dos
/// builds, y ANTES de abrir los puertos, así que al llegar el `Ready` ya están.
/// Se exige que no estén vacíos.
const SESSION_MARKERS: [&str; 2] = ["STOREFRONT_ID", "MUSIC_TOKEN"];

/// Lo que hay que borrar para cerrar sesión: las bases de la cuenta en las dos
/// disposiciones, más los marcadores (si se dejan, seguiría "habiendo sesión").
const SESSION_DB_PATHS: [&str; 2] = ["mpl_db/kvs.sqlitedb", "kvs.sqlitedb"];

/// Dónde corre el wrapper.
#[derive(Debug, Clone)]
pub enum Backend {
    /// Ya está corriendo y solo hay que hablarle (el del VPS, o uno a mano).
    External,
    /// Windows: distro WSL propia.
    Wsl { distro: String },
    /// Linux o desarrollo: el binario en una carpeta.
    Local { dir: PathBuf },
}

impl Default for Backend {
    fn default() -> Self {
        if cfg!(windows) {
            Backend::Wsl { distro: "ECAM".into() }
        } else {
            Backend::External
        }
    }
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
    /// El motor se cerró **sin llegar a escuchar**, o sea que no arrancó.
    ///
    /// Sin esto la UI se quedaba esperando un `Ready` que no iba a llegar y en
    /// pantalla no salía nada: el síntoma que dieron dos intentos en Mac. Lleva
    /// el motivo ya traducido a algo que se pueda accionar (ver `startup_hint`)
    /// y la última línea cruda para el log.
    Exited { reason: String, last_line: String },
    /// Cualquier otra línea, para el log de diagnóstico.
    Log(String),
}

/// Por qué no arrancó el motor, a partir de lo que dejó en stderr.
///
/// Se mira TODO lo que escribió (el `docker run` y el wrapper comparten el
/// mismo stderr), porque los fallos de arranque son de dos familias: los del
/// propio Docker (imagen que falta, puerto ocupado, demonio apagado) y los del
/// wrapper ya dentro del contenedor.
///
/// El caso que costó dos intentos en Mac es el primero: un contenedor **sin
/// privilegios** no puede montar `/dev/urandom` ni `/proc`, y el wrapper solo
/// decía `Operation not permitted`, que no le dice nada a nadie. Docker Desktop
/// lanza sin `--privileged` cuando le das al botón Run de su interfaz, así que
/// es un error fácil de encontrarse.
pub fn startup_hint(log: &str) -> &'static str {
    // Se devuelve una CLAVE de i18n, no un texto: la app habla tres idiomas y
    // quien ve esto suele ser alguien instalándola por primera vez.
    // El orden importa: lo más específico primero.
    if log.contains("Operation not permitted")
        && (log.contains("mount /dev/urandom") || log.contains("mount proc"))
    {
        return "exit_sin_privilegios";
    }
    if log.contains("Unable to find image") || log.contains("No such image") {
        return "exit_sin_imagen";
    }
    if log.contains("port is already allocated") || log.contains("address already in use") {
        return "exit_puerto_ocupado";
    }
    if log.contains("Cannot connect to the Docker daemon") || log.contains("daemon is not running") {
        return "exit_sin_docker";
    }
    if log.contains("permission denied") && log.contains("docker.sock") {
        return "exit_socket_docker";
    }
    if log.contains("no space left on device") {
        return "exit_sin_espacio";
    }
    if log.contains("exec format error") {
        return "exit_arquitectura";
    }
    "exit_generico"
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
        }
    }

    /// Corre un comando dentro de la distro y devuelve si salió bien.
    async fn run_in_distro(&self, script: &str) -> Result<bool> {
        let (program, mut args) = match &self.backend {
            Backend::Wsl { distro } => (
                "wsl.exe".to_string(),
                vec!["-d".into(), distro.clone(), "-u".into(), "root".into(), "--".into(), "/bin/sh".into(), "-c".into()],
            ),
            Backend::Local { .. } => ("/bin/sh".to_string(), vec!["-c".to_string()]),
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
            _ => true,
        }
    }

    /// Importa la distro desde el `.tar.gz`. No hace falta ser administrador.
    pub async fn import_distro(&self, tarball: &std::path::Path, target_dir: &std::path::Path) -> Result<()> {
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
        match self.backend {
            Backend::External => crate::wrapper::Wrapper::probe(&self.decrypt_port),
            _ => {
                // `-s` = existe y no está vacío.
                let prueba = SESSION_MARKERS
                    .iter()
                    .map(|r| format!("[ -s {DATA_DIR}/{r} ]"))
                    .collect::<Vec<_>>()
                    .join(" || ");
                self.run_in_distro(&prueba).await.unwrap_or(false)
            }
        }
    }

    /// Cierra la sesión borrando la base de cuentas.
    ///
    /// Con su `-wal` y su `-shm`: la base está en modo WAL y lo último escrito
    /// (el login) vive en el `-wal` hasta que SQLite lo vuelca. Borrar solo el
    /// `.sqlitedb` deja ese WAL huérfano, y al crear la base nueva SQLite lo
    /// reaplica encima: resucita datos de la sesión cerrada.
    pub async fn sign_out(&self) -> Result<()> {
        let borrar = SESSION_DB_PATHS
            .iter()
            .flat_map(|r| ["", "-wal", "-shm"].map(move |s| format!("{DATA_DIR}/{r}{s}")))
            .chain(SESSION_MARKERS.iter().map(|r| format!("{DATA_DIR}/{r}")))
            .collect::<Vec<_>>()
            .join(" ");
        self.run_in_distro(&format!("rm -f {borrar}")).await?;
        Ok(())
    }

    /// Entrega el código de 2FA. El wrapper lo sondea cada 3 s durante 60 s.
    pub async fn submit_two_factor(&self, code: &str) -> Result<()> {
        let code: String = code.chars().filter(|c| c.is_ascii_digit()).take(6).collect();
        if code.len() != 6 {
            return Err(Error::Other("el código son 6 dígitos".into()));
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
            // Para el diagnóstico si el motor se cierra sin arrancar. El stderr
            // es el mismo para `docker run` y para el wrapper, así que aquí caen
            // los dos tipos de fallo. Se guarda acotado: el arranque son unas
            // pocas líneas y no queremos crecer sin tope.
            let mut arranque = String::new();
            let mut ultima = String::new();
            let mut arrancado = false;
            let mut explicado = false;
            while let Ok(Some(line)) = lines.next_line().await {
                let ev = parse_line(&line);
                let ready = matches!(ev, Event::Listening(_)) && {
                    listening += 1;
                    listening >= 3
                };
                // Estos ya le dicen al usuario qué pasó (contraseña mala, código
                // caducado, mensaje de Apple). Si luego el proceso se cierra, el
                // aviso genérico solo taparía el bueno.
                if matches!(
                    ev,
                    Event::TwoFactorExpired
                        | Event::AuthError { .. }
                        | Event::LoginFailed
                        | Event::ServerMessage(_)
                ) {
                    explicado = true;
                }
                if !arrancado {
                    if arranque.len() < 8192 {
                        arranque.push_str(&line);
                        arranque.push('\n');
                    }
                    // El ruido del linker de Android no explica nada y tapa la
                    // línea que sí importa.
                    let l = line.trim();
                    if !l.is_empty() && !l.contains("WARNING: linker:") && !l.contains("bionic_open_tzdata") {
                        ultima = l.to_string();
                    }
                }
                if tx.send(ev).await.is_err() {
                    return;
                }
                if ready {
                    arrancado = true;
                    if tx.send(Event::Ready).await.is_err() {
                        return;
                    }
                }
            }
            // stderr cerrado = el proceso terminó. Si nunca llegó a escuchar los
            // tres puertos, no arrancó: hay que DECIRLO. Callarse es lo que
            // dejaba la pantalla en blanco esperando para siempre.
            if !arrancado && !explicado {
                let _ = tx
                    .send(Event::Exited {
                        reason: startup_hint(&arranque).to_string(),
                        last_line: ultima,
                    })
                    .await;
            }
        });

        Ok((child, rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El error que dejó a dos Mac sin arrancar y sin explicación. Un
    /// contenedor sin `--privileged` no puede montar nada, y el mensaje crudo
    /// del kernel no le dice al usuario qué hacer.
    #[test]
    fn un_contenedor_sin_privilegios_se_reconoce() {
        let log = "mount /dev/urandom failed: Operation not permitted\n";
        assert_eq!(startup_hint(log), "exit_sin_privilegios");
        // También cuando lo que falla es /proc, ya dentro del chroot.
        assert_eq!(
            startup_hint("mount proc failed: Operation not permitted"),
            "exit_sin_privilegios"
        );
    }
    /// `docker run` escribe en el MISMO stderr que el wrapper, así que sus
    /// fallos también se diagnostican aquí.
    #[test]
    fn los_fallos_de_docker_tambien_se_reconocen() {
        for (log, esperado) in [
            ("Unable to find image 'ecam:arm64' locally", "exit_sin_imagen"),
            ("docker: Error response from daemon: driver failed: port is already allocated.", "exit_puerto_ocupado"),
            ("Cannot connect to the Docker daemon at unix:///var/run/docker.sock.", "exit_sin_docker"),
            ("permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock", "exit_socket_docker"),
            ("write /app: no space left on device", "exit_sin_espacio"),
            ("exec /app/wrapper: exec format error", "exit_arquitectura"),
            ("algo que no sabemos interpretar", "exit_generico"),
        ] {
            assert_eq!(startup_hint(log), esperado, "log: {log}");
        }
    }
    /// Un `Operation not permitted` de cualquier otra cosa no debe hacerse pasar
    /// por falta de privilegios: mandaría a arreglar lo que no es.
    #[test]
    fn no_confunde_otros_operation_not_permitted() {
        assert_eq!(startup_hint("chmod failed: Operation not permitted"), "exit_generico");
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
        assert_eq!(
            parse_line("[!] catched an exception: Fairplay error. KDCanProcessCKC status: -42786"),
            Event::SessionDead("[!] catched an exception: Fairplay error. KDCanProcessCKC status: -42786".into())
        );
        // Invalid CKC es de una pista, no de la sesión: solo se registra.
        assert_eq!(
            parse_line("[!] key request exception: Invalid CKC error."),
            Event::Log("[!] key request exception: Invalid CKC error.".into())
        );
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
