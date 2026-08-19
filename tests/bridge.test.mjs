// Prueba del puente UI↔core sin navegador ni Tauri.
// Existe por la lección de ECBP: se publicó una versión que se veía preciosa y
// no hablaba con su propio motor. Esto lo caza en el CI antes de empaquetar.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
globalThis.window = globalThis;
const bridge = require('../src/bridge.js');

test('sin el puente global, el error es explícito y no un undefined', () => {
  delete globalThis.__TAURI__;
  assert.throws(() => bridge.invoke('lo_que_sea'), /withGlobalTauri/);
});

test('cada comando de la UI llega al core con su nombre y sus argumentos', async () => {
  const calls = [];
  globalThis.__TAURI__ = {
    core: { invoke: (cmd, args) => { calls.push([cmd, args]); return Promise.resolve('ok'); } },
    event: { listen: () => {} },
  };

  await bridge.startWrapper('a@b.com', 'clave');
  await bridge.submitTwoFactor('123456');
  await bridge.download('https://music.apple.com/nz/album/x/1', 'alac');
  await bridge.search('garrix');

  assert.deepEqual(calls[0], ['start_wrapper', { user: 'a@b.com', password: 'clave' }]);
  assert.deepEqual(calls[1], ['submit_two_factor', { code: '123456' }]);
  assert.deepEqual(calls[2], ['download', { url: 'https://music.apple.com/nz/album/x/1', quality: 'alac' }]);
  assert.deepEqual(calls[3], ['search', { term: 'garrix' }]);
});

test('la pantalla se elige según lo que diga el core', () => {
  assert.equal(bridge.screenFor({ distro_installed: false, has_session: false }), 'install');
  assert.equal(bridge.screenFor({ distro_installed: true, has_session: false }), 'login');
  assert.equal(bridge.screenFor({ distro_installed: true, has_session: true }), 'main');
});

test('un link pegado se reconoce y no se manda al buscador', () => {
  assert.ok(bridge.isAppleUrl('https://music.apple.com/nz/album/hyperspace/1234'));
  assert.ok(bridge.isAppleUrl('https://music.apple.com/us/music-video/x/999'));
  assert.ok(!bridge.isAppleUrl('martin garrix'));
  assert.ok(!bridge.isAppleUrl('https://open.spotify.com/album/x'));
});
