import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import { parse } from 'acorn';
import { generate } from 'astring';

export const privateDir = fileURLToPath(new URL('../.scratch/baseline/', import.meta.url));
const hash = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';

export function originalCipher() {
  const worker = readFileSync(`${privateDir}_worker.js`);
  assert.equal(createHash('sha256').update(worker).digest('hex'), hash);
  const ast = parse(readFileSync(`${privateDir}readable.js`, 'utf8'), {
    ecmaVersion: 'latest', sourceType: 'module',
  });
  const cipher = ast.body.find(item => item.id?.name === 'AES128ECB');
  const guard = ast.body.find(item => item.id?.name === '__p_G0UC_dummyFunction');
  assert(cipher && guard, 'pinned KV cipher or guard missing');
  const Cipher = vm.runInNewContext(`${generate(guard)}; (${generate(cipher)})`, {
    TextEncoder, TextDecoder, Uint8Array, Buffer, console: { log() {} },
  }, { timeout: 1000 });
  return new Cipher('LsPro-JxPan');
}

export function readonlyRows(target, sql) {
  assert(/^[A-Za-z0-9_.-]+@[A-Za-z0-9.:-]+$/.test(target), 'explicit authorized SSH target required');
  assert(/^SELECT\b/i.test(sql) && !/[;"\n\r$`\\]/.test(sql), 'single read-only SELECT required');
  const command = `docker exec -e WRANGLER_LOG=log jxpan ./node_modules/.bin/wrangler d1 execute jxpan --local --persist-to=/data --json --command "${sql}"`;
  const result = spawnSync('ssh', ['-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes',
    '-o', 'ConnectTimeout=10', target, command], {
    encoding: 'utf8', timeout: 45_000, maxBuffer: 2 * 1024 * 1024,
  });
  assert(result.status === 0, 'read-only baseline query failed; response values not emitted');
  const start = result.stdout.indexOf('[');
  assert(start >= 0, 'baseline JSON output missing; response values not emitted');
  let tables;
  try { tables = JSON.parse(result.stdout.slice(start)); }
  catch { throw new Error('baseline returned invalid JSON; values not emitted'); }
  return tables.flatMap(table => table.results || []);
}

export function decodeRow(cipher, row) {
  try { return JSON.parse(row.value); }
  catch {
    try { return JSON.parse(cipher.decryptHex(row.value)); }
    catch { throw new Error('baseline row decode failed; values not emitted'); }
  }
}
