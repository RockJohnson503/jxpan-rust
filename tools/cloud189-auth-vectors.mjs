// Offline differential oracle: exact frozen deployment functions, synthetic data.
// No credentials file, deployment writes, or real network access.
import {readFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import vm from 'node:vm';
import {parse} from 'acorn';
import {fileURLToPath} from 'node:url';
const base=new URL('../.scratch/baseline/',import.meta.url);
const sha='47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const epoch=1700000000123;
const step=(text,status=200,cookies=[],location=null)=>({text:typeof text==='string'?text:JSON.stringify(text),status,cookies,location});
const qr={qrUrl:'https://open.e.189.cn/offline/qr?code=synthetic',encryuuid:'offline-encrypted',lt:'1234567890ABCDEF',reqId:'offline-reqid-1234',paramId:'密 码&=',loginPageUrl:'https://open.e.189.cn/login?lt=123',openEDomainCookies:'OPEN=synthetic',cloud189DomainCookies:'CLOUD=synthetic'};
const info={session_key:'old-key',session_secret:'old-secret',app_access_token:'offline-app',open_access_token:'old-open',access_token:'old-open',refresh_token:'unused-refresh',cookie_login_user:'offline-user',sson:'offline-sson',login_time:100,expires_at:200,custom:'preserved'};
const session={res_code:0,sessionKey:'new-key',sessionSecret:'new-secret',accessToken:'offline-app',refreshToken:'offline-refresh'};
const html='lt = "1234567890ABCDEF"; reqId = "offline-reqid-1234"; paramId = "密 码&=";';
const corpus=[
 {name:'qr-domain-cookies',op:'init',steps:[step('',302,['CLOUD=synthetic; Path=/'],'https://open.e.189.cn/login'),step(html,200,['OPEN=synthetic; Path=/']),step({uuid:qr.qrUrl,encryuuid:qr.encryuuid},200,['OPEN=updated; Path=/','DELETED=discard; Max-Age=0'])]},
 {name:'qr-cookie-replace-expiry-padding',op:'init',steps:[step(html,200,['PAD=abc==; Path=/','OLD=x; Expires=Wed, 01 Jan 2020 00:00:00 GMT','BAD=; Path=/']),step({uuid:qr.qrUrl},200,['PAD=xyz==; Path=/'])]},
 {name:'qr-cookie-js-object-order',op:'init',steps:[step(html,200,['10=a','2=b','__proto__=ignored','01=c']),step({uuid:qr.qrUrl},200,['2=updated'])]},
 {name:'qr-relative-redirect',op:'init',steps:[step('',302,['A=one'],'/login'),step(html,200,['A=two']),step({uuid:qr.qrUrl})]},
 {name:'qr-alt-parameters',op:'init',steps:[step('no params'),step('',302,['OPEN=synthetic'],'/login?lt=ALT123&reqId=alt-request-id'),step('ignored',200,['OPEN=updated']),step({uuid:qr.qrUrl,encryuuid:qr.encryuuid})]},
 {name:'qr-random-reqid',op:'init',steps:[step('no params'),step('no location'),step({uuid:qr.qrUrl})]},
 {name:'qr-five-hop-limit',op:'init',steps:[...Array.from({length:5},()=>step('',302,[],'/again')),step('no location'),step({uuid:qr.qrUrl})]},
 {name:'qr-transport-fallback',op:'init',steps:[{fail:true},step('no location'),step({uuid:qr.qrUrl})]},
 {name:'qr-missing-uuid',op:'init',steps:[step(html),step({result:-1})],error:'InvalidQr'},
 {name:'complete-open-login',op:'complete',input:qr,steps:[step({status:0,redirectUrl:'https://cloud.189.cn/callback',_pollCookies:'POLL=synthetic'}),step(session,200,['COOKIE_LOGIN_USER=user==','SSON=sson==']),step({accessToken:'new-open'})]},
 {name:'complete-app-only-login',op:'complete',input:qr,steps:[step({status:0,redirectUrl:'https://cloud.189.cn/callback'}),step(session,200,['COOKIE_LOGIN_USER=user']),step({errorCode:'InvalidAccessToken'})]},
 {name:'complete-cookie-fallback',op:'complete',input:qr,steps:[step({status:-11002,redirectUrl:'https://cloud.189.cn/callback'}),step(session),step('',200,['COOKIE_LOGIN_USER=user==','SSON=sson==']),step({accessToken:'new-open'})]},
 {name:'complete-exchange-failure',op:'complete',input:qr,steps:[step({status:0,redirectUrl:'https://cloud.189.cn/callback'}),step({res_code:1,res_message:'offline failure'})]},
 {name:'complete-waiting',op:'complete',input:qr,steps:[step({status:-106})]},
 {name:'poll-full-uuid-no-cookie',op:'poll',input:qr,steps:[step({status:-106})]},
 {name:'poll-blank-fields',op:'poll',input:{},steps:[step({status:-11002})]},
 {name:'poll-large-integer-safe-parser',op:'poll',input:qr,steps:[step('{"status":-1,"accountId":123456789012345678}')]},
 {name:'poll-json-failure',op:'poll',input:qr,steps:[step('not json')],error:'InvalidJson'},
 {name:'exchange-session-cookie-truncation',op:'exchange',input:'https://cloud.189.cn/callback?ticket=synthetic&x=1',steps:[step(session,200,['COOKIE_LOGIN_USER=user==; Path=/','SSON=sson==; Path=/'])]},
 {name:'exchange-session-failure',op:'exchange',input:'https://cloud.189.cn/callback',steps:[step({res_code:1,res_message:'offline session failure'})]},
 {name:'exchange-string-code-rejected',op:'exchange',input:'https://cloud.189.cn/callback',steps:[step({...session,res_code:'0'})]},
 {name:'open-token-cookie-header',op:'open',input:{key:'key',cookies:'COOKIE_LOGIN_USER=synthetic; SSON=synthetic'},steps:[step({accessToken:'offline-open'})]},
 {name:'open-token-raw-query',op:'open',input:{key:'key&x=1#fragment',cookies:''},steps:[step({accessToken:'offline-open'})]},
 {name:'open-token-error-precedence',op:'open',input:{key:'key',cookies:''},steps:[step({errorCode:'InvalidAccessToken',res_message:'unused'})]},
 {name:'open-token-default-error',op:'open',input:{key:'key',cookies:''},steps:[step({})]},
 {name:'cookie-direct',op:'cookie',input:'https://cloud.189.cn/callback',steps:[step('',200,['COOKIE_LOGIN_USER=user==','SSON=sson=='])]},
 {name:'cookie-way1-last-wins',op:'cookie',input:'https://cloud.189.cn/callback',steps:[step('',302,['COOKIE_LOGIN_USER=first','SSON=one'],'https://cloud.189.cn/next'),step('',200,['COOKIE_LOGIN_USER=last==','SSON=two=='])]},
 {name:'cookie-way2-first-wins',op:'cookie',input:'https://cloud.189.cn/callback',steps:[step(''),step('',302,['SSON=first'],'/callback2'),step('',200,['COOKIE_LOGIN_USER=user==','SSON=second'])]},
 {name:'cookie-no-user-discards-sson',op:'cookie',input:'https://cloud.189.cn/callback',steps:[step('',200,['SSON=one']),step('',200,['SSON=two'])]},
 {name:'refresh-atomic-success',op:'refresh',input:info,steps:[step(session),step({accessToken:'new-open'})]},
 {name:'refresh-session-failed',op:'refresh',input:info,steps:[step({...session,res_code:1})]},
 {name:'refresh-http-failed',op:'refresh',input:info,steps:[step(session,403)]},
 {name:'refresh-secret-missing',op:'refresh',input:info,steps:[step({res_code:0,sessionKey:'new'})]},
 {name:'refresh-open-failed-no-partial-write',op:'refresh',input:info,steps:[step(session),step({errorCode:'InvalidAccessToken'})]},
 {name:'refresh-requires-app-not-refresh-token',op:'refresh',input:{refresh_token:'unused'},steps:[]},
 ...[null,{},info,{...info,expires_at:epoch/1000|0},{...info,expires_at:(epoch/1000|0)+1},{...info,expires_at:'9999999999'},{...info,expires_at:0}].map((input,i)=>({name:'status-'+i,op:'status',input,steps:[]})),
 ...[{status:-106},{status:-1},{status:-11002},{status:-11001},{result:-20099},{status:7,msg:'页面过期'}, {status:-106,msg:'页面过期'}, {status:-999},{},{status:'0'},{status:0},{status:-11001,redirectUrl:'https://cloud.189.cn/callback'}].map((input,i)=>({name:'state-'+i,op:'state',input,steps:[]})),
];
function walk(n,visit){if(!n||typeof n!=='object')return;visit(n);for(const [k,v] of Object.entries(n)){if(k==='start'||k==='end')continue;if(Array.isArray(v))v.forEach(x=>walk(x,visit));else if(v&&typeof v==='object')walk(v,visit);}}
export async function generateAuthVectors(){
 const raw=await readFile(new URL('_worker.js',base));
 if(createHash('sha256').update(raw).digest('hex')!==sha)throw Error('Deployment SHA mismatch');
 const readable=await readFile(new URL('readable.js',base),'utf8');
 const klass=await readFile(new URL('Cloud189Parser.js',base),'utf8');
 if(!readable.includes(klass.trim()))throw Error('Extracted class differs');
 const ast=parse(readable,{ecmaVersion:'latest',sourceType:'module'});
 const funcs=ast.body.filter(n=>n.type==='FunctionDeclaration'&&['jxpanParseJsonSafe','jxpanCloud189RefreshOnDemand'].includes(n.id.name));
 if(funcs.length!==2)throw Error('Missing original functions');
 const branches={};
 walk(ast,n=>{if(n.type==='IfStatement'&&n.test.type==='BinaryExpression'&&n.test.left.type==='Identifier'&&n.test.left.name==='action'&&['cloud189_status','cloud189_check'].includes(n.test.right.value)){
  if(branches[n.test.right.value])throw Error('Ambiguous original branch');
  branches[n.test.right.value]=readable.slice(n.start,n.end);
 }});
 if(Object.keys(branches).length!==2)throw Error('Missing original auth branches');
 const source=klass+'\n'+funcs.map(n=>readable.slice(n.start,n.end)).join('\n');
 const vectors=[];
 for(const spec of corpus){
  const trace=[];let offset=0;let writes=[];let violation=null;
  class FixedDate extends Date{constructor(...a){super(...(a.length?a:[epoch]));}static now(){return epoch;}}
  const context=vm.createContext({URL,URLSearchParams,Response,TextEncoder,Date:FixedDate,
   Math:Object.assign(Object.create(Math),{random:()=>0.25}),console:{log(){},error(){}},
   storeGet:async()=>spec.op==='status'?JSON.stringify(spec.input):['state','complete'].includes(spec.op)?JSON.stringify(qr):JSON.stringify(spec.input),
   storePutRaw:async(_env,_key,value)=>{writes.push(JSON.parse(value));},
   storePut:async(_env,_key,value)=>{writes.push(JSON.parse(value));},storeDelete:async()=>{},clearAllCookieCache:()=>{},memoryStore:new Map(),
   fetch:async(url,options={})=>{
    const h=Object.fromEntries(Object.entries(options.headers||{}).map(([k,v])=>[k.toLowerCase(),String(v)]));
    trace.push({url:String(url),method:options.method||'GET',headers:h,body:options.body??null,redirect:options.redirect||'follow'});
    const s=spec.steps[offset++];if(!s){violation=Error('Unexpected offline fetch');throw violation;}
    if(s.fail)throw Error('offline transport failure');
    return {status:s.status,headers:{getSetCookie:()=>s.cookies,get:k=>k.toLowerCase()==='location'?s.location:null},text:async()=>s.text,json:async()=>JSON.parse(s.text)};
   }});
  const exported=vm.runInContext(source+'\n({Parser:Cloud189Parser,refresh:jxpanCloud189RefreshOnDemand});',context,{timeout:3000});
  exported.Parser.prototype.generateUUID=()=> '44444444-4444-4444-8444-444444444444';
  let expected;
  if(spec.op==='init')expected=await exported.Parser.initQRLogin();
  else if(spec.op==='poll')expected=await exported.Parser.checkQRStatus(spec.input,'ignored-device');
  else if(spec.op==='exchange')expected=await exported.Parser.exchangeSession(spec.input);
  else if(spec.op==='open')expected=await exported.Parser.getOpenAccessToken(spec.input.key,spec.input.cookies);
  else if(spec.op==='cookie')expected=await exported.Parser.getCookieLoginUser(spec.input);
  else if(spec.op==='refresh')expected=await exported.refresh({jxpan:{}});
  else if(spec.op==='complete'){
   context.env={jxpan:{}};context.action='cloud189_check';context.url=new URL('https://offline.invalid/?device_id=synthetic');
   const resp=await vm.runInContext('(async()=>{'+branches.cloud189_check+'})()',context);
   expected=await resp.json();
  }
  else if(spec.op==='status'){
   context.input=spec.input;context.env={jxpan:spec.input?{}:null};context.action='cloud189_status';
   const resp=await vm.runInContext('(async()=>{'+branches.cloud189_status+'})()',context);
   expected=await resp.json();
  }else if(spec.op==='state'){
   exported.Parser.checkQRStatus=async()=>spec.input;
   exported.Parser.exchangeSession=async()=>({success:false,error:'offline-exchange-sentinel'});
   context.env={jxpan:{}};context.action='cloud189_check';context.url=new URL('https://offline.invalid/?device_id=synthetic');
   const resp=await vm.runInContext('(async()=>{'+branches.cloud189_check+'})()',context);
   expected=await resp.json();
   if(expected.msg==='Session交换失败: offline-exchange-sentinel')expected={state:'exchange'};
  }
  if(violation)throw violation;
  if(offset!==spec.steps.length)throw Error('Unconsumed steps: '+spec.name);
  if(spec.op==='refresh'&&writes.length!==(expected?1:0))throw Error('Nonatomic original renewal');
  vectors.push({...spec,expected,trace,writes});
 }
 return {source_sha256:sha,function_sha256:createHash('sha256').update(source+Object.values(branches).join('\n')).digest('hex'),vectors};
}
if(process.argv[1]===fileURLToPath(import.meta.url)){
 const result=await generateAuthVectors();
 process.stdout.write(JSON.stringify(process.argv.includes('--summary')?{source_sha256:sha,vectors:result.vectors.length}:result)+'\n');
}
