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
      already: 'ya estaba', done: 'Listo', failed: 'Error', cancelled: 'Cancelado',
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
      connect_title: 'Conecta con el motor',
      connect_lead: 'En este sistema el motor de descifrado corre fuera de la app. Dime dónde está escuchando.',
      connect_go: 'Conectar',
      engine_remote: 'El motor corre fuera de esta app: se gestiona donde esté instalado.',
      connect_hint: 'Formato host:puerto, por ejemplo 127.0.0.1:10020. Del mismo host sale el puerto 30020. Ese puerto no va cifrado: sácalo por un túnel SSH o una red privada, no lo abras a internet.',
      connect_bad_addr: 'Escríbelo como host:puerto, por ejemplo 127.0.0.1:10020',
      connect_unreachable: 'Ahí no contesta nadie. Comprueba que el motor esté encendido y que el puerto se vea desde aquí.',
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
      already: 'already there', done: 'Done', failed: 'Error', cancelled: 'Cancelled',
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
      connect_title: 'Connect to the engine',
      connect_lead: 'On this system the decryption engine runs outside the app. Tell me where it is listening.',
      connect_go: 'Connect',
      engine_remote: 'The engine runs outside this app: manage it where it is installed.',
      connect_hint: 'Use host:port, for example 127.0.0.1:10020. Port 30020 comes from the same host. That port has no encryption: tunnel it over SSH or a private network, never expose it to the internet.',
      connect_bad_addr: 'Write it as host:port, for example 127.0.0.1:10020',
      connect_unreachable: 'Nothing answers there. Check that the engine is running and that the port is reachable from here.',
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
      already: 'уже есть', done: 'Готово', failed: 'Ошибка', cancelled: 'Отменено',
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
      connect_title: 'Подключение к движку',
      connect_lead: 'В этой системе движок расшифровки работает вне приложения. Укажите, где он слушает.',
      connect_go: 'Подключиться',
      engine_remote: 'Движок работает вне приложения: управляйте им там, где он установлен.',
      connect_hint: 'Формат host:порт, например 127.0.0.1:10020. Порт 30020 берётся с того же хоста. Он не шифруется: пробрасывайте его через SSH или частную сеть, не открывайте в интернет.',
      connect_bad_addr: 'Укажите в виде host:порт, например 127.0.0.1:10020',
      connect_unreachable: 'По этому адресу никто не отвечает. Проверьте, что движок запущен и порт доступен отсюда.',
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
