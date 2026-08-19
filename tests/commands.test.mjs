// Guarda entre los dos lados del puente: que cada comando que llama la UI
// exista en el `invoke_handler` de Rust, y al revés.
//
// Sin esto, una pulsación puede llamar a un comando que no existe y el fallo
// solo aparece en tiempo de ejecución, en la máquina del usuario.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
globalThis.window = globalThis;
globalThis.fetch = async () => { throw new Error('sin red'); };
const bridge = require('../src/bridge.js');

const main = readFileSync(new URL('../src-tauri/src/main.rs', import.meta.url), 'utf8');
const handler = main.match(/generate_handler!\[([\s\S]*?)\]/);

test('el invoke_handler de Rust se puede leer', () => {
  assert.ok(handler, 'no se encontró generate_handler! en main.rs');
});

const rustCommands = new Set(
  handler[1].split(',').map((s) => s.trim()).filter(Boolean)
);

const uiCommands = (() => {
  const seen = [];
  const cmds = bridge.makeCommands((cmd) => { seen.push(cmd); return Promise.resolve(); });
  // Se invoca cada comando con argumentos de mentira solo para leer su nombre.
  Object.values(cmds).forEach((fn) => { try { fn('x', 'y', 'z'); } catch {} });
  return seen;
})();

test('todo comando que llama la UI existe en Rust', () => {
  const faltan = uiCommands.filter((c) => !rustCommands.has(c));
  assert.deepEqual(faltan, [], `la UI llama a comandos que no existen: ${faltan.join(', ')}`);
});

test('no hay comandos en Rust que nadie llame', () => {
  const sobran = [...rustCommands].filter((c) => !uiCommands.includes(c));
  assert.deepEqual(sobran, [], `comandos muertos en Rust: ${sobran.join(', ')}`);
});
