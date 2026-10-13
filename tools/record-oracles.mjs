import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = fileURLToPath(new URL('../', import.meta.url));
const target = resolve(root, 'tests/fixtures');
mkdirSync(target, { recursive: true });
const manifest = {};
for (const provider of ['cloud189', 'mcloud', 'cloud189-http', 'mcloud-http', 'tree', 'mcloud-service', 'cloud189-service', 'mcloud-auth', 'cloud189-auth', 'storage', 'params']) {
  const result = spawnSync(process.execPath, [`tools/${provider}-vectors.mjs`], {
    cwd: root, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, timeout: 30_000,
  });
  assert.equal(result.status, 0, result.stderr);
  JSON.parse(result.stdout);
  writeFileSync(resolve(target, `${provider}.json`), result.stdout);
  manifest[provider] = {
    sha256: createHash('sha256').update(result.stdout).digest('hex'),
    bytes: Buffer.byteLength(result.stdout),
    provenance: 'synthetic vectors evaluated by pinned current-deployment original methods; no network or credentials',
  };
}
writeFileSync(resolve(target, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(JSON.stringify(manifest, null, 2));
