// Offline-only differential oracle. Extract exact frozen AES/KV functions into
// a VM with no fetch, process, require, filesystem, or credential access.
import {readFile, writeFile, mkdir} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import vm from 'node:vm';
import {parse} from 'acorn';

const base = new URL('../.scratch/baseline/', import.meta.url);
const deploymentSha = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const epoch = 1700000000000;

export async function generateStorageVectors() {
  const raw = await readFile(new URL('_worker.js', base));
  if (createHash('sha256').update(raw).digest('hex') !== deploymentSha) {
    throw Error('Frozen worker SHA mismatch');
  }
  const readable = await readFile(new URL('readable.js', base), 'utf8');
  const ast = parse(readable, {ecmaVersion: 'latest', sourceType: 'module'});
  const names = ['__p_G0UC_dummyFunction', 'AES128ECB', 'encryptForKV', 'decryptFromKV', 'd1Get'];
  const extracted = names.map(name => {
    const nodes = ast.body.filter(n => ['FunctionDeclaration', 'ClassDeclaration'].includes(n.type) && n.id?.name === name);
    if (nodes.length !== 1) throw Error('Frozen declaration missing or ambiguous');
    return readable.slice(nodes[0].start, nodes[0].end);
  });
  // Retain the exact constant declaration too, not a rewritten AES implementation.
  const key = ast.body.filter(n => n.type === 'VariableDeclaration' && n.declarations.some(d => d.id.name === 'KV_ENCRYPTION_KEY'));
  if (key.length !== 1) throw Error('Frozen protocol constant missing');
  const source = extracted.join('\n') + '\n' + readable.slice(key[0].start, key[0].end);
  class FixedDate extends Date { static now() { return epoch; } }
  const context = vm.createContext({TextEncoder, TextDecoder, Date: FixedDate, console: {log() {}, error() {}}});
  const api = vm.runInContext(source + '\n({encryptForKV, decryptFromKV, d1Get});', context, {timeout: 3000});
  const corpus = [
    {name: 'empty-object', value: {}},
    {name: 'empty-array', value: []},
    {name: 'nested-unicode', value: {title: '离线合成 🧪', nested: {yes: true, no: false, none: null}, list: [1, '二', {}]}},
    {name: 'insertion-order', value: {z: 1, a: 2, nested: {b: 3, a: 4}}},
    {name: 'js-integer-keys', value: {'10': 'ten', '2': 'two', '01': 'zero-one', z: 'last'}},
    {name: 'null', value: null},
    {name: 'boolean', value: true},
    {name: 'integer', value: 123456},
    {name: 'float', value: 12.25},
    {name: 'empty-string', value: ''},
    {name: 'escape-string', value: 'line\nquote"slash\\tab\tzero\u0000中文'},
    ...[13, 14, 15, 16, 17, 30, 31, 32, 33, 255].map(length => ({name: 'padding-' + length, value: 'x'.repeat(length)})),
  ];
  const vectors = corpus.map(({name, value}) => {
    const json = JSON.stringify(value);
    const ciphertext = api.encryptForKV(value);
    if (typeof ciphertext !== 'string' || !/^(?:[0-9a-f]{32})+$/.test(ciphertext)) {
      throw Error('Frozen encryption failed');
    }
    const decoded = api.decryptFromKV(ciphertext);
    if (JSON.stringify(decoded) !== json) throw Error('Frozen round trip failed');
    if (JSON.stringify(api.decryptFromKV(ciphertext.toUpperCase())) !== json) throw Error('Uppercase hex failed');
    return {name, value, json, ciphertext};
  });
  const expiry = [];
  for (const expires_at of [0, null, -1, epoch / 1000 - 1, epoch / 1000, epoch / 1000 + 1]) {
    let deleted = false;
    const db = {prepare(sql) {return {bind() {return this;}, async first() {return {value: 'offline', expires_at};}, async run() {
      if (!sql.startsWith('DELETE')) throw Error('Unexpected offline SQL');
      deleted = true;
      return {success: true};
    }};}};
    const result = await api.d1Get(db, 'synthetic-key');
    expiry.push({now: epoch / 1000, expires_at, present: result !== null, deleted});
  }
  return {
    source_sha256: deploymentSha,
    function_sha256: createHash('sha256').update(source).digest('hex'),
    provenance: 'Exact frozen readable.js AES128ECB/encryptForKV/decryptFromKV/d1Get; synthetic offline inputs only',
    vectors, expiry,
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const result = await generateStorageVectors();
  if (process.argv.includes('--write')) {
    const dir = new URL('../tests/fixtures/', import.meta.url);
    await mkdir(dir, {recursive: true});
    await writeFile(new URL('storage.json', dir), JSON.stringify(result, null, 2) + '\n');
  }
  const output = process.argv.includes('--summary')
    ? {source_sha256: result.source_sha256, vectors: result.vectors.length, expiry: result.expiry.length}
    : result;
  process.stdout.write(JSON.stringify(output) + '\n');
}
