import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import { parse } from 'acorn';
import { generate } from 'astring';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const baseline = resolve(root, '.scratch/baseline');
const source = readFileSync(resolve(baseline, '_worker.js'), 'utf8');
const manifest = JSON.parse(readFileSync(resolve(baseline, 'manifest.json'), 'utf8'));
const sha256 = value => createHash('sha256').update(value).digest('hex');
assert.equal(sha256(source), manifest.source.sha256, 'frozen source changed');
const ast = parse(source, { ecmaVersion: 'latest', sourceType: 'module' });
const pending = [ast];
while (pending.length) {
  const node = pending.pop();
  if (node.type === 'Literal' && typeof node.value === 'string') delete node.raw;
  for (const value of Object.values(node)) {
    if (Array.isArray(value)) pending.push(...value.filter(item => item && typeof item === 'object'));
    else if (value && typeof value === 'object') pending.push(value);
  }
}
writeFileSync(resolve(baseline, 'readable.js'), generate(ast));
for (const name of ['index', 'admin', 'Cloud189Parser', 'MobileCloudParser', 'handleMainRequest']) {
  const node = ast.body.find(item => item.id?.name === name);
  assert(node, `missing baseline declaration: ${name}`);
  writeFileSync(resolve(baseline, `${name}.js`), generate(node));
}
if (process.argv.includes('--import')) {
  mkdirSync(resolve(root, 'web'), { recursive: true });
  for (const [name, entry] of Object.entries(manifest.pages)) {
    const page = readFileSync(resolve(baseline, `${name}.html`));
    assert.equal(sha256(page), entry.sha256);
    writeFileSync(resolve(root, 'web', `${name}.html`), page);
  }
  // Evaluate only the isolated page renderer, without network, storage or Worker code.
  const renderer = readFileSync(resolve(baseline, 'admin.js'), 'utf8');
  const admin = vm.runInNewContext(`(${renderer})`, {}, { timeout: 1000 });
  const rendered = admin(false);
  assert.equal(sha256(rendered), manifest.pages['admin-login'].sha256);
  writeFileSync(resolve(root, 'web/admin.html'), admin(true));
  const attribution = {
    source_sha256: manifest.source.sha256,
    pages: { ...manifest.pages, admin: { bytes: Buffer.byteLength(admin(true)), sha256: sha256(admin(true)), provenance: 'isolated deployed admin(true) renderer; no credentials or session data' } },
  };
  writeFileSync(resolve(root, 'web/baseline.json'), JSON.stringify(attribution, null, 2) + '\n');
}
console.log(`Decoded deployed source; SHA-256=${manifest.source.sha256}`);
