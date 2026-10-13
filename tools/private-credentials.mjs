import { chmodSync, writeFileSync } from 'node:fs';
import { originalCipher, privateDir, readonlyRows, decodeRow } from './deployed-kv.mjs';

// Read only these two provider rows. Never export the database or other accounts.
const rows = readonlyRows(process.argv[2] || '',
  "SELECT key, value FROM kv_store WHERE key IN ('c189_login_default','mcloud_login_default')");
const cipher = originalCipher();
const credentials = {};
for (const row of rows) {
  const provider = row.key === 'c189_login_default' ? 'cloud189'
    : row.key === 'mcloud_login_default' ? 'mcloud' : null;
  if (provider) credentials[provider] = decodeRow(cipher, row);
}
const path = `${privateDir}credentials.json`;
writeFileSync(path, JSON.stringify(credentials), { mode: 0o600 });
chmodSync(path, 0o600);
console.log(JSON.stringify({ providers: Object.keys(credentials), private_file: true, values_redacted: true }));
