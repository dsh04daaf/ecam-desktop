// Lógica de la ventana. Todo lo que sea decidir algo de verdad vive en el core;
// aquí solo se enseñan pantallas y se recogen pulsaciones.
const $ = (id) => document.getElementById(id);
const screens = ['install', 'login', 'main', '2fa'];

function show(name) {
  screens.forEach((s) => $(`screen-${s}`)?.classList.toggle('hidden', s !== name));
}

let pending = { user: '', pass: '' };
let countdownTimer = null;

function startCountdown() {
  // El wrapper espera 60 s exactos y luego se cierra solo. Enseñarlo evita que
  // alguien teclee el código en el segundo 70 y no entienda por qué falla.
  let left = 60;
  clearInterval(countdownTimer);
  $('countdown').textContent = `Quedan ${left} s`;
  countdownTimer = setInterval(() => {
    left -= 1;
    $('countdown').textContent = left > 0 ? `Quedan ${left} s` : 'El código venció, pídelo otra vez';
    if (left <= 0) clearInterval(countdownTimer);
  }, 1000);
}

async function refresh() {
  // `?screen=login|2fa|install` fuerza una pantalla. Solo sirve para revisarlas
  // en la vista previa: con sesión activa no se pasa por ellas nunca.
  const forced = new URLSearchParams(location.search).get('screen');
  if (forced && screens.includes(forced)) {
    show(forced);
    if (forced === '2fa') startCountdown();
    return;
  }
  const state = await ecam.wrapperState();
  show(ecam.screenFor(state));
  if (state.account?.storefront_id) {
    $('account').textContent = `sesión activa · tienda ${state.account.storefront_id.split('-')[0]}`;
  }
  // Con sesión guardada se arranca sin volver a pedir nada.
  if (state.has_session && !state.listening) await ecam.startWrapper(null, null);
}

// ── eventos del wrapper ────────────────────────────────────────────────────
ecam.listen('wrapper', (ev) => {
  switch (ev.type) {
    case 'needs_two_factor':
      show('2fa');
      $('code').focus();
      startCountdown();
      break;
    case 'two_factor_accepted':
      clearInterval(countdownTimer);
      $('countdown').textContent = 'Código aceptado, entrando…';
      break;
    case 'two_factor_expired':
      $('tfa-error').textContent = 'El código venció. Vuelve a entrar para pedir otro.';
      show('login');
      break;
    case 'server_message':
      $('login-error').textContent = ev.value;
      break;
    case 'auth_error':
      $('login-error').textContent = ev.value.message;
      show('login');
      break;
    case 'login_failed':
      if (!$('login-error').textContent) $('login-error').textContent = 'No se pudo entrar.';
      show('login');
      break;
    case 'ready':
      $('login-error').textContent = '';
      show('main');
      refresh();
      break;
    case 'session_dead':
      addRow('La sesión del motor se cayó. Reiniciando…', false);
      ecam.startWrapper(null, null);
      break;
  }
});

// ── descargas ──────────────────────────────────────────────────────────────
const rows = new Map();

function addRow(text, ok = true, live = false) {
  const li = document.createElement('li');
  li.className = live ? 'live' : ok ? 'ok' : 'bad';
  const span = document.createElement('span');
  span.textContent = text;
  span.title = text;   // el texto completo, aunque la fila lo recorte
  li.appendChild(span);
  $('downloads').prepend(li);
  return li;
}

function setText(li, text) {
  if (li && li.firstChild) li.firstChild.textContent = text;
}

/// El botón se añade cuando ya se sabe el id del trabajo, no antes: cancelar
/// algo que todavía no ha arrancado no cancela nada.
function attachCancel(li, job) {
  const btn = document.createElement('button');
  btn.className = 'ghost tiny';
  btn.textContent = 'Cancelar';
  btn.addEventListener('click', () => {
    btn.disabled = true;
    setText(li, 'Cancelando…');
    ecam.cancel(job);
  });
  li.appendChild(btn);
}

/// Lanza una descarga y engancha su fila. Un solo camino para las dos formas de
/// pedirla (link pegado o tarjeta pulsada).
async function run(li, promise) {
  let job;
  try {
    job = await promise;
  } catch (e) {
    li.className = 'bad';
    setText(li, String(e));
    return;
  }
  rows.set(job, li);
  attachCancel(li, job);
}

const fmtMB = (n) => `${(n / 1048576).toFixed(1)} MB`;

ecam.listen('progress', (p) => {
  const li = rows.get(p.job);
  if (!li) return;
  if (p.stage === 'downloading') return setText(li, `Bajando… ${fmtMB(p.bytes)}`);
  if (p.stage === 'decrypting') {
    // Descifrar no baja bytes: sin este aviso la pantalla parecía colgada.
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    return setText(li, `Descifrando… ${pct}%`);
  }
  if (p.stage === 'tagging') return setText(li, 'Etiquetando…');
});

ecam.listen('track', (t) => {
  // Sesión muerta: reintentar el track no arregla nada, hay que relanzar el
  // motor. El core ya lo distingue; aquí se actúa en consecuencia.
  if (t.fatal) {
    addRow('La sesión del motor se cayó. Reiniciándolo…', false);
    ecam.startWrapper(null, null).catch(() => {});
  }
  addRow(t.ok ? `✓ ${t.name} — ${t.detail}` : `✗ ${t.detail}`, t.ok);
});

ecam.listen('finished', (f) => {
  const li = rows.get(f.job);
  if (li) li.remove();
  rows.delete(f.job);
  if (f.cancelled) return addRow(`Cancelado tras ${f.done || 0} pistas`, false);
  addRow(f.ok ? `Listo: ${f.done} pistas${f.failed ? `, ${f.failed} con problemas` : ''}` : `Error: ${f.error}`, f.ok);
});

async function startDownload(url) {
  const li = addRow('Bajando…', true, true);
  await run(li, ecam.download(url, $('quality').value));
}

// ── navegación ─────────────────────────────────────────────────────────────
const detail = $('detail');
let current = null;   // entidad abierta

function showGrid() {
  detail.classList.add('hidden');
  $('results').classList.remove('hidden');
  current = null;
}

async function openEntity(kind, id) {
  $('results').classList.add('hidden');
  detail.classList.remove('hidden');
  $('d-items').innerHTML = '<li class="hint">Cargando…</li>';
  try {
    current = await ecam.browse(kind, id);
  } catch (e) {
    $('d-items').innerHTML = `<li class="bad">${e}</li>`;
    return;
  }
  $('d-art').src = current.artwork || '';
  $('d-kind').textContent = current.kind;
  $('d-name').textContent = current.name;
  $('d-artist').textContent = current.artist || '';
  $('d-count').textContent = `${current.items.length} ${current.kind === 'artist' ? 'álbumes' : 'pistas'}`;
  $('d-items').innerHTML = '';

  current.items.forEach((it, i) => {
    const li = document.createElement('li');
    li.style.animationDelay = `${Math.min(i * 12, 300)}ms`;
    li.innerHTML = `<span class="n">${i + 1}</span>
      <span class="t"><strong>${it.name}</strong><em>${it.artist}</em></span>
      <span class="x">${it.extra || ''}</span>`;
    const btn = document.createElement('button');
    btn.className = 'ghost tiny';
    // Dentro de un artista cada fila es un álbum: se abre. Dentro de un álbum
    // o una playlist cada fila es una pista: se baja.
    btn.textContent = it.kind === 'album' ? 'Abrir' : 'Bajar';
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      if (it.kind === 'album') return openEntity('album', it.id);
      const row = addRow(`Bajando ${it.name}…`, true, true);
      await run(row, ecam.downloadItem('song', it.id, $('quality').value));
    });
    li.appendChild(btn);
    if (it.kind === 'album') li.addEventListener('click', () => openEntity('album', it.id));
    $('d-items').appendChild(li);
  });
}

$('back').addEventListener('click', showGrid);

$('d-all').addEventListener('click', async () => {
  if (!current) return;
  if (current.items.length > 30 &&
      !confirm(`Son ${current.items.length} elementos. ¿Bajar todo?`)) return;
  const li = addRow(`Bajando ${current.name}…`, true, true);
  await run(li, ecam.downloadItem(current.kind, current.id, $('quality').value));
});

// ── formularios ────────────────────────────────────────────────────────────
$('form-login').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('login-error').textContent = '';
  pending = { user: $('login-user').value.trim(), pass: $('login-pass').value };
  try {
    await ecam.startWrapper(pending.user, pending.pass);
  } catch (err) {
    $('login-error').textContent = String(err);
  }
});

$('form-2fa').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('tfa-error').textContent = '';
  try {
    await ecam.submitTwoFactor($('code').value);
  } catch (err) {
    $('tfa-error').textContent = String(err);
  }
});

$('q').addEventListener('keydown', async (e) => {
  if (e.key !== 'Enter') return;
  const text = $('q').value.trim();
  if (!text) return;
  if (ecam.isAppleUrl(text)) {
    $('q').value = '';
    return startDownload(text);
  }
  // Esqueletos mientras llega la respuesta: la pantalla no se queda muerta.
  showGrid();
  $('results').innerHTML = Array.from({ length: 12 }, () => '<div class="skeleton"></div>').join('');
  let hits;
  try {
    hits = await ecam.search(text);
  } catch (err) {
    $('results').innerHTML = `<p class="empty">${err}</p>`;
    return;
  }
  $('results').innerHTML = '';
  if (!hits.length) { $('results').innerHTML = '<p class="empty">Sin resultados</p>'; return; }
  hits.forEach((h, i) => {
    const card = document.createElement('article');
    card.className = 'card';
    card.style.animationDelay = `${Math.min(i * 22, 400)}ms`;
    card.innerHTML = `<div class="art"><img src="${h.artwork}" alt="" loading="lazy" /></div>
      <div class="meta"><strong>${h.name}</strong><span>${h.artist}</span><em>${h.kind}</em></div>`;
    card.addEventListener('click', async () => {
      // Álbum, artista y playlist se ABREN para poder ver qué traen y elegir.
      // Solo lo que ya es una pista suelta se baja de una pulsación.
      if (['album', 'artist', 'playlist'].includes(h.kind)) return openEntity(h.kind, h.id);
      const li = addRow(`Bajando ${h.name}…`, true, true);
      await run(li, ecam.downloadItem(h.kind, h.id, $('quality').value));
    });
    $('results').appendChild(card);
  });
});

// ── ajustes ────────────────────────────────────────────────────────────────
/// Etiquetas y grupos. Lo que no esté aquí se pinta igual con su nombre crudo:
/// así un ajuste nuevo del core nunca queda invisible en la ventana.
const CFG_LABELS = {
  'storefront': ['Cuenta', 'Tienda', 'auto = la de tu cuenta'],
  'language': ['Cuenta', 'Idioma de la metadata'],
  'media-user-token': ['Cuenta', 'Token de usuario', 'se toma solo del motor; solo tócalo si sabes lo que haces'],
  'decrypt-port': ['Cuenta', 'Puerto del motor'],
  'alac-max': ['Calidad', 'ALAC máximo (Hz)', '192000 · 96000 · 48000 · 44100'],
  'atmos-max': ['Calidad', 'Atmos máximo (kbps)', '2768 · 2448'],
  'aac-type': ['Calidad', 'Tipo de AAC', 'aac · aac-binaural · aac-downmix'],
  'mv-max': ['Calidad', 'Vídeo máximo (altura)', '2160 · 1080 · 720'],
  'mv-audio-type': ['Calidad', 'Audio del vídeo', 'atmos · ac3 · aac'],
  'output-dir': ['Carpetas', 'Carpeta de salida'],
  'album-folder-format': ['Carpetas', 'Carpeta de álbum', '{AlbumName} {ArtistName} {ReleaseYear} {UPC} {RecordLabel} {Quality} {Tag}'],
  'playlist-folder-format': ['Carpetas', 'Carpeta de playlist', '{PlaylistName} {PlaylistId}'],
  'artist-folder-format': ['Carpetas', 'Carpeta de artista', 'vacío = sin carpeta de artista'],
  'song-file-format': ['Carpetas', 'Nombre de archivo', '{SongNumer} {SongName} {DiscNumber} {TrackNumber} {Quality} {Tag}'],
  'explicit-choice': ['Carpetas', 'Etiqueta de explícito'],
  'clean-choice': ['Carpetas', 'Etiqueta de limpio'],
  'cover-size': ['Extras', 'Tamaño de carátula embebida'],
  'save-cover': ['Extras', 'Guardar carátula aparte'],
  'save-lrc': ['Extras', 'Guardar letras (.lrc)'],
  'embed-lrc': ['Extras', 'Letras dentro del archivo'],
  'save-animated-artwork': ['Extras', 'Artwork animado', 'usa el ffmpeg que trae la app'],
  'ffmpeg-path': ['Avanzado', 'Ruta de ffmpeg', 'vacío o "ffmpeg" = el que viene con la app'],
  'widevine-device-key': ['Avanzado', 'Llave de dispositivo (vídeos)', 'PEM; si está vacío se busca en la carpeta de config'],
  'widevine-client-id': ['Avanzado', 'ClientId (vídeos)'],
};

let cfgCache = null;

function renderSettings(cfg) {
  const box = $('cfg-form');
  box.innerHTML = '';
  const groups = {};
  for (const key of Object.keys(cfg)) {
    const [group, label, help] = CFG_LABELS[key] || ['Avanzado', key];
    (groups[group] = groups[group] || []).push({ key, label, help });
  }
  for (const group of ['Cuenta', 'Calidad', 'Carpetas', 'Extras', 'Avanzado']) {
    if (!groups[group]) continue;
    const h = document.createElement('h3');
    h.textContent = group;
    box.appendChild(h);
    for (const { key, label, help } of groups[group]) {
      const value = cfg[key];
      const lab = document.createElement('label');
      if (typeof value === 'boolean') {
        lab.className = 'check';
        lab.innerHTML = `<input type="checkbox" data-key="${key}" ${value ? 'checked' : ''} /> <span>${label}</span>`;
      } else {
        lab.innerHTML = `<span>${label}</span>
          <input data-key="${key}" value="${value ?? ''}" ${typeof value === 'number' ? 'inputmode="numeric"' : ''} />
          ${help ? `<em class="help">${help}</em>` : ''}`;
      }
      box.appendChild(lab);
    }
  }
}

$('btn-settings').addEventListener('click', async () => {
  try {
    cfgCache = await ecam.getConfig();
    renderSettings(cfgCache);
    $('settings').showModal();
  } catch (e) {
    addRow(String(e), false);
  }
});

$('cfg-save').addEventListener('click', async (e) => {
  e.preventDefault();
  const cfg = { ...cfgCache };
  for (const input of $('cfg-form').querySelectorAll('[data-key]')) {
    const key = input.dataset.key;
    const before = cfgCache[key];
    if (input.type === 'checkbox') cfg[key] = input.checked;
    // Un número tiene que volver como número: mandarlo como texto rompe el
    // config al leerlo.
    else if (typeof before === 'number') cfg[key] = Number(input.value) || 0;
    else cfg[key] = input.value;
  }
  try {
    await ecam.setConfig(cfg);
    $('settings').close();
  } catch (err) {
    addRow(String(err), false);
  }
});

$('cfg-signout').addEventListener('click', async (e) => {
  e.preventDefault();
  await ecam.signOut();
  $('settings').close();
  show('login');
});

$('btn-install').addEventListener('click', async () => {
  const file = await window.__TAURI__.dialog.open({
    filters: [{ name: 'Motor de ECAM', extensions: ['gz', 'tar.gz'] }],
  });
  if (!file) return;
  $('install-hint').textContent = 'Instalando… esto tarda un poco la primera vez.';
  try {
    await ecam.installDistro(file);
    await refresh();
  } catch (err) {
    $('install-hint').textContent = String(err);
  }
});

refresh().catch((e) => {
  document.body.innerHTML = `<p class="error center">${e}</p>`;
});
