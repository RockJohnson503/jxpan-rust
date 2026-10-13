#!/usr/bin/env node
// Unchanged deployed AESUtils2 and MobileCloudParser methods, strict memory fetch.
// No real account, socket, worker entrypoint, login, storage, or route execution.
import { readFileSync } from 'node:fs';
import { createHash, webcrypto } from 'node:crypto';
import vm from 'node:vm';
import assert from 'node:assert/strict';

const baseline = new URL('../.scratch/baseline/', import.meta.url);
const read = name => readFileSync(new URL(name, baseline), 'utf8');
const sha = text => createHash('sha256').update(text).digest('hex');
const deploymentSha256 = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
assert.equal(sha(read('_worker.js')), deploymentSha256, 'deployment changed');
const source = read('readable.js'), parser = read('MobileCloudParser.js');
assert.equal(sha(source), '7196db8435e214bc0bd46f60534a247652cf426185c272a9bd9bd0c3b15eaa79');
assert.equal(sha(parser), '4c2784a32492b016548fcbf6286a71fd2ca79b995280aeb3eff8d45bba680661');
function span(start, end) {
  const a = source.indexOf(start), b = source.indexOf(end, a + start.length);
  assert(a >= 0 && b >= 0, 'pinned source boundaries absent');
  return source.slice(a, b);
}
assert.equal(span('class MobileCloudParser {', 'function handleResponse(').trim(), parser.trim());
const iv = Array.from({ length: 16 }, (_, i) => i * 17);
const endpoints = new Set([
  'https://share-kd-njs.yun.139.com/yun-share/richlifeApp/devapp/IOutLink/getOutLinkInfoV6',
  'https://share-kd-njs.yun.139.com/yun-share/richlifeApp/devapp/IOutLink/dlFromOutLinkV3',
]);
let response, captured;
const context = vm.createContext({
  console: Object.freeze({ log() {}, warn() {}, error() {} }),
  atob, btoa, TextEncoder, TextDecoder, AbortController,
  crypto: Object.freeze({
    subtle: webcrypto.subtle,
    getRandomValues(array) { assert.equal(array.length, 16); array.set(iv); return array; },
  }),
  // Original code only needs timer handles; no timers or network are scheduled.
  setTimeout() { return 1; }, clearTimeout() {},
  async fetch(url, options) {
    assert(endpoints.has(url), 'fetch tried a non-pinned endpoint');
    assert.equal(captured, null, 'unexpected second fetch (pagination?)');
    assert.equal(options.method, 'POST');
    assert(options.signal instanceof AbortSignal);
    captured = { url, method: options.method, headers: options.headers,
      encryptedBody: options.body, body: await context.aes.decrypt139(options.body) };
    return { status: response.status, async text() { return response.wire; } };
  },
}, { codeGeneration: { strings: false, wasm: false } });
vm.runInContext(span('class AESUtils2 {', 'class MobileCloudParser {') + parser + `
  globalThis.aes = AESUtils2;
  globalThis.makeParser = auth => new MobileCloudParser({mcloud:{authorization:auth}});
`, context, { timeout: 5000 });
const aes = [];
for (const text of [
  ...Array.from({length: 65}, (_, n) => 'x'.repeat(n)),
  '中文', '😀', 'é', '\ufeffJSON', '中文😀é\0\n',
  ...Array.from({length: 17}, (_, n) => '😀'.repeat(n) + '中'.repeat(16 - n)),
]) {
  const encrypted = await context.aes.encrypt139(text);
  aes.push({ text, iv, encrypted, decrypted: await context.aes.decrypt139(encrypted) });
}
const auth = 'Basic ' + btoa('pc:synthetic:token|test');
const requests = [];
async function call(method, args, raw, options = {}) {
  const authorization = options.authorization ?? auth;
  response = { status: options.status ?? 200,
    wire: options.plain ? raw : await context.aes.encrypt139(raw) };
  captured = null;
  const instance = context.makeParser(authorization);
  const expected = await instance[method](...args);
  requests.push(JSON.parse(JSON.stringify({ method, args, authorization, raw,
    status: response.status, wire: response.wire, expected, captured,
    directoryRescueProbe: options.directoryRescueProbe ?? null })));
}
const list = (args, raw, options) => call('getShareFiles', args, typeof raw === 'string' ? raw : JSON.stringify(raw), options);
const dl = (args, raw, options) => call('getDownloadUrl', args, typeof raw === 'string' ? raw : JSON.stringify(raw), options);
const goodList = {success: true, data: {coLst: [{coID:'file', coName:'a.mkv', path:'/a', coSize:5}],
  caLst: [{caID:'dir', caName:'目录'}, {id:'alt', name:'备用', path:'/custom'}]}};
for (const args of [['Share1','','root'], ['Share1','1234','child'], ['','',''],
  ['中文😀','密😀','目录'], ['a"\\b','"pwd','\\folder'], ['$&','$$',"$`$'"],
  ['{pwd}','{account}','{key}']]) await list(args, goodList);
for (const authorization of ['', ' ', '!!!', 'basic '+btoa('pc:synthetic:t'),
  'Basic \t'+btoa('pc:synthetic:t')+'\n', 'Basic '+btoa('pc::t'),
  'Basic '+btoa('pc:$&$$:t')]) await list(['S','','root'], goodList, {authorization});
const listResponses = [
  {resultCode:'0',data:{files:[{id:'f'}]}},
  {resultCode:0,data:{coLst:[{id:'never'}]}},
  {success:'yes',data:{fileList:[{id:'f'}],caLst:[{}, {caID:0,id:7,caName:0,name:'n'}, {caID:'',id:null}, {caID:[],caName:{x:1},path:[]}]}},
  {success:true,data:{coLst:[],fileList:[{id:'not-used'}]}},
  {success:true,data:{coLst:{},files:[{id:'not-used'}],caLst:[]}},
  {success:true,data:{coLst:false,contentList:[{id:'content'}]}},
  {success:true,data:{caLst:['x',false,7,[]]}},
  {success:true,data:{caLst:[null]}},
  {success:false,resultCode:'bad',data:{coLst:[{id:'never'}]}},
  {success:true}, {}, null, [], 5,
];
for (const raw of listResponses) await list(['S','','root'], raw);
await list(['S','','root'], goodList, {plain:true});
await list(['S','','root'], '', {plain:true});
await list(['S','','root'], 'not JSON', {plain:true});
await list(['S','','root'], goodList, {status:403});
// Execute the unchanged VM catch path, including its narrow/buggy fallbacks.
// Explicit fixture expectations prevent a test suite from merely accepting a
// broadened rescue implementation. All documents remain small synthetic bodies.
const folder = (id, name) => ({coID:id, coName:name, coType:1, isDir:true, path:'/'+id});
const truncated = data => JSON.stringify({success:true,data}).slice(0,-2);
const directoryRescueProbes = [
  ['complete-caLst-coLst', '{"success":true,"data":{"caLst":[{"caID":"d","caName":"D"}],"coLst":[{"coID":"f"}]',
    [folder('d','D'), {coID:'f'}]],
  ['coLst-without-success', '{"data":{"coLst":[{"coID":"f"}]', [{coID:'f'}]],
  ['caLst-nested-arrays-objects', truncated({caLst:[{caID:'nested',caName:'Nested',meta:{a:[1,[2,{b:3}]]}}]}),
    [folder('nested','Nested')]],
  ['caLst-string-brackets-quotes', truncated({caLst:[{caID:'quoted',caName:'A [x] {y} "z" \\ q 😀'}]}),
    [folder('quoted','A [x] {y} "z" \\ q 😀')]],
  ['caLst-even-backslash-quirk', truncated({caLst:[{caID:'d',caName:'ends\\'}]}), []],
  ['coLst-nested-objects', truncated({coLst:[{coID:'f',meta:{a:{b:1}}}]}), [{coID:'f',meta:{a:{b:1}}}]],
  ['coLst-nested-array-fallback-stays-failed', truncated({coLst:[{coID:'f',meta:[1,[2]]}]}), []],
  ['coLst-string-close-bracket-stays-failed', truncated({coLst:[{coID:'f',coName:'a]b'}]}), []],
  ['coLst-string-open-bracket', truncated({coLst:[{coID:'f',coName:'a[b'}]}), [{coID:'f',coName:'a[b'}]],
  ['coLst-string-quotes', truncated({coLst:[{coID:'f',coName:'a "b" {c} \\ d'}]}), [{coID:'f',coName:'a "b" {c} \\ d'}]],
  ['coLst-extra-close-for-literal-depth-fallback', '{"coLst":[{"coID":"f","meta":[1]}]]', []],
  ['coLst-extra-close-after-nested-bracket', '{"coLst":[{"coID":"f","meta":[1]}]]}', []],
  ['caLst-simple-id-name-no-object-close', '{"caLst":[{"caID":"d","caName":"D"]', [folder('d','D')]],
  ['caLst-simple-first-only', '{"caLst":[{"caID":"d","caName":"D",broken},{"caID":"e","caName":"E"}]', [folder('d','D')]],
  ['caLst-simple-raw-escapes', String.raw`{"caLst":[{"caID":"d\\x","caName":"D\"Q",broken}]`,
    [folder(String.raw`d\\x`, 'D\\')]],
  ['caLst-simple-requires-id-first', '{"caLst":[{"caName":"D","caID":"d",broken}]', []],
  ['caLst-simple-requires-tight-open-brace', '{"caLst":[{ "caID":"d","caName":"D",broken}]', []],
  ['caLst-simple-stops-at-object-brace', '{"caLst":[{"caID":"d","meta":{},"caName":"D",broken}]', []],
  ['caLst-id-only', truncated({caLst:[{caID:'d'}]}), [folder('d','未命名文件夹')]],
  ['caLst-name-only', truncated({caLst:[{caName:'D'}]}), [{coName:'D',coType:1,isDir:true,path:'/undefined'}]],
  ['single-file-regex-truncation', '{"coLst":[{"coID":"f"},{"coID":"cut"', []],
  ['single-file-missing-object-close', '{"coLst":[{"coID":"f"]', []],
  ['caLst-array-unclosed-no-simple-rescue', '{"caLst":[{"caID":"d","caName":"D"}', []],
  ['caLst-closed-coLst-unclosed', '{"caLst":[{"caID":"d","caName":"D"}],"coLst":[{"coID":"f"}', [folder('d','D')]],
  ['coLst-space-before-colon-not-rescued', '{"coLst" : [{"coID":"f"}]', []],
  ['caLst-js-whitespace', '{"caLst"\ufeff:\u2028[{"caID":"d","caName":"D"}]', [folder('d','D')]],
  ['caLst-NEL-not-js-whitespace', '{"caLst"\u0085:[{"caID":"d","caName":"D"}]', []],
  ['coLst-js-whitespace-after-colon', '{"coLst":\ufeff[{"coID":"f"}]', [{coID:'f'}]],
  ['no-array-fields', '{"success":true,"data":{', []],
  ['no-fileList-rescue', '{"data":{"fileList":[{"coID":"f"}]', []],
  ['closed-empty-arrays', '{"caLst":[],"coLst":[]', []],
  ['unsuccessful-but-truncated-still-rescued', '{"success":false,"caLst":[{"caID":"d","caName":"D"}]', [folder('d','D')]],
  ['unsuccessful-valid-never-rescued', '{"success":false,"data":{"caLst":[{"caID":"d","caName":"D"}]}}', []],
  ['valid-json-folder-conversion-throw', {success:true,data:{coLst:[{coID:'f'}],caLst:[{caID:'d',caName:'D'},null]}},
    [folder('d','D'),{coID:'f'}]],
  ['null-folder-fallback-no-partial-map', truncated({caLst:[{id:'discard',name:'discard'},null],coLst:[{coID:'f'}]}), [{coID:'f'}]],
  ['both-closed-normal-order', {success:true,data:{coLst:[{coID:'f'}],caLst:[{caID:'d',caName:'D'}]}},
    [{coID:'f'},folder('d','D')]],
];
for (const [name, raw, expected] of directoryRescueProbes) {
  await list(['S','','root'], raw, {directoryRescueProbe:name});
  assert.deepEqual(requests.at(-1).expected, expected, `original rescue fixture: ${name}`);
}
// The original rescue applies equally to plaintext and AES-framed responses.
await list(['S','','root'], directoryRescueProbes[0][1], {plain:true,directoryRescueProbe:'plaintext-complete-caLst-coLst'});
assert.deepEqual(requests.at(-1).expected, directoryRescueProbes[0][2]);
for (const args of [['S','/folder/file'], ['S','co-id'], ['',''],
  ['中文😀','路径😀'], ['$&',"$$$`$'"], ['{item}','{account}'], ['S','a"\\b']]) {
  await dl(args, {success:true,data:{redrUrl:'https://download.example.test/file?token=synthetic'}});
}
for (const raw of [
  {resultCode:'0',data:{downloadUrl:'https://download.example.test/a'}},
  {success:1,data:{redrUrl:'',downloadUrl:'',url:'https://download.example.test/b'}},
  {success:true,data:{redrUrl:7,url:'https://not-selected.example.test'}},
  {success:true,data:{}}, {resultCode:0,data:{url:'https://never.example.test'}},
  {success:false}, null,
  '{"redrUrl":"https://download.example.test/truncated"',
  '{"downloadUrl":"https:\\/\\/download.example.test/raw"',
  '{"redrUrl" : "https://download.example.test/not-matched"',
  '{"url":"https://download.example.test/fallback"',
  'not JSON',
]) await dl(['S','id'], raw);
await dl(['S','id'], {success:true,data:{url:'https://download.example.test/plain'}}, {plain:true});
await dl(['S','id'], '', {plain:true});
await dl(['S','id'], {success:true,data:{url:'https://never.example.test'}}, {status:500});
const result = {deploymentSha256, oracle:'unchanged original AESUtils2 WebCrypto + MobileCloudParser; strict memory fetch; fixed IV; synthetic inputs', aes, requests};
if (process.argv.includes('--summary')) console.log(JSON.stringify({deploymentSha256, aes:aes.length, requests:requests.length,
  directoryRescueProbes:requests.filter(x=>x.directoryRescueProbe !== null).length, vectorSha256:sha(JSON.stringify(result))}, null, 2));
else console.log(JSON.stringify(result));
