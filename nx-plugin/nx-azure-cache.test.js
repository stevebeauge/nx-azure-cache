// Activation plugin unit tests, against a fake HTTP Gateway.
'use strict';

const { test, beforeEach, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const http = require('node:http');
const os = require('node:os');
const path = require('node:path');
const { preTasksExecution } = require('./nx-azure-cache.cjs');

const SERVER = 'NX_SELF_HOSTED_REMOTE_CACHE_SERVER';
const TOKEN = 'NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN';
const TOKEN_VALUE = 'ab'.repeat(32);
const HEALTHY = { service: 'nx-azure-cache', version: '0.1.0', identity: { ready: true, kind: 'user' }, write: 'unknown' };

let savedEnv, lines, server, port, configDir;

/** Starts a fake Gateway whose `/health` is served by `handler`. */
async function gateway(handler) {
  server = http.createServer(handler);
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  port = server.address().port;
  process.env.NX_AZURE_CACHE_PORT = String(port);
}
const json = (body) => (req, res) => res.end(typeof body === 'string' ? body : JSON.stringify(body));

beforeEach(() => {
  savedEnv = { ...process.env };
  for (const k of [SERVER, TOKEN, 'NX_AZURE_CACHE_DISABLED', 'NX_AZURE_CACHE_PORT']) delete process.env[k];
  // Local token in a throwaway config directory, at both possible locations.
  configDir = fs.mkdtempSync(path.join(os.tmpdir(), 'nx-azure-cache-'));
  process.env.APPDATA = configDir;
  process.env.XDG_CONFIG_HOME = configDir;
  fs.mkdirSync(path.join(configDir, 'nx-azure-cache'));
  fs.writeFileSync(path.join(configDir, 'nx-azure-cache', 'local-token'), TOKEN_VALUE + '\n');
  lines = [];
  console.log = (...a) => lines.push(a.join(' '));
});

afterEach(async () => {
  delete console.log; // restores the prototype method
  process.env = savedEnv;
  if (server) {
    server.closeAllConnections();
    await new Promise((r) => server.close(r));
    server = undefined;
  }
  fs.rmSync(configDir, { recursive: true, force: true });
});

/** Checks that nothing is set and that a single line matches `pattern`. */
function off(pattern) {
  assert.equal(process.env[SERVER], undefined);
  assert.equal(process.env[TOKEN], undefined);
  assert.equal(lines.length, 1, lines.join('\n'));
  assert.match(lines[0], /^\[nx-azure-cache\] remote cache off: /);
  assert.match(lines[0], pattern);
}

test('healthy Gateway: both variables are set', async () => {
  await gateway(json(HEALTHY));
  await preTasksExecution({ workspace: 'my-workspace' });
  assert.equal(process.env[SERVER], `http://127.0.0.1:${port}/my-workspace`);
  assert.equal(process.env[TOKEN], TOKEN_VALUE);
  assert.deepEqual(lines, []);
});

test('options.port takes precedence over NX_AZURE_CACHE_PORT', async () => {
  await gateway(json(HEALTHY));
  process.env.NX_AZURE_CACHE_PORT = '1';
  await preTasksExecution({ workspace: 'd', port });
  assert.equal(process.env[SERVER], `http://127.0.0.1:${port}/d`);
});

test('no Gateway', async () => {
  await gateway(json(HEALTHY));
  server.close();
  await preTasksExecution({ workspace: 'd' });
  off(/no Gateway/);
});

test('ready=false: the Gateway reason is passed through', async () => {
  await gateway(json({ ...HEALTHY, identity: { ready: false, kind: null, reason: 'account missing from config' } }));
  await preTasksExecution({ workspace: 'd' });
  off(/account missing from config/);
});

test('slow response (> 200 ms)', async () => {
  await gateway((req, res) => setTimeout(() => res.end(JSON.stringify(HEALTHY)), 1000));
  const t0 = Date.now();
  await preTasksExecution({ workspace: 'd' });
  assert.ok(Date.now() - t0 < 800); // aborted at 200 ms, margin for a loaded machine
  off(/200 ms/);
});

test('invalid JSON', async () => {
  await gateway(json('not json'));
  await preTasksExecution({ workspace: 'd' });
  off(/unreadable/);
});

test('port held by something else', async () => {
  await gateway(json({ status: 'ok' }));
  await preTasksExecution({ workspace: 'd' });
  off(/not held by a Gateway/);
});

test('unreadable token', async () => {
  await gateway(json(HEALTHY));
  fs.rmSync(path.join(configDir, 'nx-azure-cache', 'local-token'));
  await preTasksExecution({ workspace: 'd' });
  off(/unreadable local token/);
});

test('NX_AZURE_CACHE_DISABLED honored, without probing', async () => {
  let probed = false;
  await gateway((req, res) => { probed = true; json(HEALTHY)(req, res); });
  process.env.NX_AZURE_CACHE_DISABLED = 'true';
  await preTasksExecution({ workspace: 'd' });
  off(/NX_AZURE_CACHE_DISABLED/);
  assert.equal(probed, false);
});

test('pre-existing URL honored', async () => {
  await gateway(json(HEALTHY));
  process.env[SERVER] = 'http://elsewhere/x';
  await preTasksExecution({ workspace: 'd' });
  assert.equal(process.env[SERVER], 'http://elsewhere/x');
  assert.equal(process.env[TOKEN], undefined);
  assert.equal(lines.length, 1);
});

test('workspace missing or invalid', async () => {
  await gateway(json(HEALTHY));
  await preTasksExecution({});
  off(/workspace/);
  lines = [];
  await preTasksExecution(undefined);
  off(/workspace/);
  lines = [];
  await preTasksExecution({ workspace: 'My_Workspace' });
  off(/invalid workspace/);
});

test('an internal exception is caught', async () => {
  await preTasksExecution({ get workspace() { throw new Error('boom'); } });
  off(/boom/);
});
