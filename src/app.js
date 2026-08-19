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
  setText(rows.get(p.job), `Bajando… ${fmtMB(p.bytes)}`);
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
      // Un artista son TODOS sus álbumes: eso no se dispara de una pulsación
      // sin avisar. (Antes, además, no había forma de pararlo.)
      if (h.bulk && !confirm(`«${h.name}» puede ser una descarga muy larga (${h.kind}). ¿Seguir?`)) return;
      const li = addRow(`Bajando ${h.name}…`, true, true);
      await run(li, ecam.downloadItem(h.kind, h.id, $('quality').value));
    });
    $('results').appendChild(card);
  });
});

// ── ajustes ────────────────────────────────────────────────────────────────
$('btn-settings').addEventListener('click', async () => {
  const cfg = await ecam.getConfig();
  $('cfg-out').value = cfg['output-dir'] ?? cfg.output_dir ?? '';
  $('cfg-store').value = cfg.storefront ?? '';
  $('cfg-lang').value = cfg.language ?? '';
  $('cfg-lrc').checked = !!(cfg['save-lrc'] ?? cfg.save_lrc);
  $('cfg-cover').checked = !!(cfg['save-cover'] ?? cfg.save_cover);
  $('cfg-anim').checked = !!(cfg['save-animated-artwork'] ?? cfg.save_animated_artwork);
  $('settings').showModal();
});

$('cfg-save').addEventListener('click', async (e) => {
  e.preventDefault();
  const cfg = await ecam.getConfig();
  cfg['output-dir'] = $('cfg-out').value;
  cfg.storefront = $('cfg-store').value || 'auto';
  cfg.language = $('cfg-lang').value;
  cfg['save-lrc'] = $('cfg-lrc').checked;
  cfg['save-cover'] = $('cfg-cover').checked;
  cfg['save-animated-artwork'] = $('cfg-anim').checked;
  await ecam.setConfig(cfg);
  $('settings').close();
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
