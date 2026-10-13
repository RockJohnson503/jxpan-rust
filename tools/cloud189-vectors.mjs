// Offline oracle: executes the exact extracted deployed class in an isolated VM.
// No credentials, storage, login/save/delete methods, or real fetch are used.
import { readFile } from 'node:fs/promises';
import { createHash, webcrypto } from 'node:crypto';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

const baseline = new URL('../.scratch/baseline/', import.meta.url);
const expectedSourceHash = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';

export async function generateVectors() {
  const raw = await readFile(new URL('_worker.js', baseline));
  const sourceHash = createHash('sha256').update(raw).digest('hex');
  if (sourceHash !== expectedSourceHash) throw new Error('Deployed worker SHA-256 mismatch');
  const source = await readFile(new URL('Cloud189Parser.js', baseline), 'utf8');
  const readable = await readFile(new URL('readable.js', baseline), 'utf8');
  if (!readable.includes(source.trim())) throw new Error('Extracted class does not match readable deployment');

  const epoch = 1704164645000; // Tue, 02 Jan 2024 03:04:05 GMT
  class FixedDate extends Date {
    constructor(...args) { super(...(args.length ? args : [epoch])); }
    static now() { return epoch; }
  }
  const requests = [];
  const signingCalls = [];
  const subtle = {
    digest: async (algorithm, data) => {
      if (String(algorithm).toUpperCase() === 'MD5') {
        const bytes = createHash('md5').update(Buffer.from(data)).digest();
        return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
      }
      return webcrypto.subtle.digest(algorithm, data);
    },
    importKey: (...args) => webcrypto.subtle.importKey(...args),
    sign: (...args) => webcrypto.subtle.sign(...args),
  };
  const context = vm.createContext({
    TextEncoder, URL, URLSearchParams, Date: FixedDate,
    Math: Object.assign(Object.create(Math), { random: () => 0.125 }),
    crypto: { subtle },
    console: { log() {}, error() {} },
    jxpanParseJsonSafe: response => response.json(),
    fetch: async (input, options = {}) => {
      const url = new URL(input);
      const allowed = [
        '/open/file/getFileDownloadUrl.action', '/getFileDownloadUrl.action',
        '/open/share/listShareDir.action', '/api/portal/v2/getRootDirectory.action',
      ];
      if (!allowed.includes(url.pathname)) throw new Error('Disallowed offline fetch');
      requests.push({ url: String(input), method: options.method || 'GET', headers: options.headers });
      const body = url.pathname.includes('getRootDirectory')
        ? { res_code: 0, fileId: '-11' }
        : url.pathname.includes('listShareDir')
          ? { res_code: 0, fileListAO: { folderList: [], fileList: [] } }
          : { fileDownloadUrl: 'https://offline.invalid/file' };
      return { status: 200, headers: { getSetCookie: () => [], get: () => null }, json: async () => body };
    },
  });
  const Parser = vm.runInContext(source + '\nCloud189Parser;', context, { timeout: 1000 });
  const parser = new Parser({
    openAccessToken: 'offline-token&+=天翼', sessionKey: 'offline-session&+=',
    sessionSecret: 'offline-secret-中文', cookieLoginUser: 'offline-cookie',
  });
  const inputs = [
    '', 'cloud.189.cn/t/AbC123', 'http://cloud.189.cn/t/AbC123?pwd=9999',
    'https://cloud.189.cn/web/share?code=AbC123',
    'https://cloud.189.cn/web/share?code=FIRST1&code=LAST22',
    'https://cloud.189.cn/web/share?xcode=Shadow1',
    'https://cloud.189.cn/web/share?CODE=Case123',
    'https://cloud.189.cn/web/share?code=abc_def',
    'https://cloud.189.cn/web/share?code=a-b',
    'https://h5.cloud.189.cn/t/H51234', 'm.cloud.189.cn/t/M12345',
    'https://cloud.189.cn/s/A_b-C123', 'https://h5.cloud.189.cn/s/A_b-C123',
    'https://cloud.189.cn/t/abc_def', 'https://cloud.189.cn/t/abc-def',
    'HTTPS://CLOUD.189.CN/T/ABC123', ' https://cloud.189.cn/t/ABC123 ',
    '分享：https://cloud.189.cn/t/ABC123 提取码：1234',
    'https%3A%2F%2Fcloud.189.cn%2Ft%2FEncoded123',
    'https://cloud.189.cn/t/%41bc123', 'https://cloud.189.cn/t/%2541bc123',
    'https://cloud.189.cn/t/%41bc123?bad=%ZZ',
    'https://cloud.189.cn/t/%41bc123?bad=%ED%A0%80',
    'https://cloud.189.cn/t/%41bc123?bad=%C0%AF',
    'https://cloud.189.cn/t/%41bc123?bad=%',
    'https://cloud.189.cn.evil.invalid/t/ABC123',
    'https://notcloud.189.cn/t/ABC123',
    'https://offline.invalid/?redirect=https://cloud.189.cn/t/ABC123',
    'https://offline.invalid/?code=ABC123', 'offline.invalid/?code=abcde',
    'https://offline.invalid/?code=', 'https://offline.invalid/?code=&code=ABC123',
    'https://offline.invalid/?Code=ABC123', 'https://offline.invalid/?code=abc+def',
    'https://offline.invalid/?code=a%252Bb%252Bc',
    'https://offline.invalid/?code=中文中文中文',
    'https://offline.invalid/?code=😀😀😀', 'https://offline.invalid/?code=😀😀a',
    'https://offline.invalid/?code=%FFabcdef',
    'https://cloud.189.cn/web/share?x=1\ncode=ABC123',
    'https://cloud.189.cn/web/share?x=1\rcode=ABC123',
    'https://cloud.189.cn/web/share?x=1\u2028code=ABC123',
    'https://cloud.189.cn/web/share?x=1\u2029code=ABC123',
    'https://cloud.189.cn/t/ſabc123', 'https://cloud.189.cn/t/Kabc123',
    'https://cloud.189.cn/web/share?code=ABC123#code=LAST22',
    'https://cloud.189.cn/t/', 'https://cloud.189.cn/s/',
    'https://cloud.189.cn/other?code=a_b-c6',
  ];
  const shares = inputs.map(input => ({ input, expected: parser.extractShareInfo(input) }));
  const hashInputs = ['', 'a', 'abc', '中文😀', 'a\0b', '600100422' + epoch + '1', ' leading and trailing '];
  // Fixed extra samples exercise leading zero nibbles without handwritten expectations.
  for (let i = 0; i < 128; i++) hashInputs.push('offline-md5-' + i);
  const hashes = [];
  for (const input of hashInputs) hashes.push({ input, md5: await parser.md5(input), javaMd5: await parser.javaMd5(input) });
  const hmacs = [];
  for (const [key, message] of [['key', 'abc'], ['中文密钥', '天翼😀'], ['a\0b', ''], ['x'.repeat(100), 'long key'], ['', 'abc']]) {
    try { hmacs.push({ key, message, output: await parser.hmacSha1(key, message), rejected: false }); }
    catch { hmacs.push({ key, message, rejected: true }); }
  }
  // Capture the original caller's text as well as the original cryptographic result.
  for (const name of ['md5', 'javaMd5', 'hmacSha1']) {
    const original = parser[name];
    parser[name] = async function (...args) {
      const output = await original.apply(this, args);
      signingCalls.push({ algorithm: name, args, output });
      return output;
    };
  }
  const fileId = '100&+=中文', shareId = '200&+=';
  await parser.getDownloadUrlByOpenAPI(fileId, shareId);
  await parser.getDownloadUrlByAppSign(fileId, shareId);
  await parser.getDownloadUrlByAppSign(fileId, '');
  await parser.listShareDirOpenAPI(shareId, fileId, 'offline-password', '2');
  await parser.listShareDirAppSign(shareId, fileId, 'offline-password', '2');
  await parser.getRootDirectory('offline-web-session');
  if (signingCalls.length !== 6 || requests.length !== 6) throw new Error('Original request methods did not reach signing paths');
  return {
    sourceHash, classHash: createHash('sha256').update(source).digest('hex'),
    fixed: { timestamp: String(epoch), date: new FixedDate().toUTCString(),
      token: parser.openAccessToken, sessionKey: parser.sessionKey, secret: parser.sessionSecret, fileId, shareId },
    shares, hashes, hmacs, signingCalls, requests,
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try { process.stdout.write(JSON.stringify(await generateVectors()) + '\n'); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
