// e2e: real Nx 23.2.1 against an in-memory fake Gateway, then against the real one started by
// the plugin, with and without the daemon.
// Prerequisite: `pnpm install` in this directory (done by `pnpm test:plugin`).
'use strict';

const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');

const ROOT = __dirname;
const NX = path.join(ROOT, 'node_modules', 'nx', 'dist', 'bin', 'nx.js');
const TOKEN_VALUE = 'cd'.repeat(32);
const DAEMON_LOG = path.join(ROOT, '.nx', 'workspace-data', 'd', 'daemon.log');

let server, port, configDir;
const store = new Map(); // `{workspace}/{hash}` → tarball
let requests = []; // `METHOD status path`

/** Fake Gateway: healthy `/health` and the Nx cache protocol, Local token required. */
function fakeGateway(req, res) {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    let status = 404;
    let body;
    const m = req.url.match(/^\/([a-z0-9-]+)\/v1\/cache\/([A-Za-z0-9]+)$/);
    const auth = req.headers.authorization === `Bearer ${TOKEN_VALUE}`;
    if (req.url === '/health') {
      status = 200;
      body = JSON.stringify({ service: 'nx-azure-cache', version: 'e2e', identity: { ready: true, kind: 'user' }, write: 'allowed' });
    } else if (m && req.method === 'GET') {
      body = auth ? store.get(`${m[1]}/${m[2]}`) : undefined;
      status = body ? 200 : 404;
    } else if (m && req.method === 'PUT') {
      const key = `${m[1]}/${m[2]}`;
      status = !auth ? 403 : store.has(key) ? 409 : 200;
      if (status === 200) store.set(key, Buffer.concat(chunks));
    }
    if (req.url !== '/health') requests.push(`${req.method} ${status} ${req.url}`);
    res.writeHead(status);
    res.end(body);
  });
}

/**
 * Runs `nx <args>` in the e2e workspace, without inheriting an ambient remote cache.
 * Asynchronous: the fake Gateway runs in this process.
 */
function nx(args, daemon, extra = {}) {
  const env = { ...process.env, NX_DAEMON: String(daemon), NX_NO_CLOUD: 'true', NX_TUI: 'false' };
  for (const k of Object.keys(env)) {
    if (k.startsWith('NX_SELF_HOSTED_') || k.startsWith('NX_AZURE_CACHE_')) delete env[k];
  }
  Object.assign(env, extra, { NX_AZURE_CACHE_PORT: String(port), APPDATA: configDir, XDG_CONFIG_HOME: configDir });
  const child = spawn(process.execPath, [NX, ...args], { cwd: ROOT, env });
  let out = '';
  child.stdout.on('data', (c) => (out += c));
  child.stderr.on('data', (c) => (out += c));
  return new Promise((resolve) => child.on('close', (code) => resolve({ code, out })));
}

// Clears the local cache and stops the daemon. `--only-cache` is not enough: Nx's database,
// left in place, still reports a local hit.
const reset = () => nx(['reset'], false);
const build = (daemon, sel = '', extra = {}) => nx(['run', 'proj:build'], daemon, { E2E_SEL: sel, ...extra });

before(async () => {
  // As in a real Workspace: the plugin is copied under `tools/`.
  fs.mkdirSync(path.join(ROOT, 'tools'), { recursive: true });
  fs.copyFileSync(path.join(ROOT, '..', 'nx-azure-cache.cjs'), path.join(ROOT, 'tools', 'nx-azure-cache.cjs'));
  configDir = fs.mkdtempSync(path.join(os.tmpdir(), 'nx-azure-cache-e2e-'));
  fs.mkdirSync(path.join(configDir, 'nx-azure-cache'));
  fs.writeFileSync(path.join(configDir, 'nx-azure-cache', 'local-token'), TOKEN_VALUE);
  server = http.createServer(fakeGateway);
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  port = server.address().port;
});

after(async () => {
  server.close();
  await reset();
  fs.rmSync(configDir, { recursive: true, force: true });
});

/** The fake Gateway received a request matching `re`. */
const seen = (re, out) => assert.ok(requests.some((r) => re.test(r)), `${requests}\n${out}`);

for (const daemon of [false, true]) {
  test(`PUT on 1st run, GET restored on 2nd (daemon: ${daemon})`, async () => {
    store.clear();
    requests = [];
    await reset();

    const first = await build(daemon);
    assert.equal(first.code, 0, first.out);
    seen(/^PUT 200 \/e2e\/v1\/cache\//, first.out);

    await reset();
    fs.rmSync(path.join(ROOT, 'dist'), { recursive: true, force: true });
    requests = [];
    const second = await build(daemon);
    assert.equal(second.code, 0, second.out);
    seen(/^GET 200 \/e2e\/v1\/cache\//, second.out);
    assert.match(second.out, /remote cache/);
    assert.equal(fs.readFileSync(path.join(ROOT, 'dist', 'proj', 'out.txt'), 'utf8'), 'built');

    // 3rd run without reset (daemon still alive, if any), on a new hash:
    // the plugin must re-enable the remote cache, not assume it is already set.
    requests = [];
    const third = await build(daemon, String(Date.now()));
    assert.equal(third.code, 0, third.out);
    seen(/^PUT 200 \/e2e\/v1\/cache\//, third.out);
    // The daemon did serve: it keeps its log in the workspace data directory.
    assert.equal(fs.existsSync(DAEMON_LOG), daemon);
  });
}

for (const daemon of [false, true]) {
  test(`fake Gateway stopped: the run exits with code 0 (daemon: ${daemon})`, async () => {
    server.close();
    await reset();
    const r = await build(daemon);
    assert.equal(r.code, 0, r.out);
    // With the daemon, the plugin line goes to the daemon log, not the terminal.
    const out = daemon ? fs.readFileSync(DAEMON_LOG, 'utf8') : r.out;
    assert.match(out, /\[nx-azure-cache\] remote cache off: no Gateway/);
    if (daemon) assert.doesNotMatch(r.out, /nx-azure-cache/);
  });
}

// The real Gateway (`pnpm test:plugin` builds it first).
const BIN = path.join(ROOT, '..', '..', 'target', 'debug', process.platform === 'win32' ? 'nx-azure-cache.exe' : 'nx-azure-cache');

async function health(p) {
  try {
    return await (await fetch(`http://127.0.0.1:${p}/health`)).json();
  } catch {
    return null;
  }
}

for (const daemon of [false, true]) {
  test(`no Gateway: the run starts it, it outlives Nx, the next run reuses it (daemon: ${daemon})`, async () => {
    server.close();
    await reset();
    const s = http.createServer();
    await new Promise((r) => s.listen(0, '127.0.0.1', r));
    port = s.address().port;
    await new Promise((r) => s.close(r));
    const extra = { NX_AZURE_CACHE_BIN: BIN, NX_AZURE_CACHE_TOKEN_STORE: 'file', LOCALAPPDATA: configDir, XDG_DATA_HOME: configDir };
    try {
      const first = await build(daemon, '', extra);
      assert.equal(first.code, 0, first.out);
      await reset(); // also stops the Nx daemon, parent of the Gateway when it is used
      assert.equal((await health(port))?.version, '0.1.0', 'the Gateway did not outlive Nx');

      const second = await build(daemon, '', extra);
      assert.equal(second.code, 0, second.out);
      const logs = path.join(configDir, 'nx-azure-cache', 'logs');
      const text = fs.readdirSync(logs).map((f) => fs.readFileSync(path.join(logs, f), 'utf8')).join('');
      assert.equal(text.split(`listening on http://127.0.0.1:${port}`).length - 1, 1, 'started twice');
    } finally {
      await fetch(`http://127.0.0.1:${port}/shutdown`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${TOKEN_VALUE}` },
      }).catch(() => {});
      while ((await health(port)) !== null) await new Promise((r) => setTimeout(r, 50));
      await new Promise((r) => setTimeout(r, 200)); // log file released before `after` removes it
    }
  });
}
