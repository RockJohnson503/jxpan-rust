import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { parse } from 'acorn';
import { generate } from 'astring';

const source = readFileSync(new URL('../.scratch/baseline/_worker.js', import.meta.url));
const hash = createHash('sha256').update(source).digest('hex');
assert.equal(hash, '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118');
const readable = readFileSync(new URL('../.scratch/baseline/handleMainRequest.js', import.meta.url), 'utf8');
const fn = parse(readable, {ecmaVersion:'latest'}).body[0];
const statements = [];
for (const statement of fn.body.body) {
  if (statement.type === 'IfStatement' && generate(statement.test).startsWith('type ===')) break;
  if (statement.type === 'IfStatement' && generate(statement.test).startsWith('url.pathname ===')) continue;
  statements.push(generate(statement));
}
const evaluate = vm.runInNewContext(`(async function(request,env){${statements.join('\n')}
  return {url:targetUrl,pwd,type,sid:sidParam,auth:authParam,action,folders:fidChainParam,id:cleanFileIdParam};
})`, {URL,getConfig:()=>({}),console:{log(){}}});
const vectors = [];
const queries = [
  '', 'url=a&url=b&pwd=&pwd=p', 'url=%E4%B8%AD%E6%96%87%20%2B%26&pwd=a%2Bb',
  'url=a&id=first&d=alias&sid=s&type=down', 'url=a&d=f%3Fwebpage%3Dsynthetic',
  'fid=r&fid1=r&fid2=a&fid20=z&fid21=ignored', 'url=%FF%FE%ZZ&pwd=+',
  'url=q&pwd=q&type=json&id=q&fid=q&auth=q',
];
const forms = [
  [], [['url','p'],['pwd','p']], [['url','first'],['url','']],
  [['id',''],['d','f']], [['fid','r'],['fid1','r'],['fid2','b']],
  [['url',''],['pwd',''],['type',''],['auth','']],
];
for (const query of queries) for (const form of forms) for (const method of ['GET','POST']) {
  const request = {url:`http://127.0.0.1/?${query}`,method,async formData(){return form;}};
  vectors.push({query,form,method,expected:await evaluate(request,{})});
}
console.log(JSON.stringify({source_sha256:hash,vectors}));
