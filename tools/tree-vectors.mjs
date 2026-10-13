// Offline differential oracle: exact current functions, extracted by AST offsets.
// The VM has no fetch, network, credentials, storage or timers. Lists are memory graphs.
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import { parse } from 'acorn';

const baseline = new URL('../.scratch/baseline/', import.meta.url);
const deploymentHash = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
export const epoch = 1704164645000;

async function originalFunctions() {
  const raw = await readFile(new URL('_worker.js', baseline));
  const sourceHash = createHash('sha256').update(raw).digest('hex');
  if (sourceHash !== deploymentHash) throw new Error('Deployed worker SHA-256 mismatch');
  const readable = await readFile(new URL('readable.js', baseline), 'utf8');
  const ast = parse(readable, { ecmaVersion: 'latest', sourceType: 'module', locations: true });
  const wanted = new Map([['jxpanWalkDirTree', 14889], ['jxpanBuildNestedTree', 14985]]);
  const found = new Map();
  const pending = [ast];
  while (pending.length) {
    const node = pending.pop();
    if (node.type === 'FunctionDeclaration' && wanted.has(node.id?.name)) {
      if (found.has(node.id.name)) throw new Error('Ambiguous original function');
      if (node.loc.start.line !== wanted.get(node.id.name)) throw new Error('Original source line moved');
      found.set(node.id.name, readable.slice(node.start, node.end));
    }
    for (const child of Object.values(node)) {
      if (Array.isArray(child)) {
        for (const entry of child) if (entry && typeof entry.type === 'string') pending.push(entry);
      } else if (child && typeof child.type === 'string') pending.push(child);
    }
  }
  if (found.size !== wanted.size) throw new Error('Original functions missing');
  const source = [...wanted.keys()].map(name => found.get(name)).join('\n');
  class FixedDate extends Date { static now() { return epoch; } }
  const context = vm.createContext({ Date: FixedDate });
  const functions = vm.runInContext(source + '\n({walk: jxpanWalkDirTree, nested: jxpanBuildNestedTree})', context, { timeout: 1000 });
  return { functions, sourceHash, functionHash: createHash('sha256').update(source).digest('hex') };
}

function corpus() {
  const cases = [];
  const add = (name, graph = {}, extra = {}) => cases.push({ name, rootId: 'r', rootName: 'Share', concurrency: 3, maxNodes: 400, graph, ...extra });
  const dir = (id, name = id, rest = {}) => ({ fileId: id, fileName: name, isFolder: true, ...rest });
  const file = (id, name = id, size = 1, rest = {}) => ({ fileId: id, fileName: name, fileSize: size, ...rest });
  add('empty');
  add('empty-root-id-and-name', { '': [file('f')] }, { rootId: '', rootName: '' });
  add('empty-root-name-json-path-types', { r: [dir('array', []), dir('number', 123), dir('object', {}), dir('bool', true), file('f', ['a', null])], array: [file('z')], number: [file('z')], object: [file('z')], bool: [file('z')] }, { rootName: '' });
  add('unicode-and-slashes', { r: [dir('d', '目录/😀'), file('f', '空 格.mkv', '123')], d: [file('z', '片段/中.mp4')] }, { rootName: '共享/根' });
  add('root-list-failure', {}, { failures: ['r'] });
  add('child-list-failure', { r: [dir('bad'), dir('good')], good: [file('f')] }, { failures: ['bad'] });
  for (const value of [null, false, 0, '', {}, { data: [] }]) add('non-array-list-' + JSON.stringify(value), { r: value });
  add('breadth-first-and-response-order', { r: [dir('a'), dir('b'), dir('c')], a: [dir('aa'), file('a1')], b: [dir('bb'), file('b1')], c: [file('c1')], aa: [file('a2')], bb: [file('b2')] });
  add('full-field-priority', { r: [{ fileId: 'first', id: 'second', coID: 'third', contentID: 'fourth', fileName: 'n1', name: 'n2', coName: 'n3', contentName: 'n4', fileSize: 7, size: 8, coSize: 9, contentSize: 10 }] });
  for (const falsy of [null, false, 0, '']) {
    add('falsy-field-fallback-' + JSON.stringify(falsy), { r: [{ fileId: falsy, id: 'id', fileName: falsy, name: 'fallback', fileSize: falsy, size: 23 }, { fileId: falsy, id: falsy, coID: 'co', fileName: falsy, name: falsy, coName: 'co-name', fileSize: falsy, size: falsy, coSize: 24 }, { fileId: falsy, id: falsy, coID: falsy, contentID: 'content', fileName: falsy, name: falsy, coName: falsy, contentName: 'content-name', fileSize: falsy, size: falsy, coSize: falsy, contentSize: 25 }] });
  }
  add('missing-ids-and-primitive-rows', { r: [null, false, 0, '', [], {}, 'text', 12, { fileId: 0 }, { fileId: [] }, file('valid')] });
  add('json-truthiness-and-coercion', { r: [file(true, 123, true), file(['a', null, ['b']], ['name', 'parts'], []), file({ x: 1 }, { x: 2 }, {}), file(1e21, 'big', -5), file(1e-7, 'tiny', '0'), file(1e-6, 'boundary', 0)] });
  add('walk-classifier-not-mobile-parser', { r: [dir('bool'), file('false', 'false', 1, { isFolder: false, isDir: false, coType: 0 }), file('string-zero', 'string-zero', 1, { isFolder: '0' }), file('string-false', 'string-false', 1, { isDir: 'false' }), file('array', 'array', 1, { isFolder: [] }), file('object', 'object', 1, { isDir: {} }), file('num', 'num', 1, { coType: 1 }), file('string-one', 'string-one', 1, { coType: '1' }), file('true-type', 'true-type', 1, { coType: true }), file('content-type', 'content-type', 1, { contentType: 1 }), file('type-folder', 'type-folder', 1, { type: 'folder' })] });
  add('folder-and-file-collisions', { r: [file('f', 'same'), dir('a', 'same'), dir('b', 'same'), file('g', 'same'), file('g', 'same'), file('h', 'same#g')], a: [file('inside')], b: [] });
  add('empty-name-falls-back-to-id', { r: [dir('d', ''), file('f', '', 0)], d: [file('inside', '')] });
  add('duplicate-folder-and-file-ids', { r: [dir('a', 'first'), dir('a', 'second'), dir('b'), file('f', 'first'), file('f', 'second')], a: [file('leaf')], b: [dir('a', 'shared')] });
  add('shared-subtree-not-global-dedup', { r: [dir('a'), dir('b')], a: [dir('shared')], b: [dir('shared')], shared: [file('leaf')] });
  add('numeric-object-key-order', { r: [dir('20', '20'), dir('3', '3'), dir('01', '01'), file('f10', '10'), file('f2', '2'), file('f0', '0'), file('fmax', '4294967295')] });
  add('safe-object-names', { r: [dir('constructor'), dir('toString'), file('f', 'hasOwnProperty'), file('g', 'prototype')] });
  for (const concurrency of [0, 1, 2, 6, 1000]) add('concurrency-' + concurrency, { r: Array.from({ length: 12 }, (_, i) => dir('d' + i)), ...Object.fromEntries(Array.from({ length: 12 }, (_, i) => ['d' + i, [file('f' + i)]])) }, { concurrency });
  for (const maxNodes of [0, 1, 2, 4, 7, 400]) add('maxNodes-' + maxNodes, { r: Array.from({ length: 16 }, (_, i) => dir('d' + i)), ...Object.fromEntries(Array.from({ length: 16 }, (_, i) => ['d' + i, [dir('next' + i), file('f' + i)]])) }, { maxNodes });
  add('duplicate-queue-consumes-admission-budget', { r: [dir('a'), dir('a'), dir('b'), dir('c')], a: [dir('leaf')], b: [file('b-file')] }, { maxNodes: 4, concurrency: 2 });
  add('start-subtree', { r: [dir('d')], d: [file('f')] }, { startId: 'd', startName: 'Subtree' });
  add('start-missing-node', { r: [file('f')] }, { startId: 'missing', startName: '' });
  add('default-400-wide', { r: Array.from({ length: 450 }, (_, i) => dir('d' + i)) });
  const deep = { r: [dir('d0')] };
  for (let i = 0; i < 24; i++) deep['d' + i] = i === 23 ? [file('leaf')] : [dir('d' + (i + 1))];
  add('normal-deep-chain', deep);
  // Reproducible acyclic graphs: arbitrary cross-parent aliases, duplicates,
  // names, truthy/falsy size aliases and truncation, without hand-made outputs.
  let seed = 0x189139;
  const random = n => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % n; };
  for (let sample = 0; sample < 96; sample++) {
    const graph = {};
    for (let parent = 0; parent < 12; parent++) {
      const rows = [];
      for (let j = 0, count = random(7); j < count; j++) {
        if (parent < 11 && random(2)) {
          const child = parent + 1 + random(11 - parent);
          rows.push({ coID: 'd' + child, coName: ['same', '', '目录', '2'][random(4)], coType: 1 });
        } else rows.push({ contentID: 'f' + random(10), contentName: ['same', '', '片段.mkv', '01'][random(4)], fileSize: [0, '', null][random(3)], contentSize: [0, 12, '123', -1][random(4)] });
      }
      graph[parent === 0 ? 'r' : 'd' + parent] = rows;
    }
    add('seeded-dag-' + sample, graph, { concurrency: [1, 2, 6][random(3)], maxNodes: [3, 7, 400][random(3)] });
  }
  return cases;
}

async function execute(functions, input, includeNested = true) {
  const calls = [];
  let active = 0, peak = 0;
  const walk = await functions.walk(async id => {
    calls.push(id);
    active++;
    peak = Math.max(peak, active);
    // Force batch overlap and completion order different from request order.
    const turns = 1 + [...id].reduce((sum, c) => sum + c.codePointAt(0), 0) % 4;
    for (let i = 0; i < turns; i++) await Promise.resolve();
    active--;
    if (input.failures?.includes(id)) throw new Error('offline list failure');
    return Object.hasOwn(input.graph, id) ? input.graph[id] : [];
  }, input.rootId, input.rootName, { concurrency: input.concurrency, maxNodes: input.maxNodes });
  const nested = includeNested ? functions.nested(walk, input.startId || '', input.startName || '') : null;
  // Include ordered property paths because object equality alone misses JS enumeration.
  const keyOrders = [];
  function keys(value, path) {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return;
    keyOrders.push({ path, keys: Object.keys(value) });
    for (const key of Object.keys(value)) keys(value[key], [...path, key]);
  }
  if (nested) keys(nested, []);
  return { walk, nested, calls, peak, nodeKeys: Object.keys(walk.nodes), keyOrders };
}

export async function generateVectors() {
  const { functions, sourceHash, functionHash } = await originalFunctions();
  const vectors = [];
  for (const input of corpus()) vectors.push({ input, expected: await execute(functions, input) });
  // Never invoke the unsafe original nested builder on cycles. These are explicitly
  // safety-difference cases; only walk/calls are obtained from the deployed function.
  const safetyInputs = [
    { name: 'self-cycle', graph: { r: [{ fileId: 'r', fileName: 'back', isFolder: true }] } },
    { name: 'two-node-cycle', graph: { r: [{ fileId: 'a', fileName: 'A', isFolder: true }], a: [{ fileId: 'r', fileName: 'back', isFolder: true }] } },
    { name: 'three-node-cycle', graph: { r: [{ fileId: 'a', fileName: 'A', isFolder: true }], a: [{ fileId: 'b', fileName: 'B', isFolder: true }], b: [{ fileId: 'a', fileName: 'back', isFolder: true }] } },
  ].map(input => ({ rootId: 'r', rootName: 'Share', concurrency: 3, maxNodes: 400, ...input }));
  const safetyVectors = [];
  for (const input of safetyInputs) safetyVectors.push({ input, expectedWalk: await execute(functions, input, false), originalNestedExecuted: false });
  return { sourceHash, functionHash, epoch, vectors, safetyVectors };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    const output = await generateVectors();
    if (process.argv.includes('--summary')) process.stdout.write(JSON.stringify({ sourceHash: output.sourceHash, functionHash: output.functionHash, normalVectors: output.vectors.length, cycleWalkVectors: output.safetyVectors.length, originalCycleNestedExecuted: false }) + '\n');
    else process.stdout.write(JSON.stringify(output) + '\n');
  } catch (error) { console.error(error.stack); process.exitCode = 1; }
}
