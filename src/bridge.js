// Puente con el core. Está aparte a propósito: así se puede probar con
// `node --test` sin navegador ni Tauri, que es lo que salvó a ECBP de publicar
// una versión que no hablaba con su propio motor.
(function (global) {
  function api() {
    // withGlobalTauri = true en tauri.conf.json. Si esto falta, la app se ve
    // pero no hace nada: es exactamente el fallo que costó la v0.1.0 de ECBP.
    const t = global.__TAURI__;
    if (!t || !t.core || typeof t.core.invoke !== 'function') {
      throw new Error('el puente con el core no está disponible (¿withGlobalTauri?)');
    }
    return t;
  }

  const bridge = {
    invoke: (cmd, args) => api().core.invoke(cmd, args || {}),
    listen: (event, handler) => api().event.listen(event, (e) => handler(e.payload)),

    wrapperState: () => bridge.invoke('wrapper_state'),
    installDistro: (tarball) => bridge.invoke('install_distro', { tarball }),
    startWrapper: (user, password) => bridge.invoke('start_wrapper', { user, password }),
    submitTwoFactor: (code) => bridge.invoke('submit_two_factor', { code }),
    signOut: () => bridge.invoke('sign_out'),
    getConfig: () => bridge.invoke('get_config'),
    setConfig: (cfg) => bridge.invoke('set_config', { cfg }),
    search: (term) => bridge.invoke('search', { term }),
    download: (url, quality) => bridge.invoke('download', { url, quality }),
    cancel: (job) => bridge.invoke('cancel', { job }),
  };

  /// Qué pantalla toca según lo que diga el core. Es una función pura para
  /// poder probarla sin abrir la app.
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
