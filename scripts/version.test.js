// Version script tests, on a copy of the files it reads and writes.
'use strict';

const { test, beforeEach, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const ROOT = path.resolve(__dirname, '..');
const FILES = [
  'scripts/version.js',
  'Cargo.toml',
  'Cargo.lock',
  'npm/nx-azure-cache/package.json',
  'npm/win32-x64/package.json',
  'npm/linux-x64/package.json',
];
let dir;

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'nx-azure-cache-version-'));
  for (const f of FILES) {
    fs.mkdirSync(path.dirname(path.join(dir, f)), { recursive: true });
    fs.copyFileSync(path.join(ROOT, f), path.join(dir, f));
  }
});
afterEach(() => fs.rmSync(dir, { recursive: true, force: true }));

const run = (...args) => spawnSync(process.execPath, [path.join(dir, 'scripts/version.js'), ...args], { encoding: 'utf8' });
const read = (f) => fs.readFileSync(path.join(dir, f), 'utf8');
const pkg = (f) => JSON.parse(read(`npm/${f}/package.json`));

test('set: writes one version to Cargo.toml, Cargo.lock and the three packages', () => {
  const r = run('1.2.3');
  assert.equal(r.status, 0, r.stderr);
  // `\r?`: a Windows checkout has CRLF line endings.
  assert.match(read('Cargo.toml'), /^\[package\]\r?\nname = "nx-azure-cache"\r?\nversion = "1\.2\.3"\r?\n/);
  assert.match(read('Cargo.lock'), /\nname = "nx-azure-cache"\r?\nversion = "1\.2\.3"\r?\n/);
  for (const p of ['nx-azure-cache', 'win32-x64', 'linux-x64']) assert.equal(pkg(p).version, '1.2.3', p);
  assert.deepEqual(pkg('nx-azure-cache').optionalDependencies, {
    '@nx-azure-cache/linux-x64': '1.2.3',
    '@nx-azure-cache/win32-x64': '1.2.3',
  });
  assert.equal(run('--check', 'v1.2.3').status, 0);
});

test('set: refuses a version that is not x.y.z', () => {
  const r = run('1.2');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /x\.y\.z/);
  assert.equal(run('--check', 'v0.1.0').status, 0, 'files left unchanged');
});

test('--check: the repository agrees with itself', () => {
  const version = pkg('nx-azure-cache').version;
  assert.equal(run('--check', `v${version}`).status, 0);
});

test('--check: fails on a tag that differs from Cargo.toml', () => {
  const r = run('--check', 'v9.9.9');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /v9\.9\.9/);
});

test('--check: fails when one package.json disagrees', () => {
  const f = path.join(dir, 'npm/linux-x64/package.json');
  const p = JSON.parse(fs.readFileSync(f, 'utf8'));
  fs.writeFileSync(f, JSON.stringify({ ...p, version: '0.0.1' }));
  const r = run('--check', 'v0.1.0');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /npm\/linux-x64\/package\.json/);
});

test('--check: fails when an optional dependency disagrees', () => {
  const f = path.join(dir, 'npm/nx-azure-cache/package.json');
  const p = JSON.parse(fs.readFileSync(f, 'utf8'));
  p.optionalDependencies['@nx-azure-cache/win32-x64'] = '0.0.1';
  fs.writeFileSync(f, JSON.stringify(p));
  const r = run('--check', 'v0.1.0');
  assert.equal(r.status, 1);
  assert.match(r.stderr, /@nx-azure-cache\/win32-x64/);
});
