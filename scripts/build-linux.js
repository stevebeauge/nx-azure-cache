// Static Linux binary (musl) built in Docker.
// The rust:1.95-alpine host is x86_64-unknown-linux-musl: static without --target.
'use strict';
const { execFileSync } = require('node:child_process');
const { mkdirSync } = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const dist = path.join(root, 'dist');
const bin = 'nx-azure-cache-linux-x64';
mkdirSync(dist, { recursive: true });
const docker = (...args) => execFileSync('docker', args, { stdio: 'inherit' });

// Registry and target in named volumes: fast rebuilds, and no mixing with the Windows
// target/.
docker('run', '--rm',
  '-v', 'nx-azure-cache-cargo-registry:/usr/local/cargo/registry',
  '-v', 'nx-azure-cache-target-linux:/target',
  '-v', `${root}:/src`, '-w', '/src', '-e', 'CARGO_TARGET_DIR=/target',
  'rust:1.95-alpine',
  'sh', '-c', `cargo build --release --locked && cp /target/release/nx-azure-cache dist/${bin}`);

// Check: static according to ldd (glibc; Alpine's is misleading on a static-pie),
// and `version` runs in two bare images.
for (const img of ['debian:bookworm-slim', 'ubuntu:24.04']) {
  docker('run', '--rm', '-v', `${dist}:/b:ro`, img,
    'sh', '-c', `ldd /b/${bin} 2>&1 | grep -q "statically linked" && /b/${bin} version`);
}
console.log(`dist/${bin}`);
