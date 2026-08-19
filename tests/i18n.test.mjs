// Los diccionarios se desincronizan solos: una clave nueva en español y el ruso
// se queda con un hueco (o con inglés a media pantalla). Esto lo caza en el CI.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const i18n = require('../src/i18n.js');

test('los tres idiomas tienen exactamente las mismas claves', () => {
  const es = Object.keys(i18n.dicts.es).sort();
  for (const code of ['en', 'ru']) {
    const otras = Object.keys(i18n.dicts[code]).sort();
    assert.deepEqual(otras, es, `faltan o sobran claves en ${code}`);
  }
});

test('ningún texto queda vacío', () => {
  for (const [code, dict] of Object.entries(i18n.dicts)) {
    for (const [k, v] of Object.entries(dict)) {
      assert.ok(String(v).trim().length > 0, `${code}.${k} está vacío`);
    }
  }
});

test('las variables se sustituyen', () => {
  i18n.use('es');
  assert.equal(i18n.t('tfa_left', { n: 42 }), 'Quedan 42 s');
  i18n.use('ru');
  assert.ok(i18n.t('bulk_warn', { name: 'X' }).includes('«X»'));
});

test('una clave que falte cae al español en vez de dejar un hueco', () => {
  i18n.use('en');
  assert.equal(i18n.t('no_existe_esta_clave'), 'no_existe_esta_clave');
});

test('el idioma se deduce del config o del sistema', () => {
  assert.equal(i18n.detect('ru-RU'), 'ru');
  assert.equal(i18n.detect('es-MX'), 'es');
  assert.equal(i18n.detect('en-GB'), 'en');
  assert.equal(i18n.detect('ja-JP'), 'en', 'lo que no tenemos cae al inglés');
  assert.ok(['es', 'en', 'ru'].includes(i18n.detect('')), 'sin config, algo válido');
});
