// Exact frozen Cloud189Parser, executed in a VM. All fetch/storage/timers are
// synthetic; this program cannot mutate or delete a real cloud account.
import {readFile} from 'node:fs/promises';
import {createHash, webcrypto} from 'node:crypto';
import {parse} from 'acorn';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
const base=new URL('../.scratch/baseline/',import.meta.url);
const fixtureURL=new URL('../tests/fixtures/cloud189-transfer.json',import.meta.url);
export async function generateTransferVectors(){
 const corpus=JSON.parse(await readFile(fixtureURL,'utf8'));
 const raw=await readFile(new URL('_worker.js',base));
 assert.equal(createHash('sha256').update(raw).digest('hex'),corpus.source_sha256,'Frozen deployment changed');
 const source=await readFile(new URL('readable.js',base),'utf8');
 const classFile=await readFile(new URL('Cloud189Parser.js',base),'utf8');
 assert.equal(createHash('sha256').update(classFile).digest('hex'),'29689a2973e07b11e27280ef3eaec0a4653d73267e81f9d15faf2bedccff46b2');
 assert.ok(source.includes(classFile.trim()),'readable.js class differs from frozen extracted class');
 const ast=parse(source,{ecmaVersion:'latest',sourceType:'module'});
 const klass=ast.body.find(n=>n.type==='ClassDeclaration'&&n.id.name==='Cloud189Parser');
 const safe=ast.body.find(n=>n.type==='FunctionDeclaration'&&n.id.name==='jxpanParseJsonSafe');
 assert.ok(klass&&safe,'Missing frozen class/safe JSON parser');
 const methods=klass.body.body.map(n=>n.key.value??n.key.name);
 for(const method of corpus.missing_methods)assert.ok(!methods.includes(method),'Unreviewed new protocol: '+method);
 for(const method of ['saveShareFile','checkBatchTask','saveAndDownload','deleteFile','getRootDirectory'])assert.ok(methods.includes(method));
 const vectors=[];
 for(const spec of corpus.vectors){
  const trace=[],sleeps=[];let offset=0;
  const steps=[...spec.steps,...Array.from({length:spec.repeat_pending??0},()=>({res_code:0,taskStatus:1}))];
  class FixedDate extends Date{constructor(...a){super(...(a.length?a:[corpus.epoch]));}static now(){return corpus.epoch;}}
  const context=vm.createContext({URL,URLSearchParams,TextEncoder,Date:FixedDate,
   Math:Object.assign(Object.create(Math),{random:()=>0.25}),console:{log(){},error(){}},
   crypto:{subtle:{digest:async(alg,data)=>alg==='MD5'?createHash('md5').update(data).digest():webcrypto.subtle.digest(alg,data),
    importKey:(...a)=>webcrypto.subtle.importKey(...a),sign:(...a)=>webcrypto.subtle.sign(...a)}},
   setTimeout:(fn,delay)=>{assert.ok(delay<=2000);sleeps.push(delay);fn();return 0;},
   storeGet:async()=>null,storePut:async()=>{throw Error('Unexpected storage write');},storeDelete:async()=>{throw Error('Unexpected storage deletion');},
   fetch:async(url,options={})=>{
    const h=Object.fromEntries(Object.entries(options.headers??{}).map(([k,v])=>[k.toLowerCase(),String(v)]));
    trace.push({url:String(url),method:options.method??'GET',headers:h,body:options.body??null});
    assert.ok(offset<steps.length,'Unexpected memory request: '+spec.name);
    const value=steps[offset++];
    return {status:200,headers:{get:()=>null,getSetCookie:()=>[]},text:async()=>JSON.stringify(value),json:async()=>value};
   }});
  const Parser=vm.runInContext(source.slice(klass.start,klass.end)+'\n'+source.slice(safe.start,safe.end)+'\nCloud189Parser;',context,{timeout:3000});
  Parser.prototype.generateUUID=()=> '44444444-4444-4444-8444-444444444444';
  const c=corpus.credentials;
  const parser=new Parser({cookieLoginUser:c.cookie_login_user,sson:c.sson,sessionKey:c.session_key,sessionSecret:c.session_secret});
  const result=JSON.parse(JSON.stringify(await parser[spec.op](...spec.args)));
  assert.deepEqual(result,spec.expected,spec.name);
  assert.equal(offset,steps.length,'Unconsumed memory responses: '+spec.name);
  if(spec.sleeps)assert.deepEqual(sleeps,spec.sleeps);
  const deletes=trace.filter(r=>r.url.includes('/batchDeleteFile.action'));
  if(spec.deletes!==undefined)assert.equal(deletes.length,spec.deletes);
  for(const r of trace){
   const url=new URL(r.url);
   assert.ok(['cloud.189.cn','api.cloud.189.cn'].includes(url.hostname));
   if(url.hostname==='cloud.189.cn'){
    assert.equal(r.headers.appkey,'600100422');
    assert.equal(r.headers.signature,createHash('md5').update('600100422'+corpus.epoch+'1').digest('hex'));
    assert.equal(r.headers.cookie,'COOKIE_LOGIN_USER=offline-user; SSON=offline-sson');
   }
   if(url.pathname.endsWith('/createBatchTask.action')){
    const form=new URLSearchParams(r.body);
    assert.equal(form.get('type'),'SHARE_SAVE');
    assert.equal(form.get('shareId'),spec.args[0]);
    assert.deepEqual(JSON.parse(form.get('taskInfos')),[{fileId:spec.args[1],fileName:spec.args[2],isFolder:0}]);
    assert.equal(form.get('targetFolderId'),spec.op==='saveAndDownload'?'-11':spec.args[3]);
   }
   if(url.pathname.endsWith('/batchDeleteFile.action')){
    // Proves the old code uses incoming SHARE file ID, not a saved-file receipt.
    assert.equal(new URLSearchParams(r.body).get('fileIdList'),spec.op==='saveAndDownload'?spec.args[1]:spec.args[0]);
   }
  }
  vectors.push({...spec,result,trace,sleeps});
 }
 const save=vectors.find(v=>v.name==='save-single').trace[0];
 const del=vectors.find(v=>v.name==='delete-single').trace[0];
 assert.equal(save.headers.signature,corpus.wire.web_signature);
 assert.equal(save.body,corpus.wire.utf8_save_body);
 assert.equal(del.headers.signature,corpus.wire.delete_signature);
 assert.equal(new URL(del.url).search.slice(1),corpus.wire.delete_suffix);
 return {...corpus,vectors};
}
if(process.argv[1]===fileURLToPath(import.meta.url)){
 const result=await generateTransferVectors();
 process.stdout.write(JSON.stringify(process.argv.includes('--check')||process.argv.includes('--summary')?
  {source_sha256:result.source_sha256,vectors:result.vectors.length,missing_methods:result.missing_methods,network:'memory-only'}:result)+'\n');
}
