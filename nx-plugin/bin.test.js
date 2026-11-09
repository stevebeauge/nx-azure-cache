// `nx-azure-cache` command tests. The binary is Node itself (`NX_AZURE_CACHE_BIN`), run with
// `-e`, so that a test sees the arguments, environment and exit code the command passes on.
'use strict';

const { test, beforeEach, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const CLI = path.join(__dirname, '..', 'npm', 'nx-azure-cache', 'bin.cjs');
const VARS = ['NX_AZURE_CACHE_ACCOUNT', 'NX_AZURE_CACHE_TENANT_ID', 'NX_AZURE_CACHE_CLIENT_ID'];
const PRINT_ENV = `console.log(JSON.stringify([${VARS.map((v) => `process.env.${v}`)}]))`;
let dir, cwd;

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'nx-azure-cache-bin-'));
  // The command runs from a sub-directory of the Workspace.
  cwd = path.join(dir, 'apps', 'web');
  fs.mkdirSync(cwd, { recursive: true });
});
afterEach(() => fs.rmSync(dir, { recursive: true, force: true }));

function cli(args, env = {}) {
  const base = { ...process.env, NX_AZURE_CACHE_BIN: process.execPath };
  for (const v of VARS) delete base[v];
  return spawnSync(process.execPath, [CLI, ...args], { cwd, env: { ...base, ...env }, encoding: 'utf8' });
}
const nxJson = (content) =>
  fs.writeFileSync(path.join(dir, 'nx.json'), typeof content === 'string' ? content : JSON.stringify(content));
const printed = (r) => JSON.parse(r.stdout);

const PLUGIN = {
  plugin: 'nx-azure-cache',
  options: { workspace: 'web', account: 'acc', tenantId: 'tid', clientId: 'cid' },
};

test('passes the nx.json settings of the plugin entry, found from a sub-directory', () => {
  nxJson({ plugins: ['@nx/js', { plugin: '@nx/vite/plugin', options: { account: 'other' } }, PLUGIN] });
  const r = cli(['-e', PRINT_ENV]);
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(printed(r), ['acc', 'tid', 'cid']);
});

test('explicit variables win over nx.json', () => {
  nxJson({ plugins: [PLUGIN] });
  const r = cli(['-e', PRINT_ENV], { NX_AZURE_CACHE_ACCOUNT: 'mine' });
  assert.deepEqual(printed(r), ['mine', 'tid', 'cid']);
});

test('no nx.json: environment unchanged, no error', () => {
  const r = cli(['-e', PRINT_ENV]);
  assert.equal(r.status, 0);
  assert.equal(r.stderr, '');
  assert.deepEqual(printed(r), [null, null, null]);
});

test('unreadable nx.json: environment unchanged, no error', () => {
  nxJson('{ not json');
  const r = cli(['-e', PRINT_ENV]);
  assert.equal(r.status, 0);
  assert.equal(r.stderr, '');
  assert.deepEqual(printed(r), [null, null, null]);
});

test('forwards arguments, output and exit code', () => {
  const r = cli(['-e', 'console.log(process.argv.slice(1).join("|")); console.error("err"); process.exit(3)', 'a b', '--json']);
  assert.equal(r.status, 3);
  assert.equal(r.stdout.trim(), 'a b|--json');
  assert.equal(r.stderr.trim(), 'err');
});

test('no binary for the platform: says so, exit code 1', () => {
  const r = cli(['version'], { NX_AZURE_CACHE_BIN: '' });
  assert.equal(r.status, 1);
  assert.match(r.stderr, new RegExp(`no Gateway binary for ${process.platform}-${process.arch}`));
});
