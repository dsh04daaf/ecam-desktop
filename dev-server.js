// Servidor de vista previa — SOLO para ver y probar la UI desde el navegador.
//
// Emula los comandos de Tauri contra el core DE VERDAD (vía `examples/rpc`),
// para que `src/app.js` sea EXACTAMENTE el mismo código aquí y en la app. Es lo
// mismo que se hizo con ECBP Desktop antes de publicarlo.
//
// Corre en la VPS, así que usa el wrapper que ya tiene la sesión iniciada: no
// hay pantalla de login que pasar para probar el resto.
const express = require('express');
const { spawn } = require('child_process');
const path = require('path');

const PORT = 3026;
const RPC = path.join(__dirname, 'target/debug/examples/rpc');
const OUT = process.env.ECAM_OUT || '/srv/sandbox/ecam-wsl/preview-out';

const app = express();
app.use(express.json({ limit: '256kb' }));

/// Llama al core y devuelve la última línea JSON.
function rpc(args) {
  return new Promise((resolve, reject) => {
    const p = spawn(RPC, args, { env: { ...process.env, ECAM_OUT: OUT } });
    let out = '', err = '';
    p.stdout.on('data', (d) => (out += d));
    p.stderr.on('data', (d) => (err += d));
    p.on('close', (code) => {
      const lines = out.trim().split('\n').filter(Boolean);
      if (code !== 0 && !lines.length) return reject(new Error(err.trim() || `rpc salió con ${code}`));
      try { resolve(JSON.parse(lines[lines.length - 1])); }
      catch { reject(new Error(err.trim() || 'el core no devolvió JSON')); }
    });
  });
}

// ── comandos ───────────────────────────────────────────────────────────────
const jobs = new Map();      // job -> [sucesos pendientes]
let seq = 1;

const commands = {
  wrapper_state: () => rpc(['state']),
  get_config: () => rpc(['config']),
  // Escribir el config desde el navegador tocaría el de la VPS: se acepta y se
  // ignora a propósito, como el 501 del dev-server de ECBP.
  set_config: async () => ({ preview: true }),
  search: ({ term }) => rpc(['search', term]),

  async start_wrapper() { return { preview: true }; },
  async submit_two_factor() { return { preview: true }; },
  async sign_out() { throw new Error('en la vista previa no se cierra la sesión de la VPS'); },

  async download({ url, quality }) {
    const job = seq++;
    jobs.set(job, []);
    const p = spawn(RPC, ['download', url, quality || 'alac'], { env: { ...process.env, ECAM_OUT: OUT } });
    let buf = '';
    p.stdout.on('data', (d) => {
      buf += d;
      const lines = buf.split('\n');
      buf = lines.pop();
      for (const l of lines) {
        if (!l.trim()) continue;
        try { jobs.get(job)?.push({ ...JSON.parse(l), job }); } catch {}
      }
    });
    p.on('close', () => jobs.get(job)?.push({ event: 'closed', job }));
    return job;
  },

  async cancel() { return { preview: true }; },
  async install_distro() { throw new Error('en la vista previa la distro no aplica'); },
};

app.post('/invoke/:cmd', async (req, res) => {
  const fn = commands[req.params.cmd];
  if (!fn) return res.status(404).json({ error: `comando desconocido: ${req.params.cmd}` });
  try { res.json(await fn(req.body || {})); }
  catch (e) { res.status(500).json({ error: String(e.message || e) }); }
});

/// Sucesos pendientes (la UI los sondea; en la app son eventos de Tauri).
app.get('/events', (req, res) => {
  const out = [];
  for (const [job, list] of jobs) {
    while (list.length) out.push(list.shift());
    if (out.some((e) => e.event === 'closed' && e.job === job)) jobs.delete(job);
  }
  res.json(out);
});

app.use(express.static(path.join(__dirname, 'src')));
app.listen(PORT, '127.0.0.1', () => console.log(`vista previa de ECAM en http://127.0.0.1:${PORT}`));
