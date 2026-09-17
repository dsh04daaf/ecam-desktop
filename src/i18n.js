// Idiomas de la ventana. Aparte de app.js para poder probar que los tres
// diccionarios tienen exactamente las mismas claves: se desincronizan solos y
// el resultado son textos en inglés apareciendo a mitad de una pantalla en ruso.
(function (global) {
  const DICTS = {
    es: {
      lang_name: 'Español',
      search_placeholder: 'Busca o pega un link de Apple Music',
      settings: 'Ajustes', save: 'Guardar', close: 'Cerrar', cancel: 'Cancelar',
      downloads: 'Descargas', history: 'Historial', engine: 'Motor',
      download: 'Bajar', open: 'Abrir', download_all: 'Descargar todo',
      back: '← Volver', tracks: 'pistas', albums: 'álbumes',
      no_results: 'Sin resultados', loading: 'Cargando…',
      downloading: 'Bajando…', decrypting: 'Descifrando…', tagging: 'Etiquetando…',
      already: 'ya estaba', done: 'Listo', failed: 'Error', cancelled: 'Cancelado', skipped: 'omitidas',
      empty_history: 'Todavía no has descargado nada',
      clear_history: 'Vaciar historial', open_folder: 'Abrir carpeta', remove: 'Quitar',
      engine_status: 'Estado del motor', relaunch: 'Relanzar motor',
      sign_out: 'Cerrar sesión', logs: 'Registro del motor',
      session_ok: 'Sesión activa', session_none: 'Sin sesión',
      listening: 'Escuchando', not_listening: 'No responde',
      import_widevine: 'Importar credenciales de vídeo',
      wv_ok: 'Vídeos: credenciales cargadas', wv_missing: 'Vídeos: faltan credenciales',
      availability: 'Disponibilidad', available: 'Disponible',
      partial: 'Parcial', unavailable: 'No disponible',
      qualities_here: 'Calidades disponibles', hq_artwork: 'Carátula en máxima calidad',
      other_versions: 'Otras versiones que sí están',
      login_title: 'Entra con tu Apple ID',
      login_lead: 'Se usa tu suscripción de Apple Music. La sesión se queda guardada.',
      login_user: 'correo', login_pass: 'contraseña', login_go: 'Entrar',
      tfa_title: 'Código de verificación',
      tfa_lead: 'Apple acaba de mandarlo a tus dispositivos.',
      tfa_go: 'Confirmar', tfa_left: 'Quedan {n} s',
      tfa_expired: 'El código venció, pídelo otra vez',
      install_title: 'Primer arranque',
      install_lead: 'ECAM necesita instalar su motor de descifrado (~50 MB). No toca ninguna otra distro de WSL que tengas.',
      install_go: 'Instalar el motor',
      exit_sin_privilegios: 'El contenedor arrancó sin privilegios. El motor necesita --privileged, porque hace chroot y monta /proc. Si lo lanzaste a mano desde Docker Desktop, bórralo y deja que lo arranque ECAM.',
      exit_sin_imagen: 'Docker no encuentra la imagen del motor. Vuelve a cargarla desde el archivo de ECAM.',
      exit_puerto_ocupado: 'Uno de los puertos del motor ya está ocupado, normalmente por otro ECAM o un contenedor viejo encendido.',
      exit_sin_docker: 'Docker no está arrancado. Ábrelo y espera a que diga que está corriendo.',
      exit_socket_docker: 'Docker no deja que ECAM le hable: es un problema de permisos de su socket.',
      exit_sin_espacio: 'No queda espacio en el disco para el motor.',
      exit_arquitectura: 'La imagen del motor no es de esta arquitectura.',
      exit_generico: 'El motor se cerró al arrancar sin llegar a escuchar.',
      bulk_warn: '«{name}» puede ser una descarga muy larga. ¿Seguir?',
    },
    en: {
      lang_name: 'English',
      search_placeholder: 'Search or paste an Apple Music link',
      settings: 'Settings', save: 'Save', close: 'Close', cancel: 'Cancel',
      downloads: 'Downloads', history: 'History', engine: 'Engine',
      download: 'Download', open: 'Open', download_all: 'Download all',
      back: '← Back', tracks: 'tracks', albums: 'albums',
      no_results: 'No results', loading: 'Loading…',
      downloading: 'Downloading…', decrypting: 'Decrypting…', tagging: 'Tagging…',
      already: 'already there', done: 'Done', failed: 'Error', cancelled: 'Cancelled', skipped: 'skipped',
      empty_history: 'Nothing downloaded yet',
      clear_history: 'Clear history', open_folder: 'Open folder', remove: 'Remove',
      engine_status: 'Engine status', relaunch: 'Restart engine',
      sign_out: 'Sign out', logs: 'Engine log',
      session_ok: 'Session active', session_none: 'No session',
      listening: 'Listening', not_listening: 'Not responding',
      import_widevine: 'Import video credentials',
      wv_ok: 'Videos: credentials loaded', wv_missing: 'Videos: credentials missing',
      availability: 'Availability', available: 'Available',
      partial: 'Partial', unavailable: 'Unavailable',
      qualities_here: 'Qualities available', hq_artwork: 'Full-resolution artwork',
      other_versions: 'Other versions that are available',
      login_title: 'Sign in with your Apple ID',
      login_lead: 'Uses your Apple Music subscription. The session is kept.',
      login_user: 'email', login_pass: 'password', login_go: 'Sign in',
      tfa_title: 'Verification code',
      tfa_lead: 'Apple just sent it to your devices.',
      tfa_go: 'Confirm', tfa_left: '{n} s left',
      tfa_expired: 'The code expired, request a new one',
      install_title: 'First run',
      install_lead: 'ECAM needs to install its decryption engine (~50 MB). It will not touch any other WSL distro you have.',
      install_go: 'Install the engine',
      exit_sin_privilegios: 'The container started without privileges. The engine needs --privileged, because it chroots and mounts /proc. If you started it by hand from Docker Desktop, delete it and let ECAM start it.',
      exit_sin_imagen: 'Docker cannot find the engine image. Load it again from the ECAM file.',
      exit_puerto_ocupado: 'One of the engine ports is already taken, usually by another ECAM or an old container still running.',
      exit_sin_docker: 'Docker is not running. Open it and wait until it reports it is running.',
      exit_socket_docker: 'Docker will not let ECAM talk to it: its socket permissions are the problem.',
      exit_sin_espacio: 'There is no disk space left for the engine.',
      exit_arquitectura: 'The engine image is not for this architecture.',
      exit_generico: 'The engine closed on startup without ever listening.',
      bulk_warn: '“{name}” may be a very long download. Continue?',
    },
    ru: {
      lang_name: 'Русский',
      search_placeholder: 'Поиск или ссылка Apple Music',
      settings: 'Настройки', save: 'Сохранить', close: 'Закрыть', cancel: 'Отмена',
      downloads: 'Загрузки', history: 'История', engine: 'Движок',
      download: 'Скачать', open: 'Открыть', download_all: 'Скачать всё',
      back: '← Назад', tracks: 'треков', albums: 'альбомов',
      no_results: 'Ничего не найдено', loading: 'Загрузка…',
      downloading: 'Загрузка…', decrypting: 'Расшифровка…', tagging: 'Теги…',
      already: 'уже есть', done: 'Готово', failed: 'Ошибка', cancelled: 'Отменено', skipped: 'пропущено',
      empty_history: 'Пока ничего не скачано',
      clear_history: 'Очистить историю', open_folder: 'Открыть папку', remove: 'Удалить',
      engine_status: 'Состояние движка', relaunch: 'Перезапустить движок',
      sign_out: 'Выйти', logs: 'Журнал движка',
      session_ok: 'Сессия активна', session_none: 'Нет сессии',
      listening: 'Слушает', not_listening: 'Не отвечает',
      import_widevine: 'Импорт учётных данных для видео',
      wv_ok: 'Видео: учётные данные загружены', wv_missing: 'Видео: нет учётных данных',
      availability: 'Доступность', available: 'Доступно',
      partial: 'Частично', unavailable: 'Недоступно',
      qualities_here: 'Доступные качества', hq_artwork: 'Обложка в максимальном качестве',
      other_versions: 'Другие доступные издания',
      login_title: 'Войдите с Apple ID',
      login_lead: 'Используется ваша подписка Apple Music. Сессия сохраняется.',
      login_user: 'почта', login_pass: 'пароль', login_go: 'Войти',
      tfa_title: 'Код подтверждения',
      tfa_lead: 'Apple отправил его на ваши устройства.',
      tfa_go: 'Подтвердить', tfa_left: 'Осталось {n} с',
      tfa_expired: 'Код истёк, запросите новый',
      install_title: 'Первый запуск',
      install_lead: 'ECAM установит свой движок расшифровки (~50 МБ). Другие дистрибутивы WSL не затрагиваются.',
      install_go: 'Установить движок',
      exit_sin_privilegios: 'Контейнер запущен без привилегий. Движку нужен --privileged: он делает chroot и монтирует /proc. Если вы запустили его вручную из Docker Desktop, удалите контейнер и дайте ECAM запустить его сам.',
      exit_sin_imagen: 'Docker не находит образ движка. Загрузите его заново из файла ECAM.',
      exit_puerto_ocupado: 'Один из портов движка уже занят — обычно другим ECAM или старым работающим контейнером.',
      exit_sin_docker: 'Docker не запущен. Откройте его и дождитесь, пока он сообщит, что работает.',
      exit_socket_docker: 'Docker не даёт ECAM обращаться к нему: проблема в правах на его сокет.',
      exit_sin_espacio: 'На диске не осталось места для движка.',
      exit_arquitectura: 'Образ движка не для этой архитектуры.',
      exit_generico: 'Движок закрылся при запуске, так и не начав слушать порты.',
      bulk_warn: '«{name}» может качаться очень долго. Продолжить?',
    },
  };

  let current = 'es';

  const i18n = {
    dicts: DICTS,
    languages: () => Object.keys(DICTS).map((code) => ({ code, name: DICTS[code].lang_name })),
    use(code) {
      if (DICTS[code]) current = code;
      return current;
    },
    current: () => current,
    /// Traduce. Si falta una clave devuelve la del español antes que un hueco.
    t(key, vars) {
      let s = (DICTS[current] && DICTS[current][key]) ?? DICTS.es[key] ?? key;
      if (vars) for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, v);
      return s;
    },
    /// Elige idioma a partir del que tenga puesto el config o el del sistema.
    detect(configLanguage) {
      const tag = String(configLanguage || navigator.language || 'es').toLowerCase();
      if (tag.startsWith('ru')) return 'ru';
      if (tag.startsWith('es')) return 'es';
      return 'en';
    },
  };

  if (typeof module !== 'undefined' && module.exports) module.exports = i18n;
  global.i18n = i18n;
})(typeof window !== 'undefined' ? window : globalThis);
