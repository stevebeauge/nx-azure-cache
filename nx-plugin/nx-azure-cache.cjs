// nx-azure-cache, Activation plugin, version 0.1.0
// https://github.com/stevebeauge/nx-azure-cache
//
// Copy into the Workspace's `tools/`, then declare it in `nx.json`:
//   {"plugin": "./tools/nx-azure-cache.cjs", "options": {"workspace": "<workspace>"}}
// Shipped as `.cjs`: a Workspace with `"type": "module"` would load a `.js` as an ES module,
// and its `require` would make every `nx` command fail.
//
// Before tasks run, sets the remote cache URL if the local Gateway is healthy, and sets
// nothing otherwise (Safe fallback). Nx ignores the hook's return value: only assignments
// to `process.env` reach the client, daemon and isolation included.
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const SERVER = 'NX_SELF_HOSTED_REMOTE_CACHE_SERVER';
const TOKEN = 'NX_SELF_HOSTED_REMOTE_CACHE_ACCESS_TOKEN';

/** Local token path, same as the Gateway's (`src/config.rs`). */
function tokenPath(env) {
  const base =
    process.platform === 'win32'
      ? env.APPDATA
      : env.XDG_CONFIG_HOME || path.join(env.HOME || os.homedir(), '.config');
  return path.join(base, 'nx-azure-cache', 'local-token');
}

/** Returns why the remote cache stays off, or `null` once it is set. */
async function activate(options, env) {
  const workspace = options && options.workspace;
  if (!workspace) return 'option `workspace` missing from nx.json';
  if (!/^[a-z0-9][a-z0-9-]{0,62}$/.test(workspace)) return `invalid workspace: ${workspace}`;
  const port = Number((options && options.port) || env.NX_AZURE_CACHE_PORT || 7484);

  let health;
  try {
    // The timeout covers both the connection and reading the body.
    const res = await fetch(`http://127.0.0.1:${port}/health`, {
      signal: AbortSignal.timeout(200),
    });
    health = await res.json();
  } catch (e) {
    if (e.name === 'TimeoutError') return `Gateway unresponsive on port ${port} (> 200 ms)`;
    if (e.name === 'SyntaxError') return `unreadable /health response on port ${port}`;
    return `no Gateway on port ${port}`;
  }
  if (!health || health.service !== 'nx-azure-cache') {
    return `port ${port} is not held by a Gateway`;
  }
  if (!health.identity || health.identity.ready !== true) {
    return (health.identity && health.identity.reason) || 'Identity not ready';
  }

  const file = tokenPath(env);
  let token;
  try {
    token = fs.readFileSync(file, 'utf8').trim();
  } catch {
    token = '';
  }
  if (!/^[0-9a-f]{64}$/i.test(token)) return `unreadable local token (${file})`;

  // Assignments, not a return value: this is what Nx propagates.
  env[SERVER] = `http://127.0.0.1:${port}/${workspace}`;
  env[TOKEN] = token;
  return null;
}

async function preTasksExecution(options) {
  // The hook never throws: an exception would fail the Nx command.
  try {
    const env = process.env;
    const off = (env.NX_AZURE_CACHE_DISABLED || '').toLowerCase();
    if (off && off !== '0' && off !== 'false') {
      console.log('[nx-azure-cache] remote cache off: NX_AZURE_CACHE_DISABLED');
      return;
    }
    if (env[SERVER]) {
      console.log(`[nx-azure-cache] ${SERVER} already set, plugin has no effect`);
      return;
    }
    const reason = await activate(options, env);
    if (reason) console.log(`[nx-azure-cache] remote cache off: ${reason}`);
  } catch (e) {
    try {
      console.log(`[nx-azure-cache] remote cache off: ${e && e.message}`);
    } catch {}
  }
}

module.exports = { name: 'nx-azure-cache', preTasksExecution };
