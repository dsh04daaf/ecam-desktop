// Prueba del puente UI↔core sin navegador ni Tauri.
//
// Existe por la cicatriz de ECBP: se publicó una versión que se veía bien y no
// hablaba con su propio motor, porque el puente tomaba el index.html que
// devolvía el protocolo de assets por datos buenos. Esta prueba lo caza.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
globalThis.window = globalThis;
globalThis.fetch = async () => { throw new Error('sin red en la prueba'); };
const bridge = require('../src/bridge.js');

const jsonRes = (body, ok = true, status = 200) => ({
  ok, status,
  headers: { get: () => 'application/json' },
  json: async () => body,
});
const htmlRes = () => ({
  ok: true, status: 200,
  headers: { get: () => 'text/html; charset=utf-8' },
  json: async () => ({ pareceDatos: true }),
});

test('dentro de la app se usa invoke y no HTTP', async () => {
  const calls = [];
  const call = bridge.makeCall({
    invoke: (cmd, args) => { calls.push([cmd, args]); return Promise.resolve('ok'); },
    isApp: true,
    fetchImpl: () => { throw new Error('no debería salir por HTTP'); },
  });
  assert.equal(await call('search', { term: 'x' }), 'ok');
  assert.deepEqual(calls[0], ['search', { term: 'x' }]);
});

test('dentro de la app SIN invoke se grita: es un fallo de build, no un navegador', async () => {
  const call = bridge.makeCall({ invoke: null, isApp: true, fetchImpl: () => htmlRes() });
  await assert.rejects(() => call('search'), /fallo de build/);
});

test('el HTML del servidor de assets NUNCA se toma por datos', async () => {
  const call = bridge.makeCall({ invoke: null, isApp: false, fetchImpl: async () => htmlRes() });
  await assert.rejects(() => call('search'), /no devolvió JSON/);
});

test('en el navegador se cae a HTTP y se parsea el JSON', async () => {
  let url = '';
  const call = bridge.makeCall({
    invoke: null, isApp: false,
    fetchImpl: async (u) => { url = u; return jsonRes([{ id: '1' }]); },
  });
  assert.deepEqual(await call('search', { term: 'x' }), [{ id: '1' }]);
  // Relativa a propósito: con barra inicial, servida bajo un subcamino,
  // la petición se va a la raíz del dominio (y a otra app).
  assert.equal(url, 'invoke/search');
});

test('un error del core llega con su motivo, no como HTTP 500 pelado', async () => {
  const call = bridge.makeCall({
    invoke: null, isApp: false,
    fetchImpl: async () => jsonRes({ error: 'el wrapper no responde' }, false, 500),
  });
  await assert.rejects(() => call('x'), /el wrapper no responde/);
});

test('se encuentra invoke por las dos vías que expone Tauri v2', () => {
  assert.ok(bridge.findInvoke({ __TAURI__: { core: { invoke: () => {} } } }));
  assert.ok(bridge.findInvoke({ __TAURI_INTERNALS__: { invoke: () => {} } }));
  assert.equal(bridge.findInvoke({}), null);
});

test('se detecta estar dentro de la app aunque el puente esté roto', () => {
  assert.ok(bridge.inApp({ __TAURI_INTERNALS__: {} }));
  assert.ok(bridge.inApp({ location: { origin: 'http://tauri.localhost' } }));
  assert.ok(!bridge.inApp({ location: { origin: 'http://127.0.0.1:3026' } }));
});

test('cada comando llega al core con su nombre y sus argumentos exactos', async () => {
  const calls = [];
  const cmds = bridge.makeCommands((cmd, args) => { calls.push([cmd, args]); return Promise.resolve(1); });

  await cmds.wrapperState();
  await cmds.startWrapper('a@b.com', 'clave');
  await cmds.submitTwoFactor('123456');
  await cmds.search('garrix');
  await cmds.download('https://music.apple.com/nz/album/x/1', 'alac');
  // Una tarjeta manda tipo e id: la URL (y con ella la tienda) la arma el core.
  await cmds.downloadItem('artist', '123', 'alac');
  await cmds.cancel(7);

  assert.deepEqual(calls, [
    ['wrapper_state', undefined],
    ['start_wrapper', { user: 'a@b.com', password: 'clave' }],
    ['submit_two_factor', { code: '123456' }],
    ['search', { term: 'garrix' }],
    ['download', { url: 'https://music.apple.com/nz/album/x/1', quality: 'alac' }],
    ['download_item', { kind: 'artist', id: '123', quality: 'alac' }],
    ['cancel', { job: 7 }],
  ]);
});

test('la pantalla se elige según lo que diga el core', () => {
  const wsl = (s) => ({ backend: 'wsl', ...s });
  assert.equal(bridge.screenFor(wsl({ distro_installed: false, has_session: false })), 'install');
  assert.equal(bridge.screenFor(wsl({ distro_installed: true, has_session: false })), 'login');
  assert.equal(bridge.screenFor(wsl({ distro_installed: true, has_session: true })), 'main');
});

// Fuera de Windows el motor está fuera de la app. Sin esto, `distro_installed`
// sale true y `has_session` false, y la app manda al usuario a la pantalla de
// login de Apple, que en ese modo NO HACE NADA (`launch_command` es un `true`).
test('con el motor fuera, la pantalla es conectar hasta que responda', () => {
  const ext = (s) => ({ backend: 'external', distro_installed: true, ...s });
  assert.equal(bridge.screenFor(ext({ has_session: false, listening: false })), 'connect');
  assert.equal(bridge.screenFor(ext({ has_session: true, listening: true })), 'main');
});

test('un link pegado se reconoce y no se manda al buscador', () => {
  assert.ok(bridge.isAppleUrl('https://music.apple.com/nz/album/hyperspace/1234'));
  assert.ok(bridge.isAppleUrl('https://music.apple.com/us/music-video/x/999'));
  assert.ok(!bridge.isAppleUrl('martin garrix'));
  assert.ok(!bridge.isAppleUrl('https://open.spotify.com/album/x'));
});
