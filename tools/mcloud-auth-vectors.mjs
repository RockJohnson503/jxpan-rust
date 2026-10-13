#!/usr/bin/env node
// Pinned original QR route blocks + pure helpers + refresh function. Offline only.
import {readFileSync,writeFileSync} from 'node:fs';
import {createHash,webcrypto} from 'node:crypto';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const base=new URL('../.scratch/baseline/',import.meta.url);
const read=n=>readFileSync(new URL(n,base),'utf8');
const sha=s=>createHash('sha256').update(s).digest('hex');
const deploymentSha256='47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
assert.equal(sha(read('_worker.js')),deploymentSha256);
const source=read('readable.js');
assert.equal(sha(source),'7196db8435e214bc0bd46f60534a247652cf426185c272a9bd9bd0c3b15eaa79');
function span(a,b){const x=source.indexOf(a),y=source.indexOf(b,x+a.length);assert(x>=0&&y>=0);return source.slice(x,y);}
const timestamp='2026-05-14 12:34:56',now=1778733296000;
const uuids=['00112233-4455-4677-8899-aabbccddeeff','10213243-5465-4767-8899-aabbccddeeff','20314253-6475-4867-8899-aabbccddeeff'];
const rand=n=>Array.from({length:n},(_,i)=>(i*17)%256);
let calls,writes,stored,response,transportError,uuidIndex;
const allowed=new Set(['https://yun.139.com/orchestration/auth-rebuild/key/v1.0/getRsaPublicKey','https://yun.139.com/orchestration/auth-rebuild/permission/v1.0/login','https://aas.caiyun.feixin.10086.cn/tellin/authTokenRefresh.do']);
const context=vm.createContext({
 console:Object.freeze({log(){},warn(){},error(){}}),atob,btoa,TextEncoder,TextDecoder,Response,
 crypto:Object.freeze({subtle:webcrypto.subtle,getRandomValues(a){a.set(rand(a.length));return a;},randomUUID(){assert(uuidIndex<3);return uuids[uuidIndex++];}}),
 async fetch(url,opt){assert(allowed.has(url),'unapproved endpoint');assert.equal(calls.length,0,'unexpected retry');calls.push({url,method:opt.method,headers:opt.headers,body:opt.body});if(transportError)throw Error(transportError);return {ok:response.ok??true,async json(){if(response.parseError)throw Error(response.parseError);return response.json;},async text(){return response.text;}};},
 async storeGet(env,key){assert(['mcloud_session','mcloud_login_default'].includes(key));return stored;},
 async storePut(env,key,value,opt){assert.equal(key,'mcloud_session');writes.push({key,value:JSON.parse(value),options:opt});},
 async storePutRaw(env,key,value,opt){assert.equal(key,'mcloud_login_default');writes.push({key,value:JSON.parse(value),options:opt});},
 encryptToKV(){return null;},
},{codeGeneration:{strings:false,wasm:false}});
vm.runInContext(span('function __p_jRSW_cff_hash(','const cookieCache =')+span('function md5(input)','function generateCacheKey(')+span('function jxpanMcloudXmlEscape(','class MobileCloudParser {')+`
const NativeDate=Date;Date=class extends NativeDate {constructor(){super(${now});}static now(){return ${now};}getFullYear(){return 2026;}getMonth(){return 4;}getDate(){return 14;}getHours(){return 12;}getMinutes(){return 34;}getSeconds(){return 56;}};
globalThis.route=async(action,env)=>{${span('    if (action === "mcloud_qrcode") {','    if (action === "mcloud_qr_save") {')}};
globalThis.ecb=aesEcbEncrypt;globalThis.rsa=rsaPkcs1v15Encrypt;globalThis.parseKey=parseRsaPublicKey;globalThis.refresh=jxpanPrepareMcloudCredential;`,context,{timeout:5000});
function reset(s,r,err=null){calls=[];writes=[];stored=s;response=r;transportError=err;uuidIndex=0;}
const ecb=[];for(const key of ['2031425364754867','k'.repeat(24),'k'.repeat(32)])for(const text of [...Array.from({length:66},(_,i)=>'x'.repeat(i)),'中文😀','\ufeff\0\né'])ecb.push({key,text,expected:await context.ecb(key,text)});
const rsa=[];for(const size of [32,64,128,256])for(const exp of [1,3,65537]){
 const n=Array.from({length:size},(_,i)=>i===0?0xd3:(i*29+17)%256);n[size-1]|=1;
 const message='2031425364754867';const padding=rand(size-19);
 const nb=BigInt('0x'+Buffer.from(n).toString('hex'));
 rsa.push({n,e:Array.from(Buffer.from(exp.toString(16).padStart(exp.toString(16).length+(exp.toString(16).length%2),'0'),'hex')),message,padding,expected:context.rsa(nb,BigInt(exp),message)});
}
const parse=[];for(const der of [
 [0x30,0,0x30,0,6,0,5,0,3,0,0x30,0,2,3,0x11,0x22,0x33,2,1,3],
 [0x30,0,0x30,0,6,0,5,0,3,0,0x30,0,2,3,0,0x22,0x33,2,1,3],
]){const encoded=Buffer.from(der).toString('base64');try{const k=context.parseKey(encoded);parse.push({encoded,n:k.n.toString(16),e:k.e.toString(16)});}catch{parse.push({encoded,error:true});}}
const qrcode=[];
for(const json of [{success:false},{success:true,data:{}},{},null]){reset(null,{json});const result=await context.route('mcloud_qrcode',{jxpan:{}});qrcode.push({response:json,expected:await result.json(),calls,writes});}
reset(null,{},'synthetic network error');qrcode.push({error:'synthetic network error',expected:await (await context.route('mcloud_qrcode',{jxpan:{}})).json(),calls,writes});
// Nonzero-leading integer and exponent 3 make an executable synthetic DER probe.
const modulus=Array.from({length:64},(_,i)=>i===0?0xd3:(i*29+17)%256);modulus[63]|=1;
const publicKey=Buffer.from([0x30,0,0x30,0,6,0,5,0,3,0,0x30,0,2,64,...modulus,2,1,3]).toString('base64');
reset(null,{json:{success:true,data:{publicKey}}});qrcode.push({response:response.json,expected:await(await context.route('mcloud_qrcode',{jxpan:{}})).json(),calls,writes});
const session=qrcode.at(-1).writes[0].value;
const poll=[];async function addPoll(json,options={}){reset(options.missing?null:JSON.stringify(session),{json,parseError:options.parseError},options.error);const env=options.noStorage?{}:{jxpan:{}};const result=await context.route('mcloud_qr_poll',env);poll.push({response:json,options,expected:await result.json(),calls,writes});}
for(const resultCode of ['0',0,'200059541','200059548','200059542','200059549',200059548,'unknown',''])for(const success of [true,false])await addPoll({success,data:{result:{resultCode},authToken:'synthetic:a|b',encryptAccount:btoa('synthetic')}});
for(const data of [{result:{resultCode:'0'},token:'t'}, {result:{resultCode:'0'},token:'t',encryptAccount:'!!!',simplifyAccount:'fallback'}, {result:{resultCode:'0'},token:7,encryptAccount:btoa('é')}, {result:{resultCode:'0'},authToken:'',token:'fallback',encryptAccount:''}, {result:{resultCode:'0'},token:'t',encryptAccount:'!!!',simplifyAccount:'中'}, {}, null])await addPoll({success:true,data});
await addPoll(null);await addPoll({});await addPoll({}, {missing:true});await addPoll({}, {noStorage:true});await addPoll({}, {error:'synthetic network error'});await addPoll({}, {parseError:'synthetic invalid JSON'});
const refresh=[];const auth=(expiration,account='synthetic',token='a:b|c|d')=>'Basic '+btoa(`pc:${account}:${token}|${expiration}`);
async function addRefresh(storedValue,reply={ok:true,text:'<root><return>0</return><token> new:token|a|b|9999999999999 </token></root>'},error=null){reset(storedValue,reply,error);const config={mcloud:{authorization:'configured-sentinel'}};const expected=await context.refresh({jxpan:{}},config);refresh.push({stored:storedValue,response:reply,error,expected,authorization:config.mcloud.authorization,calls,writes});}
for(const expiry of [now+15*86400000-1,now+15*86400000,now+15*86400000+1,now-1,0,-1,'NaN','Infinity','0x10','','  '])await addRefresh({authorization:auth(expiry)});
for(const failed of [now-3600000-1,now-3600000,now-3600000+1,now+1,'bad',0])for(const managed of [true,false])await addRefresh({authorization:auth(now),mcloud_auto_refresh_failed_at:failed,jxpan_auto_refresh_managed:managed});
for(const text of ['<return>0</return><token>t</token>','<RETURN> 0 </RETURN><TOKEN>\n t:é </TOKEN>','<return>1</return><token>t</token>','<return>0</return><token></token>','<return>0</return><token>   </token>','<return>0</return><token>中</token>','<return>0</return><token>a&amp;b</token>','not XML','<return>\u00850\u0085</return><token>t</token>'])await addRefresh({authorization:auth(now)}, {ok:true,text});
for(const space of ['\ufeff','\u2028','\u0085']) for(const token of [space,space+'t'+space]) await addRefresh({authorization:auth(now)}, {ok:true,text:'<return>0</return><token>'+token+'</token>'});
await addRefresh({authorization:auth(now)},{ok:false,text:'<return>0</return><token>t</token>'});await addRefresh({authorization:auth(now)},{},'synthetic network error');
for(const value of [null,{},[],false,5,'', '!!!',auth(now),{auth:auth(now)}, {authorization:'Basic '+btoa('pc::t|a|b|'+now),phone:'fallback'},{authorization:auth(now,'<&\"\'','<&\"\':b|c|d')}, {authorization:1,auth:auth(now)}, {authorization:' Basic '+btoa('pc:synthetic:t|a|b|'+now)}, {authorization:auth(now),phone:7}])await addRefresh(value);
const result={deploymentSha256,oracle:'unchanged pinned route blocks/helpers/refresh; strict memory fetch/storage, injected entropy/time; synthetic credentials only',timestamp,now,uuids,random:rand(16),ecb,rsa,parse,qrcode,poll,refresh};
if(process.argv.includes('--output'))writeFileSync(new URL('../tests/fixtures/mcloud-auth.json',import.meta.url),JSON.stringify(result)+'\n');
if(process.argv.includes('--summary') || process.argv.includes('--output')) console.log(JSON.stringify({deploymentSha256,ecb:ecb.length,rsa:rsa.length,parse:parse.length,qrcode:qrcode.length,poll:poll.length,refresh:refresh.length,vectorSha256:sha(JSON.stringify(result))},null,2));
else console.log(JSON.stringify(result));
