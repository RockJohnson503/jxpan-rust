// Exact deployed class / shared walker / fast function in a networkless VM.
// Synthetic credentials only; never reads credentials.json. No deployment modifications.
import { readFile } from 'node:fs/promises';
import { createHash, webcrypto } from 'node:crypto';
import { parse } from 'acorn';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';
const base = new URL('../.scratch/baseline/', import.meta.url);
const sha = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const epoch = 1700000000123;
const file = (id, name = id, size = 12) => ({fileId:id,fileName:name,fileSize:size,isFolder:false});
const dir = (id, name = id) => ({fileId:id,fileName:name,fileSize:0,isFolder:true});
const root = {...dir('r','共享'),shareId:'s'};
const graph = {r:[dir('a','目录'),file('f','根.mp4')],a:[dir('b','深层'),file('g')],b:[file('z','片段.mkv','123456789012345678')]};
const corpus = [
  {name:'root-tree',info:root,graph},
  {name:'deep-tree',info:root,graph,chain:['a','b']},
  {name:'missing-fid-fallback',info:root,graph,chain:['unknown']},
  {name:'empty-root',info:root},
  {name:'failed-child-is-empty',info:root,graph,failures:['a']},
  {name:'duplicates-collisions',info:root,graph:{r:[dir('a','same'),dir('a','again'),file('x','same')],a:[file('y')]}},
  {name:'max-400-concurrency-6',info:root,graph:{r:Array.from({length:450},(_,i)=>dir('d'+i))}},
  {name:'single-open',info:{...file('f','视频.mp4',42),shareId:'s'},downloads:{open:'https://offline.invalid/file'}},
  {name:'single-app-dt1',info:{...file('f'),shareId:'s'},downloads:{app_dt1:'https://offline.invalid/file'}},
  {name:'single-app-dt3',info:{...file('f'),shareId:'s'},downloads:{app_dt3:'https://offline.invalid/file'}},
  {name:'zero-size-treated-as-directory',info:{...file('f','zero',0),shareId:'s'}},
  {name:'pwd-overrides-sid',info:root,graph,pwd:'密 码',checked:{success:true,shareId:'new-s'}},
  {name:'wrong-pwd',info:root,pwd:'wrong',checked:{success:false}},
  {name:'blank-pwd-not-checked',info:root,pwd:'  '},
  {name:'invalid-url',url:'invalid'},
  {name:'missing-info',info:null},
  {name:'missing-fileid',info:{shareId:'s'}},
  {name:'missing-sid-directory',info:dir('r')},
  {name:'ordinary-no-direct',info:{...file('f'),shareId:'s'},difference:'no-transfer/no-tokenpool sanitized error'},
  {name:'fast-app-dt1',fast:true,info:{...file('f'),shareId:'s'},id:'f',downloads:{app_dt1:'https://offline.invalid/fast'}},
  {name:'fast-app-dt3',fast:true,info:root,id:'f',downloads:{app_dt3:'https://offline.invalid/fast'}},
  {name:'fast-open-fallback',fast:true,info:root,id:'f',downloads:{open:'https://offline.invalid/fast'}},
  {name:'fast-existing-sid-skips-share-pwd',fast:true,url:'invalid',pwd:'wrong',sid:'known',id:'f',downloads:{app_dt1:'https://offline.invalid/fast'}},
  {name:'fast-root-folder-rejected',fast:true,info:root,id:'r'},
  {name:'fast-zero-size-rejected',fast:true,info:{...file('f','zero',0),shareId:'s'},id:'f'},
  {name:'fast-folder-download-endpoint',fast:true,info:root,id:'f',downloads:{app_dt1:'https://download.cloud.189.cn/file/downloadFile.action?id=f'}},
  {name:'fast-password-failure-retains-sid',fast:true,info:root,pwd:'wrong',checked:{success:false},id:'f',downloads:{app_dt1:'https://offline.invalid/fast'}},
  {name:'fast-no-sid',fast:true,info:{...file('f')},id:'f'},
  {name:'fast-no-link',fast:true,info:root,id:'f'},
];
export async function generateServiceVectors() {
  const raw = await readFile(new URL('_worker.js',base));
  if (createHash('sha256').update(raw).digest('hex') !== sha) throw new Error('Deployment SHA mismatch');
  const readable = await readFile(new URL('readable.js',base),'utf8');
  const klass = await readFile(new URL('Cloud189Parser.js',base),'utf8');
  if (!readable.includes(klass.trim())) throw new Error('Extracted class differs');
  const wanted = new Set(['jxpanWalkDirTree','jxpanBuildNestedTree','jxpanDirCacheGet','jxpanDirCacheSet',
    'jxpanFastCloud189Play','jxpanFolderDownResponse','jxpanDownFailResponse','jxpanParseJsonSafe']);
  const ast = parse(readable,{ecmaVersion:'latest',sourceType:'module'});
  const originals = ast.body.filter(n=>n.type==='FunctionDeclaration' && wanted.has(n.id.name));
  if (originals.length !== wanted.size) throw new Error('Missing/ambiguous original functions');
  const source = originals.map(n=>readable.slice(n.start,n.end)).join('\n')+'\n'+klass;
  const vectors = [];
  for (const input of corpus) {
    const spec = {url:'https://cloud.189.cn/t/AbC123',pwd:'',chain:[],graph:{},downloads:{},...input};
    const calls = [], rawTrace = [];
    let active=0,peak=0,violation=null;
    class FixedDate extends Date { constructor(...args){super(...(args.length?args:[epoch]));} static now(){return epoch;} }
    const subtle = {
      digest:async (alg,data)=>{
        if (String(alg).toUpperCase() !== 'MD5') return webcrypto.subtle.digest(alg,data);
        const b=createHash('md5').update(Buffer.from(data)).digest();
        return b.buffer.slice(b.byteOffset,b.byteOffset+b.byteLength);
      },
      importKey:(...a)=>webcrypto.subtle.importKey(...a),sign:(...a)=>webcrypto.subtle.sign(...a),
    };
    const context = vm.createContext({URL,URLSearchParams,Response,TextEncoder,Date:FixedDate,
      Math:Object.assign(Object.create(Math),{random:()=>0.25}),crypto:{subtle},
      console:{log(){},error(){}},
      // Renewal and login are intentionally outside the read-only test target.
      jxpanCloud189RefreshOnDemand:async()=>null,
      fetch:async (input,options={})=>{
        const u=new URL(input);
        const headers=Object.fromEntries(Object.entries(options.headers||{}).map(([k,v])=>[k.toLowerCase(),String(v)]));
        rawTrace.push({url:String(input),method:options.method||'GET',headers,redirect:options.redirect||null});
        let key;
        if(u.origin==='https://api.cloud.189.cn' && u.pathname==='/getFileDownloadUrl.action') key=u.searchParams.get('dt')==='1'?'app_dt1':'app_dt3';
        else if(u.origin==='https://api.cloud.189.cn' && u.pathname==='/open/file/getFileDownloadUrl.action') key='open';
        else if(Object.values(spec.downloads).includes(String(input))) {
          if(headers.range!=='bytes=0-0' || options.redirect!=='manual' || (options.method||'GET')!=='GET') {
            violation=new Error('Unexpected Range hop contract');throw violation;
          }
          return {status:200,headers:new Headers()};
        } else {violation=new Error('Forbidden offline fetch');throw violation;}
        calls.push([key,u.searchParams.get('fileId'),key==='app_dt3'?'':u.searchParams.get('shareId')]);
        return {status:200,headers:new Headers(),text:async()=>JSON.stringify(spec.downloads[key]?{fileDownloadUrl:spec.downloads[key]}:{res_code:403})};
      },
    });
    const exported=vm.runInContext(source+'\n({Parser:Cloud189Parser,fast:jxpanFastCloud189Play});',context,{timeout:2000});
    const proto=exported.Parser.prototype;
    proto.ensureCookieLoginUser=async()=>{};
    proto.getShareInfo=async code=>{calls.push(['share',code]);return spec.info??null;};
    proto.checkAccessCode=async (code,pwd)=>{calls.push(['pwd',code,pwd]);return spec.checked||{success:true,shareId:'s'};};
    proto.listShareDir=async (sid,id,pwd,mode)=>{
      calls.push(['list',sid,id,mode]);active++;peak=Math.max(peak,active);
      for(let i=0;i<(id.length%3)+1;i++) await Promise.resolve();active--;
      if(spec.failures?.includes(id)) throw new Error('offline list error');
      return spec.graph[id]||[];
    };
    proto.saveAndDownload=async()=>{violation=new Error('Transfer attempted');throw violation;};
    const config={openAccessToken:'offline-open-token-123456789',sessionKey:'offline-session',sessionSecret:'offline-secret',
      allTokens:[{token:'offline-open-token-123456789',source:'offline',has_valid_open_token:true}]};
    let expected;
    if(spec.fast) {
      const resp=await exported.fast({},config,spec.url,spec.pwd,spec.id,spec.sid||'');
      expected={status:resp.status,location:resp.headers.get('location'),body:await resp.text()};
    } else {
      const parser=new exported.Parser(config);
      if(spec.chain.length) {
        // Method-level fid projection, using the ORIGINAL class browse and shared functions.
        const s=parser.extractShareInfo(spec.url), info=await parser.getShareInfo(s.shareCode);
        let sid=info.shareId;
        if(spec.pwd.trim()) { const checked=await parser.checkAccessCode(s.shareCode,spec.pwd);sid=checked.shareId||sid; }
        expected=await parser.browseFolderInteractive({...info,shareId:sid},spec.chain.at(-1),info.fileName,spec.pwd);
      } else expected=await parser.parse(spec.url,spec.pwd);
    }
    if(violation) throw violation;
    vectors.push({input:spec,expected,calls,peak,rawTrace});
  }
  return {source_sha256:sha,function_sha256:createHash('sha256').update(source).digest('hex'),vectors};
}
if(process.argv[1]===fileURLToPath(import.meta.url)) {
  const result=await generateServiceVectors();
  process.stdout.write(JSON.stringify(process.argv.includes('--summary')?{source_sha256:sha,vectors:result.vectors.length}:result)+'\n');
}
