// Offline frozen-function + fetch oracle; synthetic shares only, no network or secrets.
import assert from 'node:assert/strict';
import {readFile, writeFile, mkdir} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import vm from 'node:vm';
import {parse} from 'acorn';
import {generate} from 'astring';

const deploymentSha = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const epoch = 1700000000;
class MockDB {
  constructor(rows = [], fail = '') { this.rows = new Map(rows.map(row => [row.shortcode, {...row}])); this.fail = fail; this.operations = []; }
  prepare(sql) {
    const db = this;
    const kind = sql.trim().split(/\s+/)[0];
    if (!['SELECT', 'INSERT', 'UPDATE'].includes(kind) || !sql.includes('short_links')) throw Error('Unexpected synthetic SQL');
    return { args: [], bind(...args) { this.args = args; return this; },
      async first() {
        db.operations.push('SELECT');
        if (db.fail === 'SELECT') throw Error('synthetic select failure');
        const row = db.rows.get(this.args[0]);
        return row ? {...row} : null;
      },
      async run() {
        db.operations.push(kind);
        if (db.fail === kind) throw Error('synthetic write failure');
        const code = this.args[0];
        if (kind === 'INSERT') {
          if (db.rows.has(code)) throw Error('synthetic conflict');
          db.rows.set(code, {shortcode: code, original_url: this.args[1], created_at: epoch, access_count: 0, last_accessed: 0, expires_at: 0});
        } else if (kind === 'UPDATE') {
          const row = db.rows.get(code);
          row.access_count++; row.last_accessed = epoch;
        } else throw Error('Unexpected synthetic write');
        return {success: true};
      }
    };
  }
}

export async function generateShortlinksVectors() {
  const base = new URL('../.scratch/baseline/', import.meta.url);
  const raw = await readFile(new URL('_worker.js', base));
  assert.equal(createHash('sha256').update(raw).digest('hex'), deploymentSha);
  const readable = await readFile(new URL('readable.js', base), 'utf8');
  const ast = parse(readable, {ecmaVersion: 'latest', sourceType: 'module'});
  const names = ['generateShortCode', 'createShortLink', 'getShortLink'];
  const functions = names.map(name => {
    const declarations = ast.body.filter(n => n.type === 'FunctionDeclaration' && n.id.name === name);
    assert.equal(declarations.length, 1);
    return generate(declarations[0]);
  }).join('\n');
  const exports = ast.body.filter(n => n.type === 'ExportDefaultDeclaration');
  assert.equal(exports.length, 1);
  const fetchSource = generate(exports[0].declaration);
  const source = `${functions}\nconst worker = ${fetchSource};\n({generateShortCode, createShortLink, getShortLink, worker});`;
  let random = () => 0;
  class FixedDate extends Date { static now() { return epoch * 1000; } }
  const math = Object.create(Math); math.random = () => random();
  const context = vm.createContext({URL, Request, Response, Date: FixedDate, Math: math,
    console: {log() {}}, d1Init: async () => {},
    handleMainRequest: async request => ({main: true, url: request.url, method: request.method})});
  const api = vm.runInContext(source, context, {timeout: 3000});
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  const generator = [];
  for (let index = 0; index < 62; index++) {
    random = () => (index + 0.25) / 62;
    generator.push({index, code: api.generateShortCode()});
    assert.equal(generator.at(-1).code, alphabet[index].repeat(6));
  }
  random = () => 0;
  const synthetic = 'https://parser.example.invalid/?url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic';
  const create = [];
  for (const fail of ['', 'SELECT', 'INSERT']) {
    const db = new MockDB([], fail);
    let code = null, failed = false;
    try { code = await api.createShortLink(db, synthetic); } catch { failed = true; }
    create.push({name: fail || 'success', code, failed, operations: db.operations});
  }
  const collisionDb = new MockDB([{shortcode: 'AAAAAA', original_url: synthetic}]);
  let failed = false;
  try { await api.createShortLink(collisionDb, synthetic); } catch { failed = true; }
  create.push({name: 'collision-limit', failed, operations: collisionDb.operations});
  assert.equal(collisionDb.operations.length, 10);
  let calls = 0;
  random = () => (++calls > 54 ? 1.25 / 62 : 0);
  const retryDb = new MockDB([{shortcode: 'AAAAAA', original_url: synthetic}]);
  create.push({name: 'last-attempt-success', code: await api.createShortLink(retryDb, synthetic), operations: retryDb.operations});
  random = () => 0;
  const expiry = [];
  for (const expires_at of [0, null, -1, epoch - 1, epoch, epoch + 1]) {
    const db = new MockDB([{shortcode: 'AAAAAA', original_url: synthetic, access_count: 3, last_accessed: 0, expires_at}]);
    const result = await api.getShortLink(db, 'AAAAAA');
    expiry.push({expires_at, present: result !== null, returned_count: result.access_count,
      stored_count: db.rows.get('AAAAAA').access_count, last_accessed: db.rows.get('AAAAAA').last_accessed});
  }
  const errors = [];
  for (const fail of ['SELECT', 'UPDATE']) {
    const db = new MockDB([{shortcode: 'AAAAAA', original_url: synthetic, access_count: 3}], fail);
    errors.push({fail, returns_null: await api.getShortLink(db, 'AAAAAA') === null});
  }
  const shares = [
    'https://cloud.189.cn/t/SyntheticA', 'http://h5.cloud.189.cn/t/SyntheticB',
    'https://m.cloud.189.cn/t/SyntheticC', 'https://cloud.189.cn/web/share?code=SyntheticD',
    'https://cloud.189.cn/s/Synthetic_E', 'https://yun.139.com/shareweb/#/w/i/SyntheticF',
    'http://caiyun.139.com/shareweb/?linkID=SyntheticG', 'https://yun.139.com/link/?linkid=SyntheticH'
  ];
  const vectors = [];
  for (const share of shares) for (const type of ['', 'json', 'down', 'text']) for (const method of ['GET', 'POST', 'HEAD']) {
    const url = new URL('https://parser.example.invalid/');
    url.searchParams.set('url', share);
    url.searchParams.set('pwd', '合成 +&密码');
    if (type) url.searchParams.set('type', type);
    url.searchParams.set('id', 'fileSynthetic'); url.searchParams.set('d', 'ignoredAlias');
    url.searchParams.set('fid', 'root'); url.searchParams.set('fid2', 'root');
    url.searchParams.set('fid5', 'child'); url.searchParams.set('fid20', 'leaf');
    const db = new MockDB([{shortcode: 'AAAAAA', original_url: url.href, access_count: 0}]);
    const result = await api.worker.fetch(new Request('https://parser.example.invalid/s/AAAAAA', {method}), {jxpan: db}, {});
    const target = new URL(result.url);
    vectors.push({original_url: url.href, method, expected: target.pathname + target.search,
      count: db.rows.get('AAAAAA').access_count});
  }
  const routeFacts = [];
  for (const [path, method, original, incoming] of [
    ['/api/create-short', 'POST', synthetic, ''], ['/shorten', 'POST', synthetic, ''],
    ['/s/AAAAAA/extra', 'HEAD', synthetic, ''], ['/s/missing', 'GET', synthetic, ''],
    ['/s/AAAAAA', 'GET', synthetic + '&sid=SyntheticSID', ''],
    ['/s/AAAAAA', 'GET', synthetic, '?action=synthetic_unsafe&auth=synthetic_unsafe']
  ]) {
    const db = new MockDB([{shortcode: 'AAAAAA', original_url: original, access_count: 0}]);
    const request = new Request('https://parser.example.invalid' + path + incoming, {method,
      ...(method === 'POST' ? {body: JSON.stringify({url: original}), headers: {'Content-Type': 'application/json'}} : {})});
    const result = await api.worker.fetch(request, {jxpan: db}, {});
    routeFacts.push({path, method, ...(result instanceof Response ? {status: result.status, body: await result.text()} : result)});
  }
  return {source_sha256: deploymentSha, function_sha256: createHash('sha256').update(source).digest('hex'),
    provenance: 'acorn/astring frozen declarations and complete fetch in network-free VM; MockDB + synthetic inputs',
    generator, create, expiry, errors, vectors, route_facts: routeFacts};
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const result = await generateShortlinksVectors();
  if (process.argv.includes('--write')) {
    const dir = new URL('../tests/fixtures/', import.meta.url);
    await mkdir(dir, {recursive: true});
    await writeFile(new URL('shortlinks.json', dir), JSON.stringify(result, null, 2) + '\n');
  }
  process.stdout.write(JSON.stringify(process.argv.includes('--summary')
    ? {source_sha256: result.source_sha256, vectors: result.vectors.length, expiry: result.expiry.length, create: result.create.length}
    : result) + '\n');
}
