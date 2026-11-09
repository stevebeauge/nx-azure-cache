// One version for the Gateway and the npm packages (ADR 0006). `Cargo.toml` is the source.
//   node scripts/version.js 1.2.3          writes 1.2.3 to Cargo.toml, Cargo.lock and the three package.json
//   node scripts/version.js --check v1.2.3 fails unless the tag and every file agree
'use strict';
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const PACKAGES = ['nx-azure-cache', 'win32-x64', 'linux-x64'].map((p) => `npm/${p}/package.json`);
// The crate's own entry, in Cargo.toml and Cargo.lock: `name = "nx-azure-cache"` then `version`.
const CRATE = /(name = "nx-azure-cache"\r?\nversion = ")([^"]*)(")/;

const read = (f) => fs.readFileSync(path.join(root, f), 'utf8');
const write = (f, s) => fs.writeFileSync(path.join(root, f), s);
const fail = (msg) => {
  console.error(msg);
  process.exit(1);
};

/** Every version the files hold, as `[where, version]`. */
function versions() {
  const found = [];
  for (const f of ['Cargo.toml', 'Cargo.lock']) found.push([f, (read(f).match(CRATE) || [])[2]]);
  for (const f of PACKAGES) {
    const p = JSON.parse(read(f));
    found.push([f, p.version]);
    for (const [dep, v] of Object.entries(p.optionalDependencies || {})) found.push([`${f} ${dep}`, v]);
  }
  return found;
}

const [arg, tag] = process.argv.slice(2);
if (arg === '--check') {
  const expected = (read('Cargo.toml').match(CRATE) || [])[2];
  if (tag !== `v${expected}`) fail(`tag ${tag} does not match Cargo.toml version ${expected}`);
  const wrong = versions().filter(([, v]) => v !== expected);
  if (wrong.length) fail(wrong.map(([f, v]) => `${f}: ${v}, expected ${expected}`).join('\n'));
  console.log(`version ${expected} consistent`);
} else {
  if (!/^\d+\.\d+\.\d+$/.test(arg || '')) fail('usage: node scripts/version.js x.y.z | --check vx.y.z');
  for (const f of ['Cargo.toml', 'Cargo.lock']) {
    const s = read(f);
    if (!CRATE.test(s)) fail(`${f}: nx-azure-cache version not found`);
    write(f, s.replace(CRATE, `$1${arg}$3`));
  }
  for (const f of PACKAGES) {
    const s = read(f);
    const p = JSON.parse(s);
    p.version = arg;
    for (const dep of Object.keys(p.optionalDependencies || {})) p.optionalDependencies[dep] = arg;
    write(f, JSON.stringify(p, null, 2) + (s.includes('\r\n') ? '\r\n' : '\n'));
  }
  console.log(`version ${arg} written`);
}
