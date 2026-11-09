#!/usr/bin/env node
// `nx-azure-cache` command: runs the Gateway binary of the Platform package with the Workspace's
// settings (ADR 0005). The options of the `nx-azure-cache` entry of the nearest `nx.json` become
// `NX_AZURE_CACHE_*` variables, unless already set. Arguments, stdio and exit code pass through.
'use strict';

const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { binaryPath, SETTINGS } = require('./index.cjs');

/** Options of the plugin entry in the nearest `nx.json`; `{}` if none or unreadable. */
function nxOptions() {
  for (let dir = process.cwd(); ; ) {
    const file = path.join(dir, 'nx.json');
    if (fs.existsSync(file)) {
      try {
        const entry = (JSON.parse(fs.readFileSync(file, 'utf8')).plugins || []).find(
          (p) => p && p.plugin === 'nx-azure-cache',
        );
        return (entry && entry.options) || {};
      } catch {
        return {};
      }
    }
    const parent = path.dirname(dir);
    if (parent === dir) return {};
    dir = parent;
  }
}

const env = { ...process.env };
const options = nxOptions();
for (const [option, variable] of Object.entries(SETTINGS)) {
  if (!env[variable] && options[option]) env[variable] = String(options[option]);
}

const bin = binaryPath(env);
if (!bin) {
  console.error(`nx-azure-cache: no Gateway binary for ${process.platform}-${process.arch}`);
  process.exit(1);
}
const r = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit', env });
if (r.error) {
  console.error(`nx-azure-cache: ${r.error.message}`);
  process.exit(1);
}
process.exit(r.status ?? 1);
