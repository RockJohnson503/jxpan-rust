import { chmodSync, readFileSync, writeFileSync } from 'node:fs';
import { originalCipher, privateDir, readonlyRows, decodeRow } from './deployed-kv.mjs';

// The public history endpoint filters only after paginating recent rows. Read a
// bounded older slice to obtain one Cloud189 sample, without exporting its data.
const rows = readonlyRows(process.argv[2] || '',
  "SELECT key, value FROM kv_store WHERE key LIKE 'parse_record_%' ORDER BY key ASC LIMIT 20");
const cipher = originalCipher();
const path = `${privateDir}live-cases.json`;
const cases = JSON.parse(readFileSync(path, 'utf8'));
for (const row of rows) {
  if (cases.some(input => input.provider === 'cloud189')) break;
  const record = decodeRow(cipher, row);
  if (typeof record?.url !== 'string') continue;
  let link = record.url, password = record.pwd || '';
  let parsed;
  for (let depth = 0; depth < 4; depth++) {
    try { parsed = new URL(link); } catch { parsed = null; break; }
    if (!parsed.searchParams.has('url')) break;
    password = parsed.searchParams.get('pwd') || password;
    link = parsed.searchParams.get('url');
  }
  if (parsed?.hostname === 'cloud.189.cn' || parsed?.hostname.endsWith('.cloud.189.cn')) {
    cases.push({provider:'cloud189', url:link, pwd:password});
  }
}
writeFileSync(path, JSON.stringify(cases), {mode:0o600});
chmodSync(path, 0o600);
console.log(JSON.stringify({case_providers:cases.map(input=>input.provider), private_files:true}));
