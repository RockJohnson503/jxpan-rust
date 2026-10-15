// Offline original-method oracle: frozen parse record shape/default TTL/classification.
// No network, credentials, or actual database; VM receives synthetic in-memory stubs.
import {readFile, writeFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import vm from 'node:vm';
import {parse} from 'acorn';

const baseline = new URL('../.scratch/baseline/', import.meta.url);
const sourceSha = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const epoch = 1700000000000;

export async function generateRecordsVectors() {
  const raw = await readFile(new URL('_worker.js', baseline));
  if (createHash('sha256').update(raw).digest('hex') !== sourceSha) throw Error('Frozen worker SHA mismatch');
  const readable = await readFile(new URL('readable.js', baseline), 'utf8');
  const ast = parse(readable, {ecmaVersion: 'latest', sourceType: 'module'});
  const names = ['getStatsFromKV', 'saveParseRecord', 'd1GetParseRecords'];
  const source = names.map(name => {
    const nodes = ast.body.filter(node => node.type === 'FunctionDeclaration' && node.id?.name === name);
    if (nodes.length !== 1) throw Error('Frozen function missing or ambiguous');
    return readable.slice(nodes[0].start, nodes[0].end);
  }).join('\n');
  class FixedDate extends Date {
    constructor(...args) { super(...(args.length ? args : [epoch])); }
    static now() { return epoch; }
  }
  let captured;
  const context = vm.createContext({
    Date: FixedDate, console: {log() {}, error() {}},
    encryptForKV: value => JSON.stringify(value),
    decryptFromKV: text => JSON.parse(text),
    storePut: async (_env, key, value, options) => { captured = {key, record: JSON.parse(value), options}; },
    incrementRecordsCache: async () => {},
    getRecordsStatsFromCache: async () => ({successTotal: 0, failedTotal: 0}),
  });
  const api = vm.runInContext(source + '\n({getStatsFromKV, saveParseRecord, d1GetParseRecords});', context, {timeout: 3000});
  const corpus = [
    {name: 'boolean-success', result: {success: true, code: 200, msg: '解析成功'}, new_success: true},
    {name: 'boolean-failure', result: {success: false, code: 403, msg: 'synthetic failure'}, new_success: false},
    {name: 'legacy-numeric-code', result: {code: 200}, new_success: true},
    {name: 'legacy-string-code', result: {code: 'success'}, new_success: true},
    {name: 'legacy-message', result: {msg: '解析成功'}, new_success: true},
    {name: 'explicit-false-with-code', result: {success: false, code: 200}, new_success: true},
    {name: 'explicit-true-without-code', result: {success: true}, new_success: false},
    {name: 'empty-result', result: {}, new_success: false},
    {name: 'complete-response', result: {success: true, code: 200, msg: '解析成功', data: {files: [{name: 'synthetic-file', url: 'https://example.invalid/download?share_pwd=synthetic-share'}], pwd: 'synthetic-share'}}, new_success: true},
    {name: 'nonstandard-scalars', result: {code: 'upstream-denied', msg: 'synthetic upstream message', data: 42}, new_success: false},
  ];
  let keys, retention;
  const classification = [];
  for (const vector of corpus) {
    await api.saveParseRecord({jxpan: {}}, 'https://example.invalid/share?pwd=synthetic-share#fragment', 'synthetic-share', vector.result);
    if (!captured) throw Error('Original save did not run');
    retention = captured.options.expirationTtl;
    const row = {key: captured.key, value: JSON.stringify(captured.record)};
    const db = {prepare(sql) {return {
      async first() {
        if (!sql.startsWith('SELECT COUNT')) throw Error('Unexpected oracle SQL');
        return {total: 1};
      },
      async all() {
        if (!sql.startsWith('SELECT key, value')) throw Error('Unexpected oracle SQL');
        return {results: [row]};
      },
    };}};
    const list = await api.d1GetParseRecords(db, {page: 1, limit: 10});
    keys = Object.keys(list);
    const record = [...list.success, ...list.failed][0];
    if (!record) throw Error('Original list did not return record');
    classification.push({...vector, legacy_success: record.success, saved_record: captured.record, frontend_record: record});
  }
  const lifetime = {total: 100, success: 70, failed: 30, cached: 12};
  context.storeGet = async () => lifetime;
  const stored_stats = await api.getStatsFromKV({jxpan: {}});
  const historyRows = Array.from({length: 75}, (_, i) => {
    const id = epoch + i;
    return {key: 'parse_record_' + id, value: JSON.stringify({id,
      url: i < 5 ? 'https://needle.invalid/share' : 'https://other.invalid/share',
      pwd: '', json: {code: 200}, timestamp: new FixedDate().toISOString()})};
  }).reverse();
  const historyDb = {prepare(sql) {return {
    async first() { return {total: historyRows.length}; },
    async all() {
      const match = sql.match(/LIMIT (\d+) OFFSET (\d+)/);
      if (!match) throw Error('Unexpected pagination SQL');
      return {results: historyRows.slice(Number(match[2]), Number(match[2]) + Number(match[1]))};
    },
  };}};
  const pagination_bug = await api.d1GetParseRecords(historyDb, {page: 1, limit: 2, search: 'needle.invalid'});
  // Execute the two exact frozen request-counter mutation excerpts, not rewritten logic.
  const counterBlocks = readable.match(/const statsObj = await getStatsFromKV\(env\);\s+statsObj\.total\+\+;\s+(?:statsObj\.success\+\+;\s+statsObj\.cached\+\+;|result\.success \? statsObj\.success\+\+ : statsObj\.failed\+\+;)\s+await updateStatsInKV\(env, statsObj\);/g);
  if (counterBlocks?.length !== 2) throw Error('Frozen counter blocks missing or ambiguous');
  const increment = vm.runInContext(`({cached: async (env, result) => {${counterBlocks[0]}}, fresh: async (env, result) => {${counterBlocks[1]}}})`, context, {timeout: 3000});
  let counters = {total: 0, success: 0, failed: 0, cached: 0};
  context.storeGet = async () => ({...counters});
  context.updateStatsInKV = async (_env, value) => { counters = {...value}; };
  const counter_events = [];
  for (const event of [
    {result: {success: false, code: 200}, cached: false},
    {result: {success: true}, cached: false},
    {result: {success: false, code: 403}, cached: true},
    {result: {code: 200}, cached: false},
  ]) {
    await increment[event.cached ? 'cached' : 'fresh']({jxpan: {}}, event.result);
    counter_events.push({...event, stats: {...counters}});
  }
  return {
    source_sha256: sourceSha,
    function_sha256: createHash('sha256').update(source).digest('hex'),
    provenance: 'Exact frozen getStatsFromKV/saveParseRecord/d1GetParseRecords; synthetic offline stubs; original history classification and frontend fields preserved',
    counter_source_sha256: createHash('sha256').update(counterBlocks.join('\n')).digest('hex'),
    counter_events,
    stored_stats,
    pagination_bug,
    default_stats: await api.getStatsFromKV(null),
    legacy_retention_seconds: retention,
    list_keys: keys,
    classification,
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const result = await generateRecordsVectors();
  if (process.argv.includes('--write')) {
    await writeFile(new URL('../tests/fixtures/records.json', import.meta.url), JSON.stringify(result, null, 2) + '\n');
  }
  process.stdout.write(JSON.stringify(process.argv.includes('--summary')
    ? {source_sha256: result.source_sha256, vectors: result.classification.length, legacy_retention_seconds: result.legacy_retention_seconds}
    : result) + '\n');
}
