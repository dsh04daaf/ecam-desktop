/* Puente entre la UI y el core.
 *
 * Vive aparte de app.js y sin tocar el DOM para poder probarlo en Node
 * simulando cada entorno (ver `tests/bridge.test.mjs`).
 *
 * El diseño viene de una cicatriz de ECBP Desktop: su v0.1.0 salió rota porque
 * el puente, al no encontrar `window.__TAURI__`, caía a HTTP; el protocolo de
 * assets de Tauri respondía el index.html con 200 y el código tomaba ESO por
 * datos. Cero peticiones llegaron al core y varias pantallas fallaron en
 * silencio. De ahí las tres defensas de aquí abajo:
 *   1. buscar también `__TAURI_INTERNALS__`, que Tauri v2 inyecta siempre;
 *   2. saber si estamos DENTRO de la app aunque el puente esté roto, para
 *      gritar en vez de fingir que somos un navegador;
 *   3. exigir content-type JSON: un servidor de assets contesta HTML a lo que
 *      no conoce, y eso es un error, no un dato.
 */
(function (global) {
  function findInvoke(win) {
    if (!win) return null;
    const g = win.__TAURI__ && win.__TAURI__.core && win.__TAURI__.core.invoke;
    if (typeof g === 'function') return g.bind(win.__TAURI__.core);
    const i = win.__TAURI_INTERNALS__ && win.__TAURI_INTERNALS__.invoke;
    if (typeof i === 'function') return (cmd, args) => i(cmd, args);
    return null;
  }

  function inApp(win) {
    if (!win) return false;
    if (win.__TAURI__ || win.__TAURI_INTERNALS__) return true;
    const origin = (win.location && win.location.origin) || '';
    return /tauri\.localhost/.test(origin);
  }

  function makeCall({ invoke, isApp, fetchImpl }) {
    return async function call(cmd, args = {}) {
      if (invoke) return invoke(cmd, args);
      if (isApp) {
        throw new Error(`El puente con el core no está disponible (comando "${cmd}"). Es un fallo de build.`);
      }
      const r = await fetchImpl('invoke/' + cmd, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(args),
      });
      const ct = (r.headers && r.headers.get('content-type')) || '';
      if (!ct.includes('application/json')) {
        throw new Error(`"${cmd}" no devolvió JSON (HTTP ${r.status}) — el core no está disponible`);
      }
      const d = await r.json();
      if (!r.ok) throw new Error(d && d.error ? d.error : 'HTTP ' + r.status);
      return d;
    };
  }

  /// En la app los sucesos son eventos de Tauri; en la vista previa se sondean.
  function makeListen({ win, isApp, fetchImpl }) {
    const handlers = {};
    if (isApp && win.__TAURI__ && win.__TAURI__.event) {
      return (name, fn) => win.__TAURI__.event.listen(name, (e) => fn(e.payload));
    }
    let polling = false;
    return (name, fn) => {
      (handlers[name] = handlers[name] || []).push(fn);
      if (polling) return;
      polling = true;
      setInterval(async () => {
        try {
          const r = await fetchImpl('events');
          if (!(r.headers.get('content-type') || '').includes('application/json')) return;
          for (const ev of await r.json()) {
            // El rpc marca el tipo en `event`; en la app son canales distintos.
            const channel = ev.event === 'track' ? 'track'
                          : ev.event === 'finished' ? 'finished' : null;
            (handlers[channel] || []).forEach((h) => h(ev));
          }
        } catch { /* la vista previa puede estar apagada: no es fatal */ }
      }, 700);
    };
  }

  const win = global.window || global;
  const isApp = inApp(win);
  const fetchImpl = (...a) => global.fetch(...a);
  const call = makeCall({ invoke: findInvoke(win), isApp, fetchImpl });

  /// Tabla de comandos. Se construye a partir de `call` para que las pruebas
  /// puedan verificar el NOMBRE y los ARGUMENTOS de cada uno: antes quedaban
  /// fijados al cargar el módulo y no había forma de comprobarlos.
  function makeCommands(call) {
    return {
      wrapperState: () => call('wrapper_state'),
      installDistro: (tarball) => call('install_distro', { tarball }),
      startWrapper: (user, password) => call('start_wrapper', { user, password }),
      submitTwoFactor: (code) => call('submit_two_factor', { code }),
      signOut: () => call('sign_out'),
      getConfig: () => call('get_config'),
      setConfig: (cfg) => call('set_config', { cfg }),
      search: (term) => call('search', { term }),
      browse: (kind, id) => call('browse', { kind, id }),
      download: (url, quality) => call('download', { url, quality }),
      // Por tipo e id: la URL la arma el core con la tienda de la cuenta.
      downloadItem: (kind, id, quality) => call('download_item', { kind, id, quality }),
      cancel: (job) => call('cancel', { job }),
      preview: (url) => call('preview', { url }),
      historyList: () => call('history_list'),
      historyClear: () => call('history_clear'),
      historyRemove: (id) => call('history_remove', { id }),
      openFolder: (path) => call('open_folder', { path }),
      restartWrapper: () => call('restart_wrapper'),
      wrapperLogs: () => call('wrapper_logs'),
      importWidevine: (paths) => call('import_widevine', { paths }),
      widevineReady: () => call('widevine_ready'),
    };
  }

  const bridge = Object.assign({
    findInvoke, inApp, makeCall, makeListen, makeCommands,
    isApp,
    invoke: call,
    listen: makeListen({ win, isApp, fetchImpl }),
  }, makeCommands(call));

  /// Qué pantalla toca. Función pura para poder probarla sin abrir la app.
  bridge.screenFor = function (state) {
    if (!state.distro_installed) return 'install';
    if (!state.has_session) return 'login';
    return 'main';
  };

  /// Un link de Apple Music pegado en el buscador se abre directo.
  bridge.isAppleUrl = function (text) {
    return /music\.apple\.com\/[a-z]{2}\/(album|song|playlist|artist|room|music-video)\//i.test(text || '');
  };

  if (typeof module !== 'undefined' && module.exports) module.exports = bridge;
  global.ecam = bridge;
})(typeof window !== 'undefined' ? window : globalThis);
