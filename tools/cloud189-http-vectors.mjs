// Executes the deployed methods, not a reimplementation of their HTTP contract.
// All credentials are synthetic; strict in-memory fetch can never touch a network.
import { readFile } from 'node:fs/promises';
import { createHash, webcrypto } from 'node:crypto';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';
const baseline = new URL('../.scratch/baseline/', import.meta.url);
const expectedHash = '47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118';
const epoch = 1700000000123;
const credentials = {
  session_key: 'offline-session&+=', session_secret: 'offline-secret-中文',
  open_access_token: 'offline-open&+=', cookie_login_user: 'offline-login',
  share_session_cookies: 'SHARE=old; OTHER=keep',
};
const fileLists = { res_code: '0', fileListAO: {
  folderList: [{ id: 0, fileId: 'folder-fallback', name: '', fileName: '目录', fileCreateDate: 'yesterday' }],
  fileList: [ { id: 'f1', name: '视频.mp4', size: 0, fileSize: 999, isFolder: '1' },
    { fileId: 'f2', fileName: '子目录', size: null, fileSize: 25, isFolder: true } ],
} };
const ok = data => ({ body: JSON.stringify(data), cookies: [], status: 200 });
const fail = () => ok({ res_code: 403, res_message: 'offline rejection' });
const withCookies = (data, cookies) => ({ ...ok(data), cookies });
const action = (method, ...args) => ({ method, args });
const list = mode => action('list_share_dir', 'share123', 'folder456', 'a +&=中文', mode);

export async function generateHttpVectors() {
  const raw = await readFile(new URL('_worker.js', baseline));
  if (createHash('sha256').update(raw).digest('hex') !== expectedHash) throw new Error('Deployment SHA mismatch');
  const source = await readFile(new URL('Cloud189Parser.js', baseline), 'utf8');
  const readable = await readFile(new URL('readable.js', baseline), 'utf8');
  if (!readable.includes(source.trim())) throw new Error('Extracted class does not match readable deployment');
  const start = readable.indexOf('async function jxpanParseJsonSafe(resp)');
  const end = readable.indexOf('\nclass Cloud189Parser', start);
  if (start < 0 || end < 0) throw new Error('Safe JSON parser not found');
  const safeJson = readable.slice(start, end);
  const cases = [
    { name: 'share-top-bigint', credentials: { session_key: credentials.session_key },
      actions: [action('get_share_info', 'AbC123')],
      responses: [{ body: '{"res_code":0,"fileId":123456789012345678,"name":"测试","fileSize":0,"size":42,"isFolder":"1","shareId":987654321098765432}',
        cookies: ['GONE=x; Max-Age=0', 'COOKIE_LOGIN_USER=acquired==; Path=/'], status: 200 }] },
    { name: 'share-nested-non2xx', credentials: {}, actions: [action('get_share_info', 'X12345')],
      responses: [{ ...ok({ res_code: 7, fileInfoAO: { fileId: 'nested', isDirectory: true, shareId: 'sid' } }), status: 500 }] },
    { name: 'share-string-code-rejected', credentials: {}, actions: [action('get_share_info', 'X12345')],
      responses: [ok({ res_code: '0', fileId: 'f' })] },
    { name: 'password-cookie-precedence', credentials: { session_key: credentials.session_key },
      actions: [action('get_share_info', 'Cookie123'), action('check_access_code', "a +!'()~", '密 码+&='), list('1')],
      responses: [withCookies({ res_code: 0, fileId: 'root' }, ['COOKIE_LOGIN_USER=login==; Path=/']),
        withCookies({ res_code: 0, shareId: 'share123' }, ['SHARE=first; Path=/', 'SHARE=second; Path=/',
          'DEAD=x; Expires=Thu, 01 Jan 1970 00:00:00 GMT', 'OTHER=keep; Path=/']),
        fail(), fail(), fail(), ok(fileLists)] },
    { name: 'password-string-code-rejected', credentials: {},
      actions: [action('check_access_code', 'share123', '')], responses: [ok({ res_code: '0', shareId: 's' })] },
    { name: 'v1-empty-stops', credentials, actions: [list('1')], responses: [ok({ res_code: 0 })] },
    { name: 'v1-second-mode', credentials, actions: [list('1')], responses: [fail(), ok(fileLists)] },
    { name: 'v1-third-mode', credentials, actions: [list('2')], responses: [fail(), fail(), ok(fileLists)] },
    { name: 'v3-cookie-initialization', credentials: { cookie_login_user: 'offline-login' }, actions: [list('3')],
      responses: [withCookies({ res_code: 403 }, ['SHARE=first; Path=/']), fail(), fail(), ok(fileLists)] },
    { name: 'open-fallback', credentials, actions: [list('1')], responses: [fail(), fail(), fail(), fail(), ok(fileLists)] },
    { name: 'app-fallback', credentials, actions: [list('1')], responses: [fail(), fail(), fail(), fail(), fail(), ok(fileLists)] },
    { name: 'all-fallbacks-null', credentials, actions: [list('3')], responses: Array.from({length:6}, fail) },
    { name: 'no-signed-credentials', credentials: {}, actions: [list('1')], responses: Array.from({length:4}, fail) },
    { name: 'top-level-fallback-size-nullish', credentials: {}, actions: [list('1')],
      responses: [ok({ res_code: 0, fileListAO: { folderList: [], fileList: [] },
        folderList: [{ id: 'd', name: 'directory' }], fileList: [{ id: 'f', size: 0, fileSize: 88, isFolder: 1 }] })] },
    { name: 'open-download', credentials, actions: [action('download_url', 'file123', 'share123')],
      responses: [ok({ fileDownloadUrl: '//offline.invalid/file?a=1&amp;b=2&amp;c=3' })] },
    { name: 'open-error-app-dt1', credentials, actions: [action('download_url', 'file123', 'share123')],
      responses: [ok({ errorCode: 'InvalidSignature', fileDownloadUrl: '//ignored.invalid' }), ok({ fileDownloadUrl: 'http://offline.invalid/file' })] },
    { name: 'open-error-app-dt3', credentials, actions: [action('download_url', 'file123', 'share123')],
      responses: [ok({ errorCode: 'InvalidAccessToken' }), fail(), ok({ fileDownloadUrl: '//offline.invalid/small' })] },
    { name: 'app-no-share-dt3', credentials: { session_key: credentials.session_key, session_secret: credentials.session_secret },
      actions: [action('download_url', 'file123', '')], responses: [ok({ fileDownloadUrl: '//offline.invalid/own' })] },
    { name: 'download-none', credentials: {}, actions: [action('download_url', 'file123', '')], responses: [] },
    { name: 'share-raw-query-deployment', credentials: {}, actions: [action('get_share_info', 'abc +&=中文')],
      responses: [ok({ res_code: 0, fileId: 'f' })] },
    { name: 'open-raw-query-deployment', credentials, actions: [action('download_url', 'file +&=中文', 'share +&=')],
      responses: [ok({ fileDownloadUrl: '//offline.invalid/raw' })] },
    { name: 'json-failure-continues-list-fallback', credentials, actions: [list('1')],
      responses: [{ body: '<html>offline</html>', cookies: [], status: 502 }, fail(), fail(), ok(fileLists)] },
    { name: 'invalid-json-explicit-error', credentials: {}, actions: [action('get_share_info', 'AbC123')],
      responses: [{ body: '<html>offline</html>', cookies: [], status: 502 }], expected_error: 'invalid JSON' },
  ];
  const vectors = [];
  for (const spec of cases) {
    let offset = 0;
    const requests = [];
    class FixedDate extends Date {
      constructor(...args) { super(...(args.length ? args : [epoch])); }
      static now() { return epoch; }
    }
    const subtle = {
      digest: async (algorithm, data) => {
        if (String(algorithm).toUpperCase() === 'MD5') {
          const buffer = createHash('md5').update(Buffer.from(data)).digest();
          return buffer.buffer.slice(buffer.byteOffset, buffer.byteOffset + buffer.byteLength);
        }
        return webcrypto.subtle.digest(algorithm, data);
      },
      importKey: (...args) => webcrypto.subtle.importKey(...args),
      sign: (...args) => webcrypto.subtle.sign(...args),
    };
    let violation = null;
    const context = vm.createContext({ TextEncoder, URL, URLSearchParams, Date: FixedDate,
      Math: Object.assign(Object.create(Math), { random: () => 0.25 }), crypto: { subtle },
      console: { log() {}, error() {} },
      // Refresh is intentionally out of scope. No storage calls are permitted.
      jxpanCloud189RefreshOnDemand: async () => null,
      fetch: async (input, options = {}) => {
        const url = new URL(input);
        const allowed = new Set([
          'api.cloud.189.cn/open/share/getShareInfoByCodeV2.action',
          'api.cloud.189.cn/open/share/checkAccessCode.action',
          'cloud.189.cn/api/open/share/listShareDir.action',
          'api.cloud.189.cn/open/share/listShareDir.action',
          'api.cloud.189.cn/open/file/getFileDownloadUrl.action',
          'api.cloud.189.cn/getFileDownloadUrl.action',
        ]);
        if (url.protocol !== 'https:' || !allowed.has(url.hostname + url.pathname) || offset >= spec.responses.length) {
          violation = new Error(`Forbidden/unexpected offline fetch: ${spec.name}`); throw violation;
        }
        requests.push({ target: url.pathname + url.search, method: options.method || 'GET',
          headers: Object.fromEntries(Object.entries(options.headers || {}).map(([k,v]) => [k.toLowerCase(), String(v)])),
          body: String(options.body || '') });
        const response = spec.responses[offset++];
        return { status: response.status, headers: { getSetCookie: () => response.cookies,
          get: name => name.toLowerCase() === 'set-cookie' ? response.cookies.join(', ') || null : null },
          text: async () => response.body };
      },
    });
    const Parser = vm.runInContext(safeJson + '\n' + source + '\nCloud189Parser;', context, { timeout: 1000 });
    const c = spec.credentials;
    const parser = new Parser({ sessionKey: c.session_key, sessionSecret: c.session_secret,
      openAccessToken: c.open_access_token, cookieLoginUser: c.cookie_login_user });
    parser._shareSessionCookies = c.share_session_cookies || '';
    const outputs = [];
    for (const step of spec.actions) {
      if (step.method === 'get_share_info') outputs.push(await parser.getShareInfo(...step.args));
      else if (step.method === 'check_access_code') outputs.push(await parser.checkAccessCode(...step.args));
      else if (step.method === 'list_share_dir') outputs.push(await parser.listShareDir(...step.args));
      else if (step.method === 'download_url') {
        // Read-only composition of ORIGINAL Open/App methods, never parse/save.
        let result = parser.openAccessToken ? await parser.getDownloadUrlByOpenAPI(...step.args) : null;
        if (result === null && parser.sessionKey && parser.sessionSecret) result = await parser.getDownloadUrlByAppSign(...step.args);
        outputs.push(result);
      } else throw new Error('Unknown test action');
    }
    if (violation) throw violation;
    if (offset !== spec.responses.length) throw new Error(`Unused oracle responses: ${spec.name}`);
    vectors.push({ ...spec, requests, outputs });
  }
  const mergeInputs = [
    ['', ['A=1', 'B=x=y', 'A=2']], ['A=old; B=keep', ['B=new', 'C=3']], ['A=1', []],
  ];
  const context = vm.createContext({});
  const Parser = vm.runInContext(source + '\nCloud189Parser;', context);
  const merges = mergeInputs.map(([existing, incoming]) => ({ existing, incoming,
    output: Parser._mergeCookies(existing, incoming) }));
  return { source_sha256: expectedHash, vectors, merges };
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.stdout.write(JSON.stringify(await generateHttpVectors()));
}
