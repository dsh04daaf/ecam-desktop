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

function addRow(text, ok = true) {
  const li = document.createElement('li');
  li.textContent = text;
  li.className = ok ? '' : 'bad';
  $('downloads').prepend(li);
  return li;
}

ecam.listen('track', (t) => {
  addRow(t.ok ? `✓ ${t.name} — ${t.detail}` : `✗ ${t.detail}`, t.ok);
});

ecam.listen('finished', (f) => {
  const li = rows.get(f.job);
  if (li) li.remove();
  addRow(f.ok ? `Listo: ${f.done} pistas${f.failed ? `, ${f.failed} con problemas` : ''}` : `Error: ${f.error}`, f.ok);
});

async function startDownload(url) {
  const li = addRow(`Bajando ${url}…`);
  const job = await ecam.download(url, $('quality').value);
  rows.set(job, li);
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
  const hits = await ecam.search(text);
  $('results').innerHTML = '';
  hits.forEach((h) => {
    const card = document.createElement('article');
    card.className = 'card';
    card.innerHTML = `<img src="${h.artwork}" alt="" loading="lazy" />
      <div class="meta"><strong>${h.name}</strong><span>${h.artist}</span><em>${h.kind}</em></div>`;
    card.addEventListener('click', () => {
      const kind = h.kind === 'music-video' ? 'music-video' : h.kind;
      startDownload(`https://music.apple.com/us/${kind}/x/${h.id}`);
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
