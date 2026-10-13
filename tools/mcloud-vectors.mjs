#!/usr/bin/env node
// Executes ONLY pinned deployed pure methods. No module entrypoint or routes run.
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const baseline = new URL('../.scratch/baseline/', import.meta.url);
const read = name => readFileSync(new URL(name, baseline), 'utf8');
const sha = text => createHash('sha256').update(text).digest('hex');
const deploymentSha256 = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const worker = read('_worker.js');
if (sha(worker) !== deploymentSha256) throw Error('Deployment SHA-256 mismatch; never use a historical oracle');
const source = read('readable.js');
const parser = read('MobileCloudParser.js');
const route = read('handleMainRequest.js');
for (const [text, expected] of [
  [source, '7196db8435e214bc0bd46f60534a247652cf426185c272a9bd9bd0c3b15eaa79'],
  [parser, '4c2784a32492b016548fcbf6286a71fd2ca79b995280aeb3eff8d45bba680661'],
  [route, '156122bbbc652bc133a92c28e8d870eb15ab52f4fb902a84d52af9a3500cfddf'],
]) if (sha(text) !== expected) throw Error('Readable/extracted source hash mismatch');
function span(start, end) {
  const a = source.indexOf(start), b = source.indexOf(end, a + start.length);
  if (a < 0 || b < 0) throw Error(`Missing pinned source boundaries: ${start}`);
  return source.slice(a, b);
}
const embeddedParser = span('class MobileCloudParser {', 'function handleResponse(').trim();
// The class is immediately followed by handleResponse in this deployed readable file.
if (embeddedParser !== parser.trim()) throw Error('Extracted parser differs from deployed readable class');
const sandbox = {
  console: Object.freeze({ log() {}, warn() {}, error() {} }),
  atob, btoa, TextEncoder,
  // Deliberately absent: process, require, fetch, timers, storage and crypto.subtle.
};
const context = vm.createContext(sandbox, { codeGeneration: { strings: false, wasm: false } });
const pureSource = span('function __p_jRSW_cff_hash(', 'const cookieCache =')
  + span('function md5(input)', 'function parseRsaPublicKey(')
  + span('function pythonQuoteExact(', 'function generateCacheKey(')
  + parser;
vm.runInContext(pureSource, context, { filename: fileURLToPath(new URL('readable.js', baseline)), timeout: 5000 });
// Freeze time and entropy INSIDE VM without modifying generateMcloudSign itself.
vm.runInContext(`
const NativeDate = Date;
Date = class extends NativeDate {
  constructor() { super('2026-05-14T04:34:56Z'); }
  getFullYear() { return 2026; } getMonth() { return 4; } getDate() { return 14; }
  getHours() { return 12; } getMinutes() { return 34; } getSeconds() { return 56; }
};
crypto = Object.freeze({ getRandomValues(a) { for (let i=0;i<a.length;i++) a[i]=i*17; return a; } });
const instance = Object.create(MobileCloudParser.prototype);
globalThis.oracle = (method, input) => {
  switch (method) {
    case 'extractShareInfo': return instance.extractShareInfo(input).linkId;
    case 'extractAccountFromAuth': return instance.extractAccountFromAuth(input);
    case 'buildHeaders': instance.authorization=input; return instance.buildHeaders().authorization ?? null;
    case 'isFolder': return instance.isFolder(input);
    case 'md5': return md5(input);
    case 'pythonQuoteExact': return pythonQuoteExact(input);
    case 'generateMcloudSign': return { ...generateMcloudSign(input, 'ignored-device', 'ignored-version'), json: input ? JSON.stringify(input) : null };
    case 'decode': return atob(input);
    case 'encode': return 'Basic ' + btoa(input);
    default: throw Error('Method not whitelisted');
  }
};`, context, { timeout: 5000 });
const vectors = [];
function add(method, input) {
  context.method = method; context.input = input;
  let expected, error = false;
  try { expected = vm.runInContext('oracle(method,input)', context, { timeout: 2000 }); }
  catch { error = true; expected = null; }
  vectors.push({ method, input, expected, error });
}
const links = [
  '', 'not a share', '/w/i/A1', '/w/i/A1?x=1', '/w/i/A1&x=1', '/w/i/A1#x',
  'https://yun.139.com/shareweb/#/w/i/Ab9', 'http://caiyun.139.com/shareweb/#/w/i/Ab9',
  'https://yun.139.com/shareweb/?linkId=Ab9', 'https://caiyun.139.com/link/?linkId=Ab9&x=1',
  'https://yun.139.com/link/?linkId=Ab9?x=1', 'https://foreign.example/?linkId=Ab9',
  'https://foreign.example/w/i/Ab9', '/w/i/A_B', '/w/i/A-B', '/w/i/https',
  '/w/i/Abhttps', '/w/i/AbHTTPS', '/w/i/A%26x=1', '/w/i/A%2526x=1',
  '%2Fw%2Fi%2FAb', '%ZZ /w/i/A', '/w/i/A%FF', '/w/i/A%ED%A0%80',
  '\ufeff /w/i/A\u00a0', '/w/i/中文', 'linkId=A& /w/i/B',
  '/w/i/Early& https://yun.139.com/shareweb/#/w/i/Late',
  'http://caiyun.139.com/shareweb/#/w/i/First& https://yun.139.com/shareweb/#/w/i/Second',
];
for (const x of links) add('extractShareInfo', x);
for (const host of ['yun.139.com','caiyun.139.com','foreign.example','yun.139.com.evil'])
for (const scheme of ['http','https','HTTPS','ftp'])
for (const path of ['/shareweb/#/w/i/','/shareweb/?linkId=','/link/?linkId=','/w/i/','/?x=1&LINKID='])
for (const id of ['A1','aHTTPS','ahttps','a-b','a_b',''])
for (const tail of ['', '&p=1','?p=1','#p','/',' text']) add('extractShareInfo', `${scheme}://${host}${path}${id}${tail}`);
const authInputs = [null, false, true, 0, 1, [], {}, '', ' ', 'Basic ', 'basic\t', '!!!', 'Basic!!!',
  {authorization: ''}, {auth: '!!!'}, {authorization: false, auth: btoa('pc:synthetic:token')},
  {authorization: 1, auth: btoa('pc:synthetic:token')}, {authorization: {nested:true}},
];
for (const text of ['', 'pc:synthetic:token|a|b|123', 'account', ':', 'pc:', 'pc:a:b:c', 'pc:a|b:c', '\x00:\xff:t', 'é:a:t']) {
  const b = btoa(text);
  for (const prefix of ['', 'Basic ', 'basic\t', 'BASIC\n', 'Basic', 'Bearer ', '\ufeffBasic ']) {
    for (const payload of [b, b.replace(/=+$/, ''), b.slice(0,2)+' \n'+b.slice(2)]) authInputs.push(prefix+payload);
  }
  add('encode', text);
}
add('encode','中文'); add('encode','😀');
for (const x of authInputs) add('extractAccountFromAuth', x);
for (const x of ['', ' ', 'Basic ', 'Basic \tYWJj\n', 'basic YWJj', '\ufeffBasic\u00a0YWJj', 'BASIC YWJj', 'Bearer a b', 'BasicBasicYWJj']) add('buildHeaders', x);
for (const x of ['', 'YQ','YR','YQ=','YQ==','YQ===','Y','Y===','YWJj','YWJj=','YWJj====','Y Q\n==','YQ\v==','YQ\u00a0==','_-//','/w==','//==','é']) {
  add('decode',x); add('extractAccountFromAuth',x);
}
for (const coType of [null,0,1,1.0,'1',true,2])
for (const resType of [null,0,1,'1',true])
for (const isDir of [null,false,true,0,1,'0','1','true',{},[]])
for (const type of [null,'folder','Folder','file']) add('isFolder',{coType,resType,isDir,type});
for (const x of [{}, {isFolder:true}, [], '', 1, false, null]) add('isFolder',x);
for (let n=0;n<=160;n++) add('md5','a'.repeat(n));
for (const x of ['', 'abc', '中文', '😀', 'a-z_~.* /?:&=+', '\0é', 'a'.repeat(1024)]) {
  add('pythonQuoteExact',x); add('md5',x);
}
for (const body of [null,false,0,'',{},[],{clientCode:'10701',type:'1'},
  {encryptMsg:'synthetic+base64==',clientId:'10701',returnToken:true},
  {unicode:'中文😀',punctuation:'-_.~!* /?:&=+'}, {b:2,a:1}, {a:1,b:2},
  {'10':'x','2':'y',z:'\n"\\'}, ['😀',true,null,1], '😀']) add('generateMcloudSign',body);
const result = {
  deploymentSha256, readableSha256: sha(source), parserSha256: sha(parser), routeSha256: sha(route),
  oracle: 'isolated VM: unchanged deployed named methods; fixed local date and 16 entropy bytes',
  vectors,
};
if (process.argv.includes('--summary')) {
  const counts = {}; for (const {method} of vectors) counts[method]=(counts[method]||0)+1;
  console.log(JSON.stringify({ ...result, vectors: undefined, counts, total:vectors.length,
    vectorSha256:sha(JSON.stringify(vectors)), md5Empty:vectors.find(v=>v.method==='md5'&&v.input==='').expected }, null, 2));
} else console.log(JSON.stringify(result));
