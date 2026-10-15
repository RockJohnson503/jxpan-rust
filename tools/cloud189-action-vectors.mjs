// Frozen action blocks + original class/walker in a networkless VM. Synthetic data only.
import { readFile } from 'node:fs/promises';
import { createHash, webcrypto } from 'node:crypto';
import { parse } from 'acorn';
import vm from 'node:vm';
const base = new URL('../.scratch/baseline/', import.meta.url);
const sha = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const raw = await readFile(new URL('_worker.js', base));
if (createHash('sha256').update(raw).digest('hex') !== sha) throw Error('Deployment SHA mismatch');
const readable = await readFile(new URL('readable.js', base), 'utf8');
const klass = await readFile(new URL('Cloud189Parser.js', base), 'utf8');
const main = await readFile(new URL('handleMainRequest.js', base), 'utf8');
if (!readable.includes(klass.trim()) || !readable.includes(main.trim())) throw Error('Extract mismatch');
const wanted = new Set(['jxpanWalkDirTree','jxpanBuildNestedTree','jxpanDirCacheGet','jxpanDirCacheSet','jxpanParseJsonSafe']);
const ast = parse(readable, {ecmaVersion:'latest',sourceType:'module'});
const functions = ast.body.filter(n => n.type === 'FunctionDeclaration' && wanted.has(n.id.name));
if (functions.length !== wanted.size) throw Error('Missing original function');
const blocks = [];
function visit(node) {
  if (!node || typeof node !== 'object') return;
  if (node.type === 'IfStatement' && /^action === "parse_cloud189_(folder_item|file)"$/.test(main.slice(node.test.start,node.test.end))) {
    blocks.push(main.slice(node.start,node.end)); return;
  }
  for (const value of Object.values(node)) if (Array.isArray(value)) value.forEach(visit); else if (value && typeof value === 'object') visit(value);
}
visit(parse(main,{ecmaVersion:'latest'}));
if (blocks.length !== 2) throw Error('Missing original action');
const source = functions.map(n => readable.slice(n.start,n.end)).join('\n') + '\n' + klass + '\nasync function actionOnly(request,env) { const url=new URL(request.url),action=url.searchParams.get("action");\n'+blocks.join('\n')+'\n}';
const dir = id => ({fileId:id,fileName:id,isFolder:true});
const graph = {r:[dir('a'),{fileId:'f',fileName:'film',fileSize:99}],a:[]};
const corpus = [
  {name:'missing-share',action:'file',share_id:''},
  {name:'missing-file',action:'file',file_id:''},
  {name:'open-encoded',file_name:"中 文+/?#&!'()*~.mp4",file_size:'  +0x20tail',downloads:{open:'https://offline.invalid/f?x=1#frag'}},
  {name:'app-shared',downloads:{app_dt1:'https://offline.invalid/f'}},
  {name:'app-owned',downloads:{app_dt3:'https://offline.invalid/f'}},
  {name:'negative-prefix',file_size:' -12.8suffix',downloads:{open:'https://offline.invalid/f'}},
  {name:'decimal-large',file_size:'9007199254740993',downloads:{open:'https://offline.invalid/f'}},
  {name:'invalid-size',file_size:'oops',downloads:{open:'https://offline.invalid/f'}},
  {name:'overflow-size',huge_size:true,downloads:{open:'https://offline.invalid/f'}},
  {name:'password-failure-ignored',pwd:'wrong',share_code:'code',checked:{success:false,shareId:'other'},downloads:{open:'https://offline.invalid/f'}},
  {name:'password-no-code',pwd:'password',downloads:{open:'https://offline.invalid/f'}},
  {name:'no-direct-no-transfer',difference:'transfer intentionally disabled'},
  {name:'folder-missing-target',action:'folder_item',target_file_id:''},
  {name:'folder-tree',action:'folder_item',graph},
  {name:'folder-empty',action:'folder_item'},
  {name:'folder-list-failure',action:'folder_item',failures:['r']},
  {name:'folder-existing-file-no-idmap',action:'folder_item',folder_id:'r',target_file_id:'f',graph},
  {name:'folder-existing-folder-no-idmap',action:'folder_item',folder_id:'r',target_file_id:'a',graph},
  {name:'folder-unknown-no-idmap',action:'folder_item',folder_id:'r',target_file_id:'missing',graph},
  {name:'folder-password-mode',action:'folder_item',share_mode:'3',pwd:'p',share_code:'code',checked:{success:false},graph},
];
const vectors=[];
for (const input of corpus) {
  const spec={action:'file',share_id:'s',file_id:'f',file_name:'',file_size:'0',folder_id:'',target_file_id:'r',pwd:'',share_code:'',share_mode:'1',downloads:{},graph:{},...input};
  const calls=[]; let violation=null;
  class FixedDate extends Date { constructor(...a){super(...(a.length?a:[1700000000123]));} static now(){return 1700000000123;} }
  const subtle={digest:async (alg,data)=> { if(String(alg).toUpperCase()!=='MD5') return webcrypto.subtle.digest(alg,data); const b=createHash('md5').update(Buffer.from(data)).digest();return b.buffer.slice(b.byteOffset,b.byteOffset+b.byteLength); },importKey:(...a)=>webcrypto.subtle.importKey(...a),sign:(...a)=>webcrypto.subtle.sign(...a)};
  const context=vm.createContext({URL,URLSearchParams,Response,TextEncoder,Date:FixedDate,crypto:{subtle},console:{log(){},error(){}},Math:Object.assign(Object.create(Math),{random:()=>.25}),
    storeGet:async()=>({expires_at:2000000000,open_access_token:'offline-open-token-123456789',has_valid_open_token:true,session_key:'offline-session',session_secret:'offline-secret'}),
    jxpanCloud189RefreshOnDemand:async()=>null,
    fetch:async (input)=> { const u=new URL(input);let key;
      if(u.origin==='https://api.cloud.189.cn'&&u.pathname==='/open/file/getFileDownloadUrl.action')key='open';
      else if(u.origin==='https://api.cloud.189.cn'&&u.pathname==='/getFileDownloadUrl.action')key=u.searchParams.get('dt')==='1'?'app_dt1':'app_dt3';
      else {violation=Error('Forbidden offline fetch');throw violation;}
      calls.push([key,u.searchParams.get('fileId'),key==='app_dt3'?'':u.searchParams.get('shareId')]);
      return {status:200,headers:new Headers(),text:async()=>JSON.stringify(spec.downloads[key]?{fileDownloadUrl:spec.downloads[key]}:{res_code:403})};
    }});
  const exported=vm.runInContext(source+'\n({Parser:Cloud189Parser,action:actionOnly})',context,{timeout:2000});
  exported.Parser.prototype.ensureCookieLoginUser=async()=>{};
  exported.Parser.prototype.checkAccessCode=async(code,pwd)=>{calls.push(['pwd',code,pwd]);return spec.checked||{success:true};};
  exported.Parser.prototype.listShareDir=async(sid,id,pwd,mode)=>{calls.push(['list',sid,id,mode]);if(spec.failures?.includes(id))throw Error('offline');return spec.graph[id]||[];};
  exported.Parser.prototype.saveAndDownload=async()=>{violation=Error('Forbidden transfer');throw violation;};
  const u=new URL('https://offline.invalid/');
  for(const key of ['share_id','file_id','file_name','file_size','folder_id','target_file_id','pwd','share_code','share_mode']) u.searchParams.set(key,key==='file_size'&&spec.huge_size?'9'.repeat(400):spec[key]);
  u.searchParams.set('action','parse_cloud189_'+spec.action);
  const response=await exported.action({url:String(u)},{jxpan:{}});
  if(violation)throw violation;
  vectors.push({input:spec,expected:await response.json(),calls});
}
const result={source_sha256:sha,function_sha256:createHash('sha256').update(source).digest('hex'),vectors};
if (process.argv.includes('--check-fixture')) {
  const { deepStrictEqual } = await import('node:assert');
  deepStrictEqual(result, JSON.parse(await readFile(new URL('../tests/fixtures/cloud189-action.json', import.meta.url), 'utf8')));
}
process.stdout.write(JSON.stringify(process.argv.includes('--summary')?{source_sha256:sha,vectors:vectors.length}:result)+'\n');
