// Lógica de la ventana. Todo lo que sea decidir algo de verdad vive en el core;
// aquí se enseñan pantallas, se recogen pulsaciones y se traduce.
const $ = (id) => document.getElementById(id);
const screens = ['install', 'connect', 'login', '2fa', 'main'];
const t = (k, v) => i18n.t(k, v);

// ── idioma ─────────────────────────────────────────────────────────────────
function applyLanguage() {
  document.documentElement.lang = i18n.current();
  document.querySelectorAll('[data-t]').forEach((el) => { el.textContent = t(el.dataset.t); });
  document.querySelectorAll('[data-t-ph]').forEach((el) => { el.placeholder = t(el.dataset.tPh); });
}

// ── pantallas ──────────────────────────────────────────────────────────────
function show(name) {
  screens.forEach((s) => $(`screen-${s}`)?.classList.toggle('hidden', s !== name));
}

let pending = { user: '', pass: '' };
let countdownTimer = null;

function startCountdown() {
  // La ventana del wrapper es de 60 s exactos y luego se cierra solo: sin verlo,
  // se teclea el código en el segundo 70 y no se entiende por qué falla.
  let left = 60;
  clearInterval(countdownTimer);
  $('countdown').textContent = t('tfa_left', { n: left });
  countdownTimer = setInterval(() => {
    left -= 1;
    $('countdown').textContent = left > 0 ? t('tfa_left', { n: left }) : t('tfa_expired');
    if (left <= 0) clearInterval(countdownTimer);
  }, 1000);
}

// ── panel: pestañas ────────────────────────────────────────────────────────
document.querySelectorAll('.tab').forEach((btn) => {
  btn.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach((b) => b.classList.toggle('on', b === btn));
    for (const name of ['downloads', 'history', 'engine']) {
      $(`tab-${name}`).classList.toggle('hidden', name !== btn.dataset.tab);
    }
    if (btn.dataset.tab === 'history') renderHistory();
    if (btn.dataset.tab === 'engine') renderEngine();
  });
});

// ── descargas en curso ─────────────────────────────────────────────────────
const rows = new Map();

function addRow(text, ok = true, live = false) {
  const li = document.createElement('li');
  li.className = live ? 'live' : ok ? 'ok' : 'bad';
  const span = document.createElement('span');
  span.textContent = text;
  span.title = text;         // el texto completo, aunque la fila lo recorte
  li.appendChild(span);
  $('downloads').prepend(li);
  return li;
}

const setText = (li, text) => {
  if (li && li.firstChild) {
    li.firstChild.textContent = text;
    li.firstChild.title = text;
  }
};

/// El botón de cancelar se añade cuando ya se sabe el id del trabajo: cancelar
/// algo que todavía no ha arrancado no cancela nada.
function attachCancel(li, job) {
  const btn = document.createElement('button');
  btn.className = 'ghost tiny';
  btn.textContent = t('cancel');
  btn.addEventListener('click', () => {
    btn.disabled = true;
    setText(li, t('cancelled') + '…');
    ecam.cancel(job);
  });
  li.appendChild(btn);
}

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
  if (p.stage === 'downloading') return setText(li, `${t('downloading')} ${fmtMB(p.bytes)}`);
  if (p.stage === 'decrypting') {
    // Descifrar no baja bytes: sin este aviso la pantalla parece colgada.
    const pct = p.total ? Math.round((p.done / p.total) * 100) : 0;
    return setText(li, `${t('decrypting')} ${pct}%`);
  }
  if (p.stage === 'tagging') return setText(li, t('tagging'));
});

ecam.listen('track', (tr) => {
  // Sesión muerta: el core ya la relanza y reintenta solo; aquí solo se informa.
  if (tr.fatal) addRow('⟳ ' + tr.detail, false);
  addRow(tr.ok ? `✓ ${tr.name} — ${tr.detail}` : `✗ ${tr.detail}`, tr.ok);
});

ecam.listen('finished', (f) => {
  const li = rows.get(f.job);
  if (li) li.remove();
  rows.delete(f.job);
  if (f.cancelled) addRow(`${t('cancelled')} (${f.done || 0})`, false);
  else if (f.ok) addRow(`${t('done')}: ${f.done}${f.failed ? ` · ${f.failed} ✗` : ''}`);
  else addRow(`${t('failed')}: ${f.error}`, false);
  renderHistory();
});

// ── historial ──────────────────────────────────────────────────────────────
async function renderHistory() {
  let list = [];
  try {
    list = await ecam.historyList();
  } catch { return; }

  const ul = $('history');
  ul.innerHTML = '';
  if (!list.length) {
    ul.innerHTML = `<li class="hint">${t('empty_history')}</li>`;
    return;
  }
  for (const e of list) {
    const li = document.createElement('li');
    const when = new Date(e.at * 1000).toLocaleString();
    const resumen = e.cancelled
      ? t('cancelled')
      : `${e.ok} ${t('tracks')}${e.failed.length ? ` · ${e.failed.length} ✗` : ''}`;
    li.innerHTML = `<span><strong>${e.name}</strong><em>${when} · ${resumen} · ${e.quality || e.kind}</em></span>`;
    li.className = e.failed.length && !e.ok ? 'bad' : 'ok';
    // El motivo de cada fallo, al alcance: es para lo que existe el historial.
    if (e.failed.length) li.title = e.failed.map((f) => `${f.name}: ${f.reason}`).join('\n');

    if (e.folder) {
      const open = document.createElement('button');
      open.className = 'ghost tiny';
      open.textContent = t('open_folder');
      open.addEventListener('click', () => ecam.openFolder(e.folder).catch((err) => addRow(String(err), false)));
      li.appendChild(open);
    }
    const del = document.createElement('button');
    del.className = 'ghost tiny';
    del.textContent = '✕';
    del.title = t('remove');
    del.addEventListener('click', async () => { await ecam.historyRemove(e.id); renderHistory(); });
    li.appendChild(del);
    ul.appendChild(li);
  }
}

$('hist-clear').addEventListener('click', async () => {
  await ecam.historyClear();
  renderHistory();
});

// ── motor ──────────────────────────────────────────────────────────────────
async function renderEngine() {
  const ul = $('engine-state');
  ul.innerHTML = `<li class="hint">${t('loading')}</li>`;
  try {
    const s = await ecam.wrapperState();
    const store = s.account?.storefront_id ? s.account.storefront_id.split('-')[0] : '—';
    // La dirección de verdad, no un 10020 escrito a mano: si el motor está en
    // otra máquina, ver el puerto equivocado aquí manda a buscar donde no es.
    let addr = '';
    try { addr = (await ecam.getConfig())['decrypt-port'] || ''; } catch { /* sin core */ }
    const remoto = s.backend === 'external';
    ul.innerHTML = `
      <li>${s.has_session ? '✓' : '✕'} ${s.has_session ? t('session_ok') : t('session_none')}</li>
      <li>${s.listening ? '✓' : '✕'} ${s.listening ? t('listening') : t('not_listening')}${addr ? ` · ${addr}` : ''}</li>
      <li>${t('availability')}: ${store}</li>
      ${remoto ? `<li class="hint">${t('engine_remote')}</li>` : ''}`;
    // Con el motor fuera, relanzar y cerrar sesión desde aquí NO harían nada
    // (`launch_command` es un `true` y `sign_out` no tiene dónde borrar).
    // Un botón que miente es peor que no tenerlo.
    $('eng-restart').classList.toggle('hidden', remoto);
    $('eng-signout').classList.toggle('hidden', remoto);
  } catch (e) {
    ul.innerHTML = `<li class="bad">${e}</li>`;
  }
  try {
    $('engine-logs').textContent = (await ecam.wrapperLogs()).slice(-60).join('\n');
  } catch { /* la vista previa no tiene logs */ }
}

$('eng-restart').addEventListener('click', async (e) => {
  e.target.disabled = true;
  addRow(t('relaunch') + '…');
  try {
    const ok = await ecam.restartWrapper();
    addRow(ok ? `${t('relaunch')} ✓` : `${t('relaunch')} ✗`, ok);
  } catch (err) {
    addRow(String(err), false);
  }
  e.target.disabled = false;
  renderEngine();
});

$('eng-signout').addEventListener('click', async () => {
  await ecam.signOut().catch((e) => addRow(String(e), false));
  show('login');
});

// ── eventos del wrapper ────────────────────────────────────────────────────
ecam.listen('wrapper', (ev) => {
  switch (ev.type) {
    case 'needs_two_factor': show('2fa'); $('code').focus(); startCountdown(); break;
    case 'two_factor_accepted': clearInterval(countdownTimer); $('countdown').textContent = '✓'; break;
    case 'two_factor_expired': $('tfa-error').textContent = t('tfa_expired'); show('login'); break;
    case 'server_message': $('login-error').textContent = ev.value; break;
    case 'auth_error': $('login-error').textContent = ev.value.message; show('login'); break;
    case 'login_failed':
      if (!$('login-error').textContent) $('login-error').textContent = t('failed');
      show('login');
      break;
    case 'ready': $('login-error').textContent = ''; show('main'); refresh(); break;
    case 'session_dead': addRow('⟳ ' + ev.value, false); break;
  }
});

// ── navegación ─────────────────────────────────────────────────────────────
let current = null;

function showGrid() {
  $('detail').classList.add('hidden');
  $('card').classList.add('hidden');
  $('results').classList.remove('hidden');
  current = null;
}

/// Las calidades que ofrece una pista, tal cual las publica Apple.
function qualityOptions(q) {
  const opts = [];
  if (q?.alac) opts.push({ value: 'alac', label: q.alac });
  if (q?.atmos) opts.push({ value: 'atmos', label: 'Atmos' });
  if (q?.aac) opts.push({ value: 'aac', label: q.aac });
  if (q?.binaural) opts.push({ value: 'binaural', label: 'Binaural' });
  return opts.length ? opts : [
    { value: 'alac', label: 'ALAC' }, { value: 'atmos', label: 'Atmos' },
    { value: 'aac', label: 'AAC' }, { value: 'binaural', label: 'Binaural' },
  ];
}

function qualitySelect(q, cls = '') {
  const sel = document.createElement('select');
  sel.className = `qsel ${cls}`;
  for (const o of qualityOptions(q)) {
    const opt = document.createElement('option');
    opt.value = o.value;
    opt.textContent = o.label;
    sel.appendChild(opt);
  }
  // Se respeta lo que haya elegido arriba, si esa calidad existe aquí.
  const global = $('quality').value;
  if ([...sel.options].some((o) => o.value === global)) sel.value = global;
  return sel;
}

async function openEntity(kind, id) {
  $('results').classList.add('hidden');
  $('card').classList.add('hidden');
  $('detail').classList.remove('hidden');
  $('d-items').innerHTML = `<li class="hint">${t('loading')}</li>`;
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
  $('d-count').textContent = `${current.items.length} ${current.kind === 'artist' ? t('albums') : t('tracks')}`;

  // Selector de calidad para "descargar todo", con las calidades reales.
  const head = $('d-quality');
  head.innerHTML = '';
  for (const o of qualityOptions(current.items.find((i) => i.quality)?.quality)) {
    const opt = document.createElement('option');
    opt.value = o.value;
    opt.textContent = o.label;
    head.appendChild(opt);
  }
  head.classList.toggle('hidden', current.kind === 'artist');

  $('d-items').innerHTML = '';
  current.items.forEach((it, i) => {
    const li = document.createElement('li');
    li.style.animationDelay = `${Math.min(i * 12, 300)}ms`;
    const badges = it.quality
      ? [it.quality.alac, it.quality.atmos ? 'Atmos' : null, it.quality.aac]
          .filter(Boolean).map((b) => `<b>${b}</b>`).join('')
      : (it.traits || []).map((x) => `<b>${x}</b>`).join('');
    li.innerHTML = `<span class="n">${i + 1}</span>
      <span class="tt"><strong>${it.name}</strong><em>${it.artist}</em></span>
      <span class="badges">${badges}</span>
      <span class="x">${it.extra || ''}</span>`;

    if (it.kind === 'album') {
      const open = document.createElement('button');
      open.className = 'ghost tiny';
      open.textContent = t('open');
      open.addEventListener('click', (e) => { e.stopPropagation(); openEntity('album', it.id); });
      li.appendChild(open);
      li.addEventListener('click', () => openEntity('album', it.id));
    } else if (it.playable === false) {
      li.classList.add('bad');
      li.querySelector('.x').textContent = t('unavailable');
    } else {
      // Calidad POR PISTA: no hay que tocar el selector de arriba.
      const sel = qualitySelect(it.quality);
      li.appendChild(sel);
      const btn = document.createElement('button');
      btn.className = 'ghost tiny';
      btn.textContent = t('download');
      btn.addEventListener('click', async (e) => {
        e.stopPropagation();
        const row = addRow(`${t('downloading')} ${it.name}`, true, true);
        await run(row, ecam.downloadItem('song', it.id, sel.value));
      });
      li.appendChild(btn);
    }
    $('d-items').appendChild(li);
  });
}

$('back').addEventListener('click', showGrid);

$('d-all').addEventListener('click', async () => {
  if (!current) return;
  if (current.items.length > 30 && !confirm(t('bulk_warn', { name: current.name }))) return;
  const li = addRow(`${t('downloading')} ${current.name}`, true, true);
  const quality = $('d-quality').classList.contains('hidden') ? $('quality').value : $('d-quality').value;
  await run(li, ecam.downloadItem(current.kind, current.id, quality));
});

// ── card de un link pegado ─────────────────────────────────────────────────
function renderCard(p) {
  $('results').classList.add('hidden');
  $('detail').classList.add('hidden');
  const box = $('card');
  box.classList.remove('hidden');

  const estado = { available: t('available'), partial: t('partial'), unavailable: t('unavailable') }[p.availability];
  const q = p.real_quality;
  const calidades = [q?.alac, q?.atmos ? 'Atmos' : null, q?.aac]
    .filter(Boolean).map((x) => `<li>${x}</li>`).join('');

  box.innerHTML = `
    <div class="chead">
      <img src="${p.artwork}" alt="" />
      <div>
        <em>${p.kind}</em>
        <h2>${p.title}</h2>
        <span>${p.artist}</span>
        <p class="hint">${p.track_count} ${t('tracks')} · <b class="av ${p.availability}">${estado}</b>
          ${p.reason ? ` — ${p.reason}` : ''}</p>
      </div>
    </div>
    ${calidades ? `<h3>${t('qualities_here')}</h3><ul class="quals">${calidades}</ul>` : ''}
    ${p.warnings.length ? `<ul class="warns">${p.warnings.map((w) => `<li>${w.detail}</li>`).join('')}</ul>` : ''}
    ${p.artwork_hq ? `<p><a href="#" id="hq-link">${t('hq_artwork')}</a></p>` : ''}
    ${p.alternatives.length ? `<h3>${t('other_versions')}</h3><ul class="alts">${
      p.alternatives.map((a) => `<li data-id="${a.id}"><img src="${a.artwork}" alt=""/><span>${a.name}<em>${a.year}</em></span></li>`).join('')
    }</ul>` : ''}
    <div class="crow"></div>`;

  // Un <a target="_blank"> no abre nada dentro de la app: el navegador lo abre
  // el sistema, y eso hay que pedirlo explícitamente.
  const hq = box.querySelector('#hq-link');
  if (hq) {
    hq.addEventListener('click', (e) => {
      e.preventDefault();
      ecam.openFolder(p.artwork_hq).catch((err) => addRow(String(err), false));
    });
  }

  const row = box.querySelector('.crow');
  if (p.availability !== 'unavailable') {
    const sel = qualitySelect(q, 'big');
    row.appendChild(sel);
    const btn = document.createElement('button');
    btn.className = 'primary';
    btn.textContent = t('download');
    btn.addEventListener('click', async () => {
      const li = addRow(`${t('downloading')} ${p.title}`, true, true);
      await run(li, ecam.downloadItem(p.kind, p.id, sel.value));
    });
    row.appendChild(btn);
  }
  if (p.artwork_hq) {
    const art = document.createElement('button');
    art.className = 'ghost';
    art.textContent = t('get_artwork');
    art.addEventListener('click', async () => {
      const li = addRow(`${t('downloading')} ${t('get_artwork')}`, true, true);
      try {
        const n = await ecam.downloadArtwork(p.kind, p.id, false);
        li.className = 'ok';
        setText(li, `✓ ${t('get_artwork')} (${n})`);
      } catch (err) {
        li.className = 'bad';
        setText(li, String(err));
      }
    });
    row.appendChild(art);
  }
  if (p.has_animated_artwork) {
    const anim = document.createElement('button');
    anim.className = 'ghost';
    anim.textContent = t('get_animated');
    anim.addEventListener('click', async () => {
      const li = addRow(`${t('downloading')} ${t('get_animated')}`, true, true);
      try {
        const n = await ecam.downloadArtwork(p.kind, p.id, true);
        li.className = 'ok';
        setText(li, `✓ ${t('get_animated')} (${n})`);
      } catch (err) {
        li.className = 'bad';
        setText(li, String(err));
      }
    });
    row.appendChild(anim);
  }
  if (['album', 'playlist', 'artist'].includes(p.kind)) {
    const open = document.createElement('button');
    open.className = 'ghost';
    open.textContent = t('open');
    open.addEventListener('click', () => openEntity(p.kind, p.id));
    row.appendChild(open);
  }
  box.querySelectorAll('.alts li').forEach((li) => {
    li.addEventListener('click', () => openEntity('album', li.dataset.id));
  });
}

// ── buscador ───────────────────────────────────────────────────────────────
$('q').addEventListener('keydown', async (e) => {
  if (e.key !== 'Enter') return;
  const text = $('q').value.trim();
  if (!text) return;

  if (ecam.isAppleUrl(text)) {
    // Un link no se baja a ciegas: primero la card con lo que hay y qué avisos.
    $('card').classList.remove('hidden');
    $('card').innerHTML = `<p class="hint">${t('loading')}</p>`;
    try {
      renderCard(await ecam.preview(text));
    } catch (err) {
      $('card').innerHTML = `<p class="bad">${err}</p>`;
    }
    return;
  }

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
  if (!hits.length) {
    $('results').innerHTML = `<p class="empty">${t('no_results')}</p>`;
    return;
  }
  hits.forEach((h, i) => {
    const card = document.createElement('article');
    card.className = 'card';
    card.style.animationDelay = `${Math.min(i * 22, 400)}ms`;
    card.innerHTML = `<div class="art"><img src="${h.artwork}" alt="" loading="lazy" /></div>
      <div class="meta"><strong>${h.name}</strong><span>${h.artist}</span><em>${h.kind}</em></div>`;
    card.addEventListener('click', async () => {
      // Lo navegable se abre; una pista suelta se baja.
      if (['album', 'artist', 'playlist'].includes(h.kind)) return openEntity(h.kind, h.id);
      const li = addRow(`${t('downloading')} ${h.name}`, true, true);
      await run(li, ecam.downloadItem(h.kind, h.id, $('quality').value));
    });
    $('results').appendChild(card);
  });
});

// ── ajustes ────────────────────────────────────────────────────────────────
/// Etiquetas, ayuda y opciones fijas. Lo que no esté aquí se pinta igual con su
/// nombre crudo: un ajuste nuevo del core nunca queda invisible.
const CFG = {
  'storefront':            ['Cuenta', 'Tienda', 'auto = la de tu cuenta. Un código de dos letras la fuerza (nz, us, jp…)'],
  'language':              ['Cuenta', 'Idioma de la metadata', 'Debe estar entre los que soporta la tienda o Apple lo ignora'],
  'media-user-token':      ['Cuenta', 'Token de usuario', 'Se toma solo del motor. Solo tócalo si sabes lo que haces'],
  'decrypt-port':          ['Cuenta', 'Puerto del motor', 'Donde escucha el wrapper. 127.0.0.1:10020 salvo que lo hayas movido'],
  'alac-max':              ['Calidad', 'ALAC máximo', 'Techo, no selector: se toma la mejor variante que quepa debajo', [192000, 96000, 48000, 44100]],
  'atmos-max':             ['Calidad', 'Atmos máximo (kbps)', 'Techo del bitrate de Atmos', [2768, 2448]],
  'aac-type':              ['Calidad', 'Tipo de AAC', 'aac normal, binaural o downmix', ['aac', 'aac-binaural', 'aac-downmix']],
  'mv-max':                ['Calidad', 'Vídeo máximo (altura)', 'Se toma el mejor que no pase de esta altura', [2160, 1080, 720, 480]],
  'mv-audio-type':         ['Calidad', 'Audio del vídeo', 'Si el vídeo no lo trae, cae al mejor que tenga', ['atmos', 'ac3', 'aac']],
  'output-dir':            ['Carpetas', 'Carpeta de salida', 'Donde acaba todo'],
  'album-folder-format':   ['Carpetas', 'Carpeta de álbum', '{AlbumName} {ArtistName} {ReleaseYear} {UPC} {RecordLabel} {Quality} {Tag}'],
  'playlist-folder-format':['Carpetas', 'Carpeta de playlist', '{PlaylistName} {PlaylistId}'],
  'artist-folder-format':  ['Carpetas', 'Carpeta de artista', 'Vacío = sin carpeta de artista'],
  'song-file-format':      ['Carpetas', 'Nombre de archivo', '{SongNumer} {SongName} {DiscNumber} {TrackNumber} {Quality} {Tag}'],
  'explicit-choice':       ['Carpetas', 'Etiqueta de explícito', 'Lo que sustituye a {Tag} en contenido explícito'],
  'clean-choice':          ['Carpetas', 'Etiqueta de limpio', 'Lo que sustituye a {Tag} en contenido limpio'],
  'cover-size':            ['Extras', 'Carátula embebida', 'La carátula suelta se baja siempre a resolución nativa', ['1200x1200', '3000x3000', '6000x6000']],
  'save-cover':            ['Extras', 'Guardar carátula aparte', 'cover.jpg junto a la música, al tamaño que publique Apple'],
  'save-lrc':              ['Extras', 'Guardar letras (.lrc)', 'Necesita token de usuario'],
  'embed-lrc':             ['Extras', 'Letras dentro del archivo', 'Además del .lrc'],
  'separate-quality-folders': ['Carpetas', 'Una carpeta por calidad', 'ALAC/, Atmos/, AAC/… Sin esto, el mismo track en otra calidad choca de nombre y se salta con "ya estaba"'],
  'save-animated-artwork': ['Extras', 'Artwork animado', 'Usa el ffmpeg que trae la app. Solo algunos álbumes lo tienen'],
  'ffmpeg-path':           ['Avanzado', 'Ruta de ffmpeg', 'Vacío o "ffmpeg" = el que viene con la app'],
  'widevine-device-key':   ['Avanzado', 'Llave de dispositivo (vídeos)', 'Vacío = se busca en la carpeta de config'],
  'widevine-client-id':    ['Avanzado', 'ClientId (vídeos)', 'Vacío = se busca en la carpeta de config'],
};

let cfgCache = null;

function renderSettings(cfg) {
  const box = $('cfg-form');
  box.innerHTML = '';

  // Idioma de la ventana: no es del core, es de aquí.
  const langLab = document.createElement('label');
  langLab.innerHTML = `<span>Idioma de la app</span>`;
  const langSel = document.createElement('select');
  langSel.id = 'ui-lang';
  for (const l of i18n.languages()) {
    const o = document.createElement('option');
    o.value = l.code;
    o.textContent = l.name;
    if (l.code === i18n.current()) o.selected = true;
    langSel.appendChild(o);
  }
  langSel.addEventListener('change', () => {
    i18n.use(langSel.value);
    localStorage.setItem('ecam.lang', langSel.value);
    applyLanguage();
  });
  langLab.appendChild(langSel);
  box.appendChild(langLab);

  const groups = {};
  for (const key of Object.keys(cfg)) {
    const [group, label, help, choices] = CFG[key] || ['Avanzado', key];
    (groups[group] = groups[group] || []).push({ key, label, help, choices });
  }

  for (const group of ['Cuenta', 'Calidad', 'Carpetas', 'Extras', 'Avanzado']) {
    if (!groups[group]) continue;
    const h = document.createElement('h3');
    h.textContent = group;
    box.appendChild(h);

    for (const { key, label, help, choices } of groups[group]) {
      const value = cfg[key];
      const lab = document.createElement('label');
      if (typeof value === 'boolean') {
        lab.className = 'check';
        lab.innerHTML = `<input type="checkbox" data-key="${key}" ${value ? 'checked' : ''} />
          <span>${label}</span>${help ? `<em class="help">${help}</em>` : ''}`;
      } else if (choices) {
        // Opciones fijas: en un campo de texto se escriben mal y el fallo sale
        // mucho después, a mitad de una descarga.
        const opts = choices.map((c) => `<option value="${c}" ${String(c) === String(value) ? 'selected' : ''}>${c}</option>`).join('');
        lab.innerHTML = `<span>${label}</span><select data-key="${key}">${opts}</select>
          ${help ? `<em class="help">${help}</em>` : ''}`;
      } else {
        lab.innerHTML = `<span>${label}</span>
          <input data-key="${key}" value="${value ?? ''}" ${typeof value === 'number' ? 'inputmode="numeric"' : ''} />
          ${help ? `<em class="help">${help}</em>` : ''}`;
      }
      box.appendChild(lab);
    }
  }
}

async function refreshWidevine() {
  try {
    $('wv-state').textContent = (await ecam.widevineReady()) ? t('wv_ok') : t('wv_missing');
  } catch { /* la vista previa no aplica */ }
}

$('btn-settings').addEventListener('click', async () => {
  try {
    cfgCache = await ecam.getConfig();
    renderSettings(cfgCache);
    refreshWidevine();
    $('settings').showModal();
  } catch (e) {
    addRow(String(e), false);
  }
});

// Cerrar SIEMPRE se puede: con la ✕, con Escape o pulsando fuera. Antes, si el
// guardado fallaba, el diálogo se quedaba abierto y había que relanzar la app.
const closeSettings = () => $('settings').close();
$('cfg-close').addEventListener('click', (e) => { e.preventDefault(); closeSettings(); });
$('settings').addEventListener('click', (e) => { if (e.target === $('settings')) closeSettings(); });

$('cfg-save').addEventListener('click', async (e) => {
  e.preventDefault();
  // Solo se manda lo que cambió: el core mezcla sobre lo que ya hay.
  const patch = {};
  for (const input of $('cfg-form').querySelectorAll('[data-key]')) {
    const key = input.dataset.key;
    const before = cfgCache[key];
    const value = input.type === 'checkbox' ? input.checked
      : typeof before === 'number' ? (Number(input.value) || 0)
      : input.value;
    if (value !== before) patch[key] = value;
  }
  try {
    if (Object.keys(patch).length) await ecam.setConfig(patch);
    closeSettings();
  } catch (err) {
    addRow(String(err), false);
    closeSettings();   // el error ya se ve en la lista: no se secuestra la ventana
  }
});

$('wv-load').addEventListener('click', async (e) => {
  e.preventDefault();
  const files = await window.__TAURI__.dialog.open({
    multiple: true,
    title: 'device.pem + client_id.bin',
  });
  if (!files) return;
  try {
    const dir = await ecam.importWidevine(Array.isArray(files) ? files : [files]);
    await refreshWidevine();
    addRow(`✓ ${dir}`);
  } catch (err) {
    $('wv-state').textContent = String(err);
  }
});

$('btn-install').addEventListener('click', async () => {
  const file = await window.__TAURI__.dialog.open({
    filters: [{ name: 'ECAM', extensions: ['gz', 'tar.gz'] }],
  });
  if (!file) return;
  $('install-hint').textContent = t('loading');
  try {
    await ecam.installDistro(file);
    await refresh();
  } catch (err) {
    $('install-hint').textContent = String(err);
  }
});

// ── login ──────────────────────────────────────────────────────────────────
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

// ── motor remoto (macOS/Linux) ─────────────────────────────────────────────
// Fuera de Windows no hay WSL: el wrapper corre en otra máquina (o en otra cosa
// de esta) y lo único que la app necesita saber es a qué dirección hablarle. Se
// guarda en el config como cualquier otro ajuste, y del host sale también el
// 30020 de la cuenta.
$('form-connect').addEventListener('submit', async (e) => {
  e.preventDefault();
  $('connect-error').textContent = '';
  const addr = $('connect-addr').value.trim();
  if (!/^[^\s:]+:\d{1,5}$/.test(addr)) {
    $('connect-error').textContent = t('connect_bad_addr');
    return;
  }
  try {
    await ecam.setConfig({ 'decrypt-port': addr });
    // Solo se sale de aquí si el motor contesta de verdad. Guardar y entrar a
    // ciegas deja la app dentro con todo roto y sin decir por qué.
    const state = await ecam.wrapperState();
    if (state.listening) await refresh();
    else $('connect-error').textContent = t('connect_unreachable');
  } catch (err) {
    $('connect-error').textContent = String(err);
  }
});

// ── arranque ───────────────────────────────────────────────────────────────
async function refresh() {
  const forced = new URLSearchParams(location.search).get('screen');
  if (forced && screens.includes(forced)) {
    show(forced);
    if (forced === '2fa') startCountdown();
    return;
  }
  const state = await ecam.wrapperState();
  const next = ecam.screenFor(state);
  if (next === 'connect') {
    // Se rellena con lo que ya haya para no obligar a teclearlo cada vez.
    try { $('connect-addr').value = (await ecam.getConfig())['decrypt-port'] || ''; } catch { /* sin core */ }
  }
  show(next);
  if (state.has_session && !state.listening) await ecam.startWrapper(null, null);
  renderHistory();
}

(async () => {
  let cfg = null;
  try { cfg = await ecam.getConfig(); } catch { /* aún sin core */ }
  i18n.use(localStorage.getItem('ecam.lang') || i18n.detect(cfg?.language));
  applyLanguage();
  try {
    await refresh();
  } catch (e) {
    document.body.innerHTML = `<p class="error center">${e}</p>`;
  }
})();
