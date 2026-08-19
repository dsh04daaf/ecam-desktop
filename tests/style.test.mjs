// Guardas de CSS. Existen por bugs que YA se colaron en una build:
// un `display` en `dialog` a secas deja la ventana de ajustes pintada para
// siempre, aunque el código la cierre.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const css = readFileSync(new URL('../src/style.css', import.meta.url), 'utf8');

test('el display de dialog solo se toca cuando está [open]', () => {
  // Cualquier regla que ponga display en `dialog` sin [open] pisa el
  // display:none que le da el navegador al cerrarlo.
  const malas = css
    .split('}')
    .map((b) => b.trim())
    .filter((b) => /(^|,)\s*dialog\s*\{/.test(b + '{') || /^dialog\s*\{/.test(b))
    .filter((b) => /display\s*:/.test(b));
  assert.deepEqual(malas, [], 'dialog con display fuera de [open]');
});

test('las pantallas se ocultan con una clase que gana siempre', () => {
  assert.match(css, /\.hidden\s*\{\s*display:\s*none\s*!important/,
    'sin !important, cualquier regla de display deja una pantalla visible');
});
