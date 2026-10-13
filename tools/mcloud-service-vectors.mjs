// Offline business-orchestration differential oracle. No real account or HTTP.
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import { parse } from 'acorn';

const baseline = new URL('../.scratch/baseline/', import.meta.url);
const deploymentHash = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const hash = source => createHash('sha256').update(source).digest('hex');
async function originals() {
  assert.equal(hash(await readFile(new URL('_worker.js', baseline))), deploymentHash, 'deployment drift');
  const readable = await readFile(new URL('readable.js', baseline), 'utf8');
  const classSource = (await readFile(new URL('MobileCloudParser.js', baseline), 'utf8')).trim();
  const wanted = new Set(['MobileCloudParser', 'formatFileSize', 'jxpanWalkDirTree', 'jxpanBuildNestedTree']);
  const found = new Map();
  const pending = [parse(readable, { ecmaVersion: 'latest', sourceType: 'module' })];
  while (pending.length) {
    const node = pending.pop();
    if (['FunctionDeclaration', 'ClassDeclaration'].includes(node.type) && wanted.has(node.id?.name)) {
      assert.ok(!found.has(node.id.name), 'ambiguous source');
      found.set(node.id.name, readable.slice(node.start, node.end));
    }
    for (const value of Object.values(node)) {
      if (Array.isArray(value)) for (const child of value) { if (child?.type) pending.push(child); }
      else if (value?.type) pending.push(value);
    }
  }
  assert.equal(found.size, wanted.size, 'missing original source');
  assert.equal(found.get('MobileCloudParser'), classSource, 'class extraction differs');
  const source = [...wanted].map(name => found.get(name)).join('\n');
  return { source, classHash: hash(classSource), functionsHash: hash(source) };
}
function corpus() {
  const cases = [];
  const file = (id, name = id, size = 1024, extra = {}) => ({ coID: id, coName: name, coSize: size, ...extra });
  const folder = (id, name = id, extra = {}) => ({ coID: id, coName: name, coType: 1, ...extra });
  const add = (name, graph = {}, extra = {}) => cases.push({ name, graph, action: 'parse', enabled: true, authorizationConfigured: true, shareUrl: 'https://yun.139.com/shareweb/#/w/i/Offline123', chain: [], target: '', ...extra });
  add('disabled-before-auth-and-link', {}, { enabled: false, authorizationConfigured: false, shareUrl: 'bad' });
  add('authorization-missing-before-link', {}, { authorizationConfigured: false, shareUrl: 'bad' });
  add('invalid-link', {}, { shareUrl: 'bad' });
  add('empty-share');
  add('failed-authentication-is-empty', {}, { failures: ['root'] });
  add('single-file-never-download', { root: [file('f', '电影😀.mkv', '1024bytes', { path: '/real', coPath: '/other' })] });
  add('single-file-missing-fields', { root: [{}] });
  add('single-file-field-priority', { root: [{ coID: 'co', contentID: 'content', id: 'id', fileId: 'file', coName: 'coName', contentName: 'contentName', name: 'name', fileName: 'fileName', coSize: 1, contentSize: 2, size: 3, fileSize: 4 }] });
  add('multi-file-tree', { root: [file('a'), file('b')] });
  const multi = { root: [folder('a', '重复'), folder('b', '重复'), file('f', '重复')], a: [folder('deep', '深层'), file('x')], deep: [file('movie', '片段.mkv', 1536, { path: '/actual/movie' })], b: [] };
  add('multilevel-empty-duplicate-names', multi);
  add('fid-last-item-only', multi, { chain: ['ignored-nonexistent', 'deep'] });
  add('fid-missing-keeps-requested-tree', multi, { chain: ['missing'] });
  add('fid-empty-string-is-root', multi, { chain: [''] });
  add('fileID-DFS-path', multi, { target: 'movie' });
  add('fileID-wins-over-fid', multi, { chain: ['missing'], target: 'movie' });
  add('fileID-is-folder', multi, { target: 'a' });
  add('fileID-missing', multi, { target: 'nope' });
  add('fileID-no-path-no-ID-download-fallback', { root: [file('f')] }, { target: 'f' });
  add('fileID-path-before-coPath', { root: [file('f', 'f', 1, { path: '/primary', coPath: '/secondary' })] }, { target: 'f' });
  add('fileID-coPath', { root: [file('f', '', 1, { coPath: '/cop' })] }, { target: 'f' });
  add('fileID-download-null', { root: [file('f', 'f', 1, { path: '/null' })] }, { target: 'f', downloadNull: true });
  add('fileID-download-error', { root: [file('f', 'f', 1, { path: '/fail' })] }, { target: 'f', downloadFailure: true });
  add('fileID-numeric-coercion', { root: [file(42)] }, { target: '42' });
  add('fileID-DFS-first-match-not-BFS', { root: [folder('a'), folder('b')], a: [folder('deep')], deep: [file('same', 'first')], b: [file('same', 'second')] }, { target: 'same' });
  add('fileID-visited-cycle', { root: [folder('a')], a: [folder('root'), folder('b')], b: [file('f')] }, { target: 'missing' });
  add('tree-child-failure-is-empty-node', { root: [folder('a'), folder('b')], b: [file('f')] }, { failures: ['a'] });
  add('fileID-child-failure-continues', { root: [folder('a'), folder('b')], b: [file('f')] }, { target: 'f', failures: ['a'] });
  const depth = { root: [folder('d1')] };
  for (let i = 1; i < 10; i++) depth['d' + i] = [file('at' + i), folder('d' + (i + 1))];
  add('fileID-depth-eight-included', depth, { target: 'at8' });
  add('fileID-depth-nine-excluded', depth, { target: 'at9' });
  for (const size of [0, 1, 1023, 1024, 1152, 1280, 1408, 1536, 1048576, -1, '12.9suffix', '0x10', 'invalid', '  +2048foo', 1e21, 1e20, 1e-6, 1e-7, '1099511627776', '9'.repeat(400)]) {
    add('filesize-' + JSON.stringify(size), { root: [file('f', 'f', size)] });
  }
  const classifier = { root: [folder('a'), file('b', 'resType-folder', 1, { resType: 1 }), file('c', 'type-folder', 2, { type: 'folder' }), file('d', 'coType-string', 3, { coType: '1' })] };
  add('shared-walker-classifier-not-parser', classifier);
  const wide = { root: Array.from({ length: 420 }, (_, i) => folder('d' + i)) };
  add('tree-four-concurrency-400-nodes', wide);
  const chainGraph = { root: [folder('a')], a: [folder('deep'), file('first', 'First')], deep: [folder('empty'), file('f', '影片', 2048, { path: '/chain/file' }), { contentID: 'c', contentName: 'Content', contentSize: 4 }], empty: [] };
  const chain = (name, chain, target = '', graph = chainGraph) => add(name, graph, { action: 'fidChain', chain, target });
  chain('chain-full-list', ['a', 'deep']);
  chain('chain-single-level-parent-root', ['a']);
  chain('chain-empty-means-root-list', []);
  chain('chain-empty-root', [], '', {});
  chain('chain-first-folder-failure', ['bad', 'deep']);
  chain('chain-second-folder-empty', ['a', 'empty']);
  chain('chain-file', ['a', 'deep'], 'f');
  chain('chain-file-folder-error', ['a', 'deep'], 'empty');
  chain('chain-file-missing-available-undefined-omitted', ['a', 'deep'], 'absent');
  chain('chain-no-fid-with-file', [], 'f');
  // Seeded acyclic graphs exercise list ordering, empty nodes, duplicate names and aliases.
  let seed = 139;
  const random = n => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % n; };
  for (let sample = 0; sample < 32; sample++) {
    const graph = {};
    for (let parent = 0; parent < 8; parent++) {
      const rows = [];
      for (let j = 0, count = random(5); j < count; j++) {
        if (parent < 7 && random(2)) rows.push(folder('d' + (parent + 1 + random(7 - parent)), ['same', '目录', ''][random(3)]));
        else rows.push(file('f' + random(6), ['same', 'movie', ''][random(3)], [0, 100, '123bytes'][random(3)]));
      }
      graph[parent === 0 ? 'root' : 'd' + parent] = rows;
    }
    add('seeded-tree-' + sample, graph);
    add('seeded-fileID-' + sample, graph, { target: 'f2' });
  }
  return cases;
}
async function execute(source, input, seedRoot = false) {
  const calls = [], downloadCalls = [];
  let active = 0, peak = 0, deniedNetwork = 0;
  class FixedDate extends Date { static now() { return 1704164645000; } }
  const context = vm.createContext({ Date: FixedDate, atob, console: { log() {}, error() {} },
    fetch() { deniedNetwork++; throw new Error('strict offline VM: fetch forbidden'); },
    jxpanDirCacheGet() { return null; }, jxpanDirCacheSet() {} });
  const Parser = vm.runInContext(source + '\nMobileCloudParser', context, { timeout: 1000 });
  // This credential is entirely synthetic, decodes to type:offline|fake.
  const parser = new Parser({ mcloud: { enabled: input.enabled, authorization: input.authorizationConfigured ? 'Basic dHlwZTpvZmZsaW5lfGZha2U=' : '' } });
  let rootReads = 0;
  parser.getShareFiles = async (_link, _pwd, id = 'root') => {
    if (seedRoot && id === 'root' && ++rootReads === 2) return structuredClone(input.graph.root || []);
    calls.push(id); active++; peak = Math.max(peak, active);
    for (let i = 0; i < 1 + [...id].reduce((sum, c) => sum + c.codePointAt(0), 0) % 4; i++) await Promise.resolve();
    active--;
    if (input.failures?.includes(id)) return [];
    return structuredClone(input.graph[id] || []);
  };
  parser.getDownloadUrl = async (_link, path) => {
    downloadCalls.push(path);
    if (input.downloadFailure || input.downloadNull) return null;
    return 'https://download.invalid/offline/' + encodeURIComponent(path);
  };
  const output = input.action === 'fidChain'
    ? await parser.parseWithFidChain('Offline123', '', input.chain, input.target)
    : await parser.parse(input.shareUrl, '', { fidChain: input.chain, fileId: input.target });
  assert.equal(deniedNetwork, 0);
  return { output: JSON.parse(JSON.stringify(output)), calls, downloadCalls, peak };
}
export async function generateVectors() {
  const { source, classHash, functionsHash } = await originals();
  const vectors = [];
  let savedRootRequests = 0;
  for (const input of corpus()) {
    const expected = await execute(source, input);
    const canSeed = input.action === 'parse' && input.chain.length === 0 && !input.target && expected.calls[0] === 'root' && expected.calls[1] === 'root';
    let optimized = expected;
    if (canSeed) {
      optimized = await execute(source, input, true);
      assert.deepEqual(optimized.output, expected.output, 'root seed must preserve complete JSON');
      assert.equal(expected.calls.length - optimized.calls.length, 1);
      savedRootRequests++;
    }
    vectors.push({ input, expected, optimizedCalls: optimized.calls });
  }
  return { sourceHash: deploymentHash, classHash, functionsHash, savedRootRequests, vectors };
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const output = await generateVectors();
    process.stdout.write(JSON.stringify(process.argv.includes('--summary') ? { ...output, vectors: output.vectors.length } : output) + '\n');
  } catch (error) { console.error(error.stack); process.exitCode = 1; }
}
