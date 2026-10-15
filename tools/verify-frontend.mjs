// Offline synthetic checks only: no HTTP, CDN, real QR sessions, credentials or provider calls.
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import { parse } from 'acorn';

const root = new URL('../', import.meta.url);
let checks = 0;
const ok = (condition, message) => { assert.ok(condition, message); checks++; };
const flush = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const qrSource = readFileSync(new URL('web/vendor/qrcode.js', root), 'utf8');
const qrLicense = readFileSync(new URL('web/vendor/qrcode.LICENSE', root), 'utf8');
const sha256 = value => createHash('sha256').update(value).digest('hex');
ok(sha256(qrSource) === '79ec86f82856005b1c887905cfccfcfbec3821ca61c7fd5a952faa5f778f791c', 'unmodified official qrcode-generator 2.0.4 dist artifact');
ok(sha256(qrLicense) === '3a850fa5f08101db6f40676c2786e10bd2cd5fff7b12ffdf1e0c434d4e49d90c', 'complete upstream MIT license retained');
parse(qrSource, { ecmaVersion: 'latest' }); checks++;
const qrScript = new vm.Script(qrSource, { filename: 'web/vendor/qrcode.js' });
const escapeText = text => String(text).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#039;');
const decodeAttribute = text => text.replace(/&quot;/g, '"').replace(/&#039;/g, "'").replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');
function checkGIF(url, message) {
    ok(/^data:image\/gif;base64,[A-Za-z0-9+/]+=*$/.test(url), message + ': inline GIF, no network URL');
    const bytes = Buffer.from(url.split(',')[1], 'base64');
    ok(bytes.subarray(0, 6).toString() === 'GIF87a' && bytes.at(-1) === 0x3b, message + ': actual GIF encoder output');
    ok(bytes.readUInt16LE(6) === bytes.readUInt16LE(8) && bytes.readUInt16LE(6) >= 116, message + ': square QR with quiet zone');
}
function harness(page) {
    const html = readFileSync(new URL(`web/${page}.html`, root), 'utf8');
    ok(html.includes('<script src="/assets/qrcode.js"></script>'), `${page}: same-origin QR script included`);
    ok(!/api\.qrserver\.com|chart\.googleapis\.com\/chart/.test(html), `${page}: no third-party QR source or fallback`);
    const resources = [];
    const imageSource = src => { resources.push(src); ok(/^data:image\/gif;base64,[A-Za-z0-9+/]+=*$/.test(src), `${page}: dynamic QR image cannot leak session URL`); };
    const scripts = [...html.matchAll(/<script\b([^>]*)>([\s\S]*?)<\/script>/gi)].filter(m => !/\bsrc=/.test(m[1]));
    for (const [, , js] of scripts) { parse(js, { ecmaVersion: 'latest' }); checks++; }
    const markup = html.replace(/<script\b[^>]*>[\s\S]*?<\/script>/gi, '').replace(/<style\b[^>]*>[\s\S]*?<\/style>/gi, '');
    for (const [, js] of markup.matchAll(/\bon(?:click|input|keypress|error)="([^"]*)"/g)) {
        parse(js.replace(/&quot;/g, '"').replace(/&amp;/g, '&'), { ecmaVersion: 'latest', allowReturnOutsideFunction: true }); checks++;
    }
    class Element {
        constructor(classes = '', tagName = '') {
            this.tagName = tagName.toLowerCase(); this.events = new Map();
            this.classes = new Set(classes.split(/\s+/)); this.style = {}; this.children = []; this.value = ''; this.disabled = false; this._text = ''; this.domIds = [];
            this.classList = { add: (...xs) => xs.forEach(x => this.classes.add(x)), remove: (...xs) => xs.forEach(x => this.classes.delete(x)), contains: x => this.classes.has(x), toggle: x => this.classes.has(x) ? this.classes.delete(x) : this.classes.add(x) };
        }
        set className(value) { this.classes = new Set(String(value).split(/\s+/)); }
        get className() { return [...this.classes].join(' '); }
        get unavailable() { return this.disabled || this.hidden || this.classes.has('hidden') || (this.tagName === 'details' && !this.open) || Boolean(this.parentNode?.unavailable); }
        set textContent(value) {
            for (const [id, el] of this.domIds) if (elements.get(id) === el) elements.delete(id);
            this.domIds = []; this._text = String(value); this.children = [];
        }
        get textContent() { return this._text + this.children.map(c => c.textContent).join(''); }
        set innerHTML(value) {
            this.textContent = value;
            for (const [, , src] of String(value).matchAll(/<img\b[^>]*\bsrc=(["'])(.*?)\1/gi)) imageSource(src);
            for (const [tag, id] of String(value).matchAll(/<[^>]*\bid="([^"]+)"[^>]*>/g)) {
                const child = new Element(tag.match(/\bclass="([^"]*)"/)?.[1] || '');
                elements.set(id, child); this.domIds.push([id, child]);
            }
        }
        get innerHTML() { return this.textContent; }
        appendChild(child) { child.parentNode = this; this.children.push(child); return child; }
        set src(value) { this._src = value; if (this.tagName === 'img') imageSource(value); }
        get src() { return this._src; }
        addEventListener(name, fn) { if (!this.events.has(name)) this.events.set(name, []); this.events.get(name).push(fn); }
        dispatch(name) { (this.events.get(name) || []).forEach(fn => fn({ stopPropagation() {}, preventDefault() {} })); }
        remove() {}
        getContext() { return this; }
    }
    const elements = new Map();
    // Build the actual static tree: IDs, inherited fieldset disabling, hidden ancestors and initial text.
    const body = new Element('', 'body'), stack = [body];
    const voidTags = new Set(['input', 'img', 'link', 'meta', 'br', 'hr']);
    for (const token of markup.match(/<!--[\s\S]*?-->|<[^>]+>|[^<]+/g) || []) {
        if (token.startsWith('<!--') || token.startsWith('<!')) continue;
        if (token.startsWith('</')) { if (stack.length > 1) stack.pop(); continue; }
        if (token.startsWith('<')) {
            const name = token.match(/^<([\w-]+)/)?.[1].toLowerCase(); if (!name) continue;
            const element = new Element(token.match(/\bclass="([^"]*)"/)?.[1] || '', name);
            element.disabled = /\sdisabled(?:\s|=|>)/.test(token);
            element.hidden = /\shidden(?:\s|=|>)/.test(token); element.open = /\sopen(?:\s|=|>)/.test(token);
            element.markup = token;
            const id = token.match(/\bid="([^"]+)"/)?.[1];
            if (id) { ok(!elements.has(id), `${page}: unique real DOM ID ${id}`); elements.set(id, element); }
            stack.at(-1).appendChild(element);
            if (!voidTags.has(name) && !token.endsWith('/>')) stack.push(element);
        } else stack.at(-1)._text += decodeAttribute(token);
    }
    const listeners = new Map();
    const document = {
        hidden: false, getElementById: id => elements.get(id) || null, createElement: tag => new Element('', tag),
        querySelectorAll: selector => selector.startsWith('.') ? [...elements.values()].filter(el => el.classes.has(selector.slice(1))) : [], body, write() {},
        addEventListener: (event, fn) => { if (!listeners.has(event)) listeners.set(event, []); listeners.get(event).push(fn); }
    };
    let now = 0, nextTimer = 1;
    const timers = new Map(), requests = [], confirmations = [], alerts = [], charts = [], chartOwners = new Set();
    let answer = () => ({ code: 200, success: true, data: {} });
    const timer = (fn, delay, repeat) => { const id = nextTimer++; timers.set(id, { fn, due: now + delay, repeat: repeat ? delay : 0 }); return id; };
    const context = vm.createContext({
        console, document, tailwind: {}, URLSearchParams, AbortController,
        Date: class extends Date { static now() { return now; } },
        setTimeout: (fn, delay = 0) => timer(fn, delay, false), setInterval: (fn, delay = 0) => timer(fn, delay, true),
        clearTimeout: id => timers.delete(id), clearInterval: id => timers.delete(id),
        confirm: text => { confirmations.push(text); return context.confirmAnswer; }, confirmAnswer: true,
        alert: text => alerts.push(text),
        fetch: async (url, options = {}) => {
            const fields = options.body ? Object.fromEntries(options.body) : Object.fromEntries(new URLSearchParams(String(url).split('?')[1]));
            const req = { url, options, fields }; requests.push(req);
            ok(!/https?:/.test(String(url)), 'synthetic test must never fetch external URLs');
            if (options.method === 'POST') {
                ok(options.body instanceof URLSearchParams, 'POST body must be URLSearchParams');
                ok(!String(url).includes('?'), 'POST URL contains no credentials or parameters');
                ok(options.credentials === 'same-origin', 'browser cookie authorization');
            }
            const result = await answer(req);
            return { ok: !result?.__httpStatus || (result.__httpStatus >= 200 && result.__httpStatus < 300), status: result?.__httpStatus || 200, json: async () => { if (result?.__invalidJson) throw new Error('synthetic malformed response'); return result; } };
        },
        location: { search: '', origin: 'http://synthetic.invalid', pathname: '/' }, scrollTo() {}, addEventListener() {},
        Chart: class {
            constructor(canvas, options) { assert.ok(!chartOwners.has(canvas), 'canvas already owns a Chart'); chartOwners.add(canvas); this.canvas = canvas; this.data = options.data; this.updates = 0; charts.push(this); }
            destroy() { chartOwners.delete(this.canvas); }
            update() { this.updates++; }
        }, navigator: {},
    });
    context.window = context;
    qrScript.runInContext(context);
    for (const [, , js] of scripts) vm.runInContext(js, context, { filename: `${page}.html` });
    timers.clear();
    context.showNotification = text => alerts.push(text);
    const originalLoadRecords = context.loadRecords, originalLoadStats = context.loadStats;
    context.loadRecords = () => {}; context.loadStats = () => {};
    const originalLoadAllLoginStatus = context.loadAllLoginStatus;
    if (page === 'admin') {
        elements.get('page-qrlogin').classList.add('active');
        context.loadAllLoginStatus = () => {};
    }
    return {
        context, html, elements, requests, timers, confirmations, alerts, resources, charts, originalLoadAllLoginStatus, originalLoadRecords, originalLoadStats,
        async ready() { (listeners.get('DOMContentLoaded') || []).forEach(fn => fn()); await flush(); },
        answer(fn) { answer = fn; },
        get(id) { const el = elements.get(id); assert.ok(el, `missing DOM ID: ${id}`); return el; },
        async clickParseButton() {
            // A browser click runs both the static inline handler and registered listeners.
            // preventDefault does not suppress a second listener on the same button.
            const tag = [...markup.matchAll(/<[^>]*\bid="([^"]+)"[^>]*>/g)].find(([, id]) => id === 'parseButton')?.[0];
            const inline = tag?.match(/\bonclick="([^"]*)"/)?.[1];
            if (inline) vm.runInContext(decodeAttribute(inline), context);
            this.get('parseButton').dispatch('click');
            await flush();
        },
        async advance(ms) {
            const end = now + ms;
            for (;;) {
                const due = [...timers.entries()].filter(([, t]) => t.due <= end).sort((a, b) => a[1].due - b[1].due)[0];
                if (!due) break;
                const [id, t] = due; now = t.due;
                if (t.repeat) t.due += t.repeat; else timers.delete(id);
                t.fn(); await flush();
            }
            now = end; await flush();
        },
        async click(id) {
            const el = this.get(id); if (el.unavailable) return;
            const inline = el.markup?.match(/\bonclick="([^"]*)"/)?.[1];
            if (inline) vm.runInContext(decodeAttribute(inline), context);
            el.dispatch('click'); await flush();
        },
        async hide() { document.hidden = true; (listeners.get('visibilitychange') || []).forEach(fn => fn()); await flush(); },
        unhide() { document.hidden = false; }
    };
}
// One button activation is one parse request; include HTML onclick, not only JS listeners.
{
    const h = harness('index'), c = h.context;
    const share = 'https://synthetic.invalid/share?key=one&other=two', pwd = 'synthetic+&"pwd';
    h.answer(req => req.fields.url ? { code: 400, success: false, msg: 'synthetic result', data: null }
        : { code: 200, success: true, data: { total: 0, success: 0, failed: 0, cached: 0 } });
    h.get('shareUrl').value = share; h.get('sharePassword').value = pwd;
    c.setupEventListeners();
    await h.clickParseButton();
    let parses = h.requests.filter(req => 'url' in req.fields);
    ok(parses.length === 1, 'one parse-button click sends exactly one parse request, including inline and registered handlers');
    ok(parses[0].fields.url === share && parses[0].fields.pwd === pwd && parses[0].fields.type === 'json', 'single parse keeps exact share/password/type encoding');
    await h.clickParseButton();
    parses = h.requests.filter(req => 'url' in req.fields);
    ok(parses.length === 2, 'a later intentional click adds just one request; repeat parsing is not disabled');
}
{
    const h = harness('index'), c = h.context;
    const share = 'https://synthetic.invalid/auto-share', pwd = 'synthetic+auto';
    h.answer(() => ({ code: 400, success: false, msg: 'synthetic result', data: null }));
    c.location.search = '?' + new URLSearchParams({ url: share, pwd, type: 'json' });
    c.setupEventListeners(); c.autoParseFromUrl(); await flush();
    let parses = h.requests.filter(req => 'url' in req.fields);
    ok(parses.length === 1 && parses[0].fields.url === share && parses[0].fields.pwd === pwd, 'URL auto-parse remains exactly once with the original parameters');
    await h.clickParseButton();
    parses = h.requests.filter(req => 'url' in req.fields);
    ok(parses.length === 2, 'URL auto-parse followed by one intentional click adds one request');
}
const qr = { code: 200, success: true, data: { qr_url: 'https://synthetic.invalid/qr', device_id: 'synthetic-device', expires_in: 300, interval: 3 } };
const status = { code: 200, success: true, data: { cloud189: { configured: true, logged_in: true, verified: false, source: 'manual' }, mcloud: { configured: true, logged_in: true, verified: false, account_mask: '123****890' } } };
for (const page of ['admin', 'index']) {
    for (const provider of ['cloud189', 'mcloud']) {
        const h = harness(page), c = h.context;
        h.answer(req => req.fields.action.endsWith('_qrcode') ? qr : req.fields.action.endsWith('_check') ? { code: 200, success: true, data: { status: provider === 'cloud189' ? 'success' : 'confirmed' } } : status);
        const start = () => page === 'admin' ? c.startAuthQR(provider) : c.frontendQrLogin(provider);
        await start();
        const image = h.get(page === 'admin' ? (provider === 'cloud189' ? 'c189' : 'mcloud') + 'QrcodeImage' : 'qrCodeContainer').children[0];
        checkGIF(image?.src || '', `${page}/${provider}`);
        ok(image.src === c.localQRCodeDataURL(qr.data.qr_url), 'exact session URL encoded by actual local generator');
        await h.advance(3000);
        ok(h.requests.some(r => r.fields.action === provider + '_check' && r.fields.device_id === 'synthetic-device'), `${page}/${provider}: correct check action`);
        ok(h.timers.size === 0, `${page}/${provider}: success stops all timers`);
        if (page === 'admin') ok(!h.get((provider === 'cloud189' ? 'c189' : 'mcloud') + 'LoginSuccess').classList.contains('hidden'), 'admin success UI');
        else { ok(h.get('qrLoginStatus').textContent.includes('服务端'), 'server persistence success UI'); ok(!c.memoryConfig[provider], 'no credential copied to browser memory'); }

        h.answer(req => req.fields.action.endsWith('_qrcode') ? qr : { code: 400, success: false, data: { status: 'expired' } });
        await start(); await h.advance(3000);
        ok(h.timers.size === 0, 'success=false expired also stops polling');
        h.answer(req => req.fields.action.endsWith('_qrcode') ? qr : { code: 200, success: true, data: { status: 'waiting' } });
        await start(); await h.advance(300000);
        ok(h.timers.size === 0, 'five minute bound');
        await start(); await h.hide(); ok(h.timers.size === 0, 'hidden tab stops polling'); h.unhide();

        // A slow check must never spawn another overlapping check; cancellation ignores stale success.
        let resolveCheck;
        h.answer(req => req.fields.action.endsWith('_qrcode') ? qr : new Promise(resolve => { resolveCheck = resolve; }));
        const before = h.requests.filter(r => r.fields.action.endsWith('_check')).length;
        await start(); await h.advance(15000);
        ok(h.requests.filter(r => r.fields.action.endsWith('_check')).length === before + 1, 'one in-flight poll only');
        if (page === 'admin') c.cancelAuthQR(provider); else c.closeQrLogin();
        resolveCheck({ code: 200, success: true, data: { status: 'confirmed' } }); await flush();
        ok(h.timers.size === 0, 'stale success cannot restart timers');
        ok(h.requests.filter(r => r.fields.action.endsWith('_check')).at(-1).options.signal.aborted, 'cancel aborts in-flight request');

        // Double starts cannot revive the old QR request after the current one completes.
        const imagesBefore = h.resources.length;
        let resolveOld;
        h.answer(() => new Promise(resolve => { resolveOld = resolve; }));
        const oldStart = start();
        h.answer(() => qr); await start();
        resolveOld(qr); await oldStart; await flush();
        ok(h.timers.size === 2, 'double start has just one clock and one poll timer');
        ok(h.resources.length === imagesBefore + 1, 'stale QR response cannot render or leak an extra session image');
        if (page === 'admin') c.cancelAuthQR(provider); else c.closeQrLogin();

        h.answer(req => req.fields.action === 'login_status' ? status : { code: 200, success: true });
        const inputId = page === 'admin' ? provider === 'cloud189' ? 'c189TokenInput' : 'mcloudAuthInput' : provider === 'cloud189' ? 'c189Token' : 'mcloudAuth';
        h.get(inputId).value = 'synthetic-not-a-credential';
        if (page === 'admin') await (provider === 'cloud189' ? c.saveC189ManualLogin() : c.saveMCloudManualLogin()); else await c.saveToMemory(provider);
        const saved = h.requests.find(r => r.fields.action === provider + '_manual_save');
        ok(saved?.fields[provider === 'cloud189' ? 'access_token' : 'authorization'] === 'synthetic-not-a-credential', 'manual save uses correct form field');
        ok(h.get(inputId).value === '', 'clear manual credential input after save');
        if (page === 'admin') await (provider === 'cloud189' ? c.logoutC189() : c.logoutMCloud()); else await c.logoutFrontendAuth(provider);
        ok(h.requests.some(r => r.fields.action === provider + '_logout' && r.options.method === 'POST'), 'logout POST');
    }
}
{
    const h = harness('admin'), c = h.context;
    h.answer(() => status);
    await h.originalLoadAllLoginStatus();
    ok(h.get('cloud189-status-badge').textContent === '已配置' && h.get('mcloud-status-badge').textContent === '已配置', 'real status loader continues past absent unmigrated providers');
    ok(h.get('cloud189-status-source').textContent.includes('未在线验证'), 'configured is not represented as online verification');
    c.updateLoginCard('gy', undefined); c.updateFeijiLoginCard(undefined); c.updateIlanzouLoginCard(undefined);
    ok(h.get('gy-status-badge').textContent === '暂不支持', 'missing provider not reported falsely configured/logged out');
    ok(h.get('feiji-qr-status').textContent.includes('暂不支持') && h.get('ilanzou-login-status').textContent.includes('暂不支持'), 'other provider DOM retained with release unsupported state');
    h.answer(() => ({ success: true, data: { cloud189: { logged_in: true, source: 'manual', account_mask: '123****890', loginInfo: { access_token: 'synthetic-private' } } } }));
    await h.originalLoadAllLoginStatus(); c.showLoginDetail('cloud189');
    ok(h.get('modalContent').textContent.includes('123****890') && !h.get('modalContent').textContent.includes('synthetic-private'), 'details use safe metadata only');
    c.showLoginDetail('mcloud'); ok(h.get('modalContent').textContent.includes('状态读取失败'), 'absent supported metadata is an explicit read failure');

    h.answer(() => ({ code: 200, success: true, data: { log_retention_days: 7 } }));
    await c.loadLogSettings(); ok(Number(h.get('logRetentionDays').value) === 7 && !h.get('saveLogSettingsButton').disabled, 'load retention setting');
    const before = h.requests.length;
    for (const invalid of ['', '0', '-1', '1.5', '3651', 'text']) { h.get('logRetentionDays').value = invalid; await c.saveLogSettings(); }
    ok(h.requests.length === before, 'invalid retention days blocked');
    h.get('logRetentionDays').value = '3'; c.confirmAnswer = false; await c.saveLogSettings();
    ok(h.requests.length === before && h.confirmations.at(-1).includes('永久'), 'shortening confirmation prevents destructive submission');
    c.confirmAnswer = true; await c.saveLogSettings();
    ok(h.requests.at(-1).fields.log_retention_days === '3' && h.requests.at(-1).fields.action === 'set_log_settings', 'shortening setting form POST');
    const count = h.confirmations.length;
    h.get('logRetentionDays').value = '3650'; await c.saveLogSettings();
    ok(h.confirmations.length === count, 'extension needs no destructive confirmation');
    h.answer(() => ({ code: 500, success: false, msg: 'synthetic failure' }));
    await c.saveLogSettings(); ok(!h.get('saveLogSettingsButton').disabled && h.get('logSettingsMessage').textContent.includes('失败'), 'save failure visible and retry enabled');
    await c.loadLogSettings(); ok(h.get('saveLogSettingsButton').disabled, 'read failure blocks uncertain destructive updates');
    h.answer(() => ({ code: 200, success: true })); await c.deleteRecord('synthetic-record');
    ok(h.requests.at(-1).fields.action === 'delete_record' && h.requests.at(-1).fields.record_id === 'synthetic-record', 'record delete POST');
    await c.logout();
    ok(h.requests.at(-1).fields.action === 'logout' && h.requests.at(-1).url === '/admin/' && c.location.href === '/admin/', 'admin session logout POST before navigation');
    ok(h.html.includes('oninput="searchRecords(this.value)"') && h.html.includes('function renderRecords'), 'original record search and rendering UI retained');
}
// Legacy providers keep their existing actions/UI, but QR rendering is local for every session.
for (const page of ['admin', 'index']) {
    const h = harness(page), c = h.context;
    const sessionURL = 'https://synthetic.invalid/qr?session=synthetic-only&text="<svg/onload=synthetic>"&unicode=扫码😀';
    h.answer(() => ({ success: true, data: { ...qr.data, qr_url: sessionURL, qr_code_url: sessionURL, verification_uri_complete: sessionURL, token: 'synthetic-token', user_code: 'synthetic-code' } }));
    const providers = page === 'admin' ? [
        ['startGuangyaQRLogin', 'cancelGuangyaQRLogin', 'qrcodeImage'],
        ['startAliyunQRLoginCard', 'cancelAliyunQRLoginCard', 'aliyunQrcodeImageCard'],
        ['startUCQRLogin', 'cancelUCQRLogin', 'ucQrcodeImage'],
        ['startQuarkQRLogin', 'cancelQuarkQRLogin', 'quarkQrcodeImage']
    ] : ['guangya', 'aliyun', 'uc', 'quark'];
    for (const provider of providers) {
        if (page === 'admin') await c[provider[0]](); else await c.frontendQrLogin(provider);
        const id = page === 'admin' ? provider[2] : 'qrCodeContainer';
        const src = h.get(id).innerHTML.match(/<img\b[^>]*src="([^"]+)"/)?.[1];
        checkGIF(src || '', `${page}: legacy QR`);
        ok(src === c.localQRCodeDataURL(sessionURL), 'legacy QR encodes original URL locally, including Unicode');
        ok(Buffer.from(c.qrcode.stringToBytes(sessionURL)).equals(Buffer.from(sessionURL, 'utf8')), 'local pages select the real UTF-8 byte encoder');
        ok(!h.get(id).innerHTML.includes('onerror=') && !h.get(id).innerHTML.includes(sessionURL), 'no remote fallback or raw session interpolation');
        if (page === 'admin') c[provider[1]](); else c.closeQrLogin();
    }
    ok(h.resources.length === 4, 'all legacy QR images use local data URLs');
}
// Actual local factory API works without any DOM, network or package runtime.
{
    const c = vm.createContext({}); qrScript.runInContext(c);
    const code = c.qrcode(0, 'M'); code.addData('synthetic QR API smoke test'); code.make();
    checkGIF(code.createDataURL(4, 16), 'standalone vendored factory');
    ok(code.getModuleCount() >= 21 && code.isDark(0, 0) && !code.isDark(1, 1), 'actual QR module matrix/finder pattern');
    ok(code.createImgTag().includes('src="data:image/gif;base64,'), 'official createImgTag API callable');
}
for (const page of ['admin', 'index']) {
    const h = harness(page), c = h.context;
    h.answer(() => qr);
    c.qrcode = undefined;
    await (page === 'admin' ? c.startAuthQR('cloud189') : c.frontendQrLogin('cloud189'));
    ok(h.timers.size === 0 && h.resources.length === 0, 'missing local generator fails closed: no QR service fallback or polling');
    ok(page === 'admin' ? h.alerts.length > 0 : h.get('qrLoginStatus').textContent.includes('失败'), 'local generator failure is visible');
}
function recordData(page, limit, type, term) {
    const counts = { success: 25, failed: 13 };
    const rows = Array.from({ length: Math.max(0, Math.min(limit, counts[type] - (page - 1) * limit)) }, (_, i) => ({
        id: (page - 1) * limit + i + 1, timestamp: '2026-01-01T00:00:00Z', success: type === 'success',
        url: 'https://synthetic.invalid/share', code: type === 'success' ? 200 : 400, msg: term || 'synthetic result'
    }));
    return { success: type === 'success' ? rows : [], failed: type === 'failed' ? rows : [], total: counts[type], successTotal: counts.success, failedTotal: counts.failed, page, limit };
}
// Search requests must use SQL-filter-compatible type/page/limit, not fetch both unbounded tabs.
{
    const h = harness('admin'), c = h.context; c.loadRecords = h.originalLoadRecords;
    h.answer(req => ({ success: true, data: recordData(Number(req.fields.page), Number(req.fields.limit), req.fields.type, req.fields.search) }));
    const term = '  Synthetic +&"?  ';
    h.get('recordSearchInput').value = term;
    await c.searchRecords(term);
    let request = h.requests.at(-1);
    ok(request.fields.action === 'search_records' && request.fields.search === term.trim(), 'search uses encoded trimmed content');
    ok(request.fields.type === 'success' && request.fields.page === '1' && request.fields.limit === '10', 'search sends current tab and bounded first page');
    ok(h.get('successCount').textContent.includes('25/搜索结果') && h.get('failedCount').textContent.includes('13/搜索结果'), 'search counts use server totals, not page lengths');
    ok(h.get('recordsList').innerHTML.includes('onclick="loadRecords(2)"') && h.get('recordsList').innerHTML.includes('第 1 / 3 页'), 'search pagination visible');
    await c.loadRecords(2);
    ok(h.requests.at(-1).fields.page === '2' && h.requests.at(-1).fields.search === term.trim(), 'search next page keeps term, does not restart page 1');
    await c.loadRecords();
    ok(h.requests.at(-1).fields.page === '2', 'refresh keeps searched page');
    await c.changePage(3); await flush();
    ok(h.requests.at(-1).fields.page === '3' && h.get('recordsList').innerHTML.includes('第 3 / 3 页'), 'existing changePage hook preserves search pagination');
    c.switchTab('failed'); await flush();
    request = h.requests.at(-1);
    ok(request.fields.type === 'failed' && request.fields.page === '1' && request.fields.search === term.trim(), 'switch search tab resets only page, retains filter');
    ok(h.get('recordsList').innerHTML.includes('第 1 / 2 页') && h.get('recordsList').innerHTML.includes('onclick="loadRecords(2)"'), 'failed search tab has separate server count/pages');
    await c.loadRecords(2);
    ok(h.requests.at(-1).fields.type === 'failed' && h.requests.at(-1).fields.page === '2', 'failed tab next page stays failed');
    h.get('recordSearchInput').value = 'other'; await c.searchRecords('other');
    ok(h.requests.at(-1).fields.page === '1', 'changed search resets first page deliberately');
    h.get('recordSearchInput').value = ''; await c.searchRecords('');
    request = h.requests.at(-1);
    ok(request.fields.action === 'get_records' && request.fields.type === 'failed' && request.fields.page === '1' && !('search' in request.fields), 'clearing search restores normal filtered listing');
    let resolveOld;
    h.answer(req => req.fields.search === 'old' ? new Promise(resolve => { resolveOld = resolve; }) : { success: true, data: recordData(1, 10, 'failed', 'new') });
    h.get('recordSearchInput').value = 'old'; const old = c.searchRecords('old');
    h.get('recordSearchInput').value = 'new'; await c.searchRecords('new');
    resolveOld({ success: true, data: recordData(1, 10, 'failed', 'stale-old') }); await old;
    ok(vm.runInContext('allRecords._searchTerm', c) === 'new' && !h.get('recordsList').innerHTML.includes('stale-old'), 'slow older search cannot overwrite newer results');
    let rejectOld;
    h.answer(req => req.fields.search === 'old-error' ? new Promise((resolve, reject) => { rejectOld = reject; }) : { success: true, data: recordData(1, 10, 'failed', 'new') });
    h.get('recordSearchInput').value = 'old-error'; const failedOld = c.searchRecords('old-error');
    h.get('recordSearchInput').value = 'new'; await c.searchRecords('new');
    rejectOld(new Error('stale-old-error')); await failedOld;
    ok(!h.get('recordsList').innerHTML.includes('stale-old-error'), 'slow older error cannot replace newer search results');
    h.answer(() => ({ success: true, data: { success: [], failed: [], total: 0, successTotal: 0, failedTotal: 0, page: 1, limit: 10 } }));
    h.get('recordSearchInput').value = '<svg/onload=synthetic>'; await c.searchRecords(h.get('recordSearchInput').value);
    ok(h.get('recordsList').innerHTML.includes(escapeText(h.get('recordSearchInput').value)) && !h.get('recordsList').innerHTML.includes('<svg'), 'empty search keyword is escaped');
}
// Stored record JSON, nested data, messages, URL/pwd, and delete handler are untrusted.
{
    const h = harness('admin'), c = h.context; c.loadRecords = h.originalLoadRecords;
    const hostile = '</pre><svg/onload=syntheticXSS()><script>syntheticXSS()</script>&"\'';
    const record = { id: "synthetic');syntheticXSS();//\"&<", timestamp: '2026-01-01T00:00:00Z', success: true, code: 200, url: hostile, pwd: hostile, msg: hostile, data: { file_name: hostile, nested: [{ error: hostile }] } };
    h.answer(() => ({ success: true, data: { success: [record], failed: [], total: 1, successTotal: 1, failedTotal: 0, page: 1, limit: 10 } }));
    await c.loadRecords(1);
    let html = h.get('recordsList').innerHTML;
    ok(!/<svg\b|<script\b|<img\b/.test(html) && html.includes(escapeText(hostile)), 'record payload cannot create active HTML');
    ok(!html.includes(escapeText(JSON.stringify(record.data, null, 2))) && !html.includes('查看详细数据') && !html.includes('<pre'), 'release records omit raw nested JSON without modifying history data');
    const handlers = [...html.matchAll(/\bonclick="([^"]*)"/g)].map(m => decodeAttribute(m[1]));
    ok(handlers.length === 1, 'hostile record ID cannot break out into additional handlers');
    const tree = parse(handlers[0], { ecmaVersion: 'latest' });
    ok(tree.body.length === 1 && tree.body[0].expression.callee.name === 'deleteRecord', 'delete handler contains exactly one intended call');
    let deleted; c.deleteRecord = id => { deleted = id; }; c.event = { stopPropagation() {} };
    vm.runInContext(handlers[0], c);
    ok(deleted === record.id, 'ID travels as a string, not injected JavaScript');
    // Highlighter must escape each raw fragment, never split escaped entity names.
    for (const term of ['<svg', 'amp', '.*+?', '"', '</pre>']) {
        h.get('recordSearchInput').value = term; await c.searchRecords(term);
        html = h.get('recordsList').innerHTML;
        ok(!/<svg\b|<script\b/.test(html), `hostile search ${term} cannot reactivate stored text`);
        if (term === 'amp') ok(html.includes('&amp;') && !html.includes('&<mark'), 'highlighter cannot corrupt escape entities');
    }
    h.answer(() => ({ success: false, msg: hostile })); await c.loadRecords();
    ok(h.get('recordsList').innerHTML.includes('记录读取失败，请重试') && !h.get('recordsList').innerHTML.includes(hostile), 'record fetch failure visible without exposing raw service errors');
}
// Mobile direct-link closure: root and nested files use ordinary JSON parse with id/fid.
for (const chain of [[], ['synthetic-root', 'synthetic-nested'], null, '{bad-json']) {
    const h = harness('index'), c = h.context;
    c.currentShareUrl = 'https://synthetic.invalid/share?key=a&other=b'; c.currentFolderPwd = 'synthetic+&"pwd';
    const button = { dataset: { name: 'synthetic file', size: '123', fileId: 'synthetic&file+id', fidChain: typeof chain === 'string' ? chain : JSON.stringify(chain) } };
    const downloadURL = 'https://synthetic.invalid/download?one=1&two=2';
    h.answer(() => ({ success: true, data: { download_url: downloadURL, file_name: 'synthetic result file', file_size: '123' } }));
    c.parseMcloudFile(button); await flush();
    const request = h.requests.at(-1), nested = Array.isArray(chain) && chain.length > 0;
    ok(request.fields.url === c.currentShareUrl && request.fields.pwd === c.currentFolderPwd && request.fields.id === button.dataset.fileId && request.fields.type === 'json', 'mobile uses ordinary parse with exact encoded share/password/file ID');
    ok(!('action' in request.fields) && !('file_id' in request.fields), 'mobile no longer relies on legacy subfolder file_id path');
    ok(nested ? request.fields.fid === chain[0] && request.fields.fid2 === chain[1] : !('fid' in request.fields), 'mobile root/nested/malformed chains handled');
    ok(h.get('modalResult').innerHTML.includes(escapeText(downloadURL)) && h.get('modalActions').innerHTML.includes('modalDownBtn'), 'mobile success shows download and actions');
    let shortURL; c.generateShortLinkForModal = url => { shortURL = url; };
    h.get('modalShortBtn').dispatch('click');
    const shortFields = Object.fromEntries(new URL(shortURL).searchParams);
    ok(shortFields.id === button.dataset.fileId && shortFields.type === 'down' && shortFields.url === c.currentShareUrl && shortFields.pwd === c.currentFolderPwd, 'mobile generated short-link target keeps file context');
    ok(nested ? shortFields.fid === chain[0] && shortFields.fid2 === chain[1] : !('fid' in shortFields), 'mobile short-link target keeps nested folder chain');
}
{
    const h = harness('index'), c = h.context;
    c.currentShareUrl = 'https://synthetic.invalid/share';
    const url = 'https://synthetic.invalid/preloaded-download';
    const button = { dataset: { name: 'synthetic preloaded file', size: '1', fileId: 'synthetic-file', downloadUrl: url, fidChain: '[]' } };
    const before = h.requests.length;
    c.parseMcloudFile(button); await h.advance(500);
    ok(h.requests.length === before && h.get('modalResult').innerHTML.includes(url), 'mobile preloaded direct link still closes without reparsing');
    let downloaded; c.downloadLanzouFile = (url, name) => { downloaded = { url, name }; };
    h.get('modalDownBtn').dispatch('click');
    ok(downloaded?.url === url && downloaded?.name === button.dataset.name, 'mobile preloaded download keeps original URL and filename');
}
{
    const h = harness('index'), c = h.context;
    c.currentShareUrl = 'https://synthetic.invalid/share';
    const button = { dataset: { name: 'synthetic file', size: '1', fileId: 'synthetic-file', fidChain: '[]' } };
    const hostile = '<svg/onload=syntheticXSS()>';
    h.answer(() => ({ success: false, msg: hostile })); c.parseMcloudFile(button); await flush();
    ok(h.get('modalResult').innerHTML.includes(escapeText(hostile)) && !h.get('modalResult').innerHTML.includes('<svg'), 'mobile parse failure HTML escaped');
    h.answer(() => { throw new Error(hostile); }); c.parseMcloudFile(button); await flush();
    ok(h.get('modalResult').innerHTML.includes(escapeText(hostile)) && !h.get('modalResult').innerHTML.includes('<svg'), 'mobile network failure HTML escaped');
}
// Parser tuning is private, validated, persisted by one POST, and safe on failures.
{
    const h = harness('admin'), c = h.context;
    ok(h.get('parseConcurrency').disabled && h.get('saveParseSettingsButton').disabled, 'parser settings start disabled until an authenticated read');
    h.answer(() => ({ success: true, data: { concurrency: 4 } }));
    await c.loadParseSettings();
    ok(h.requests.at(-1).fields.action === 'get_parse_settings' && h.requests.at(-1).options.credentials === 'same-origin', 'parser settings read uses private endpoint/cookie');
    ok(Number(h.get('parseConcurrency').value) === 4 && !h.get('saveParseSettingsButton').disabled, 'authenticated saved concurrency is rendered only after a valid read (synthetic value four)');
    const before = h.requests.length;
    for (const invalid of ['', '0', '-1', '17', '1.5', 'NaN', '1e99']) {
        h.get('parseConcurrency').value = invalid; await c.saveParseSettings();
        ok(h.requests.length === before, 'invalid concurrency never posts');
    }
    h.get('parseConcurrency').value = '8';
    let resolveSave;
    h.answer(() => new Promise(resolve => { resolveSave = resolve; }));
    const save = c.saveParseSettings(); await flush();
    ok(h.get('parseConcurrency').disabled && h.get('saveParseSettingsButton').disabled, 'pending save disables controls');
    await c.saveParseSettings();
    ok(h.requests.length === before + 1, 'repeated click does not duplicate concurrency update');
    const request = h.requests.at(-1);
    ok(request.fields.action === 'set_parse_settings' && request.fields.concurrency === '8' && request.url === '/admin/', 'one encoded same-origin POST saves the new concurrency');
    resolveSave({ success: true, data: { concurrency: 8 } }); await save;
    ok(Number(h.get('parseConcurrency').value) === 8 && h.get('parseSettingsMessage').textContent.includes('已保存'), 'only successful response publishes visible saved value');
    h.get('parseConcurrency').value = '3';
    const hostile = '<svg/onload=syntheticXSS()>';
    h.answer(() => ({ success: false, msg: hostile })); await c.saveParseSettings();
    ok(h.get('parseSettingsMessage').textContent.includes('保存失败，请重试') && !h.get('parseSettingsMessage').textContent.includes(hostile) && h.get('parseSettingsMessage').children.length === 0, 'release save failure does not expose raw service errors');
    ok(vm.runInContext('currentParseConcurrency', c) === 8 && !h.get('saveParseSettingsButton').disabled, 'failed save retains confirmed setting and permits retry');
    h.answer(() => { throw new Error('synthetic transport failure'); }); await c.saveParseSettings();
    ok(vm.runInContext('currentParseConcurrency', c) === 8 && !h.get('saveParseSettingsButton').disabled, 'network failure restores controls without claiming persistence');
    const fresh = harness('admin'); fresh.answer(() => ({ success: false })); await fresh.context.loadParseSettings();
    ok(fresh.get('saveParseSettingsButton').disabled, 'failed initial read cannot save an assumed default');
}
// Exact {id,size} leaves, including ID zero and unknown size, cannot hide directories named id/size.
{
    const h = harness('index'), c = h.context;
    const size = '9007199254740993';
    c.displayResult({ code: 200, success: true, data: { tree: { share: {
        unknown: { id: 'unknown', size: null }, empty: { id: 0, size: 0 }, big: { id: 'big', size },
        reserved: { id: {}, size: {} }
    } }, sid: 'synthetic' } });
    const html = h.get('result').innerHTML;
    ok((html.match(/data-id=/g) || []).length === 3, 'only real leaves receive direct-link controls');
    ok(html.includes('大小未知') && html.includes('data-id="0"'), 'unknown is not fabricated zero and zero ID remains a file');
    ok(html.includes('<summary') && html.includes(size), 'reserved-name directories render and large byte count remains exact in JSON');
}
// Real startup + dashboard/account navigation reuse charts and leave account status usable without the CDN.
for (const chartAvailable of [true, false]) {
    const h = harness('admin'), c = h.context;
    c.loadStats = h.originalLoadStats; c.loadAllLoginStatus = h.originalLoadAllLoginStatus;
    if (!chartAvailable) c.Chart = undefined;
    h.answer(req => req.fields.action === 'login_status' ? status : ({ success: true, data: { total: 12, success: 9, failed: 3, cached: 2, concurrency: 16, log_retention_days: 30 } }));
    await h.ready();
    ok(h.get('cloud189-status-badge').textContent === '已配置', 'real startup loads accounts even when Chart CDN is unavailable');
    for (let i = 0; i < 3; i++) { c.switchPage('qrlogin'); await flush(); c.switchPage('dashboard'); await flush(); }
    ok(h.charts.length === (chartAvailable ? 2 : 0), 'revisiting dashboard never recreates occupied chart canvases');
    if (chartAvailable) {
        ok(h.charts.every(chart => chart.updates >= 4), 'existing charts continue receiving fresh statistics after navigation');
        ok(h.charts[0].data.datasets[0].data.join(',') === '9,3' && h.charts[1].data.datasets[0].data.join(',') === '12,9,3,2', 'charts retain actual cumulative statistics rather than reset to zero');
    }
}
// Homepage authorization copy follows mandatory on-demand renewal and never displays raw status/errors.
{
    const h = harness('index'), c = h.context;
    ok(!/console\.|尚未迁移|授权已过期，请重新登录|仿照PHP逻辑/.test(h.html), 'homepage contains no debug logs or stale development/expiry copy');
    h.answer(() => ({ success: true, data: { cloud189: { configured: true, expired: true, token: 'rawtoken-sentinel' }, mcloud: { logged_in: true } } }));
    await c.refreshFrontendAuthStatus();
    ok(h.get('cloud189AuthStatus').textContent.startsWith('授权待续期') && h.get('mcloudAuthStatus').textContent.startsWith('已配置'), 'homepage distinguishes configured from locally expired authorization');
    for (const provider of ['cloud189', 'mcloud']) ok(h.get(provider + 'AuthStatus').textContent.includes('解析时按需尝试') && h.get(provider + 'AuthStatus').textContent.includes('根授权失效') && !h.get(provider + 'AuthStatus').textContent.includes('rawtoken'), 'homepage does not force immediate relogin or echo credential metadata');
    for (const failure of [{ ...status, __httpStatus: 503 }, { ...status, code: 400 }, { success: true, data: [] }, { ...status, __invalidJson: true }, null]) {
        h.answer(() => failure === null ? Promise.reject(new Error('rawtoken-sentinel')) : failure);
        await c.refreshFrontendAuthStatus();
        ok(['cloud189', 'mcloud'].every(provider => h.get(provider + 'AuthStatus').textContent === '状态读取失败，请重试'), 'homepage failures visibly clear stale authorization without leaking raw errors');
    }
    let resolveOld;
    h.answer(() => new Promise(resolve => { resolveOld = resolve; })); const old = c.refreshFrontendAuthStatus();
    h.answer(() => ({ success: true, data: { cloud189: { configured: false }, mcloud: { disabled: true } } })); await c.refreshFrontendAuthStatus();
    resolveOld(status); await old;
    ok(h.get('cloud189AuthStatus').textContent.startsWith('未授权') && h.get('mcloudAuthStatus').textContent === '暂不支持账号管理', 'older homepage status cannot overwrite a newer logout or disabled state');
}
// Release account management: static defaults and inherited disabled controls use real DOM ancestors.
{
    const h = harness('admin'), c = h.context;
    ok(h.get('nav-qrlogin').textContent.includes('网盘账号') && !h.html.includes('登录配置概况'), 'both entry points use the release account name while retaining navigation IDs');
    for (const id of ['cloud189-status-badge', 'mcloud-status-badge', 'c189StatusText', 'mcloud-qr-status']) ok(h.get(id).textContent.includes('正在读取'), 'first paint never claims a stale logged-out state');
    const unavailable = ['guangyaQRButton', 'aliyunQRButtonCard', 'ucQRButton', 'quarkQRButton', 'feijiQRButton', 'feijiAccountLoginBtn', 'ilanzouLoginButton', 'ilanzouAccountLoginBtn'];
    for (const id of unavailable) { ok(h.get(id).unavailable, `unsupported ${id} is statically hidden/disabled`); await h.click(id); }
    ok(h.requests.length === 0, 'unsupported entry clicks cannot send known-501 actions before status loads');
    const panels = [...h.html.matchAll(/<details\b([^>]*)>[\s\S]*?<summary[^>]*>其他网盘/g)];
    ok(panels.length === 2 && panels.every(m => !/\bopen\b/.test(m[1])), 'other providers default collapsed on both pages');
    for (const id of ['c189TokenInput', 'mcloudAuthInput', 'guangyaAccessToken', 'guangyaRefreshToken', 'aliyunAuthCard', 'ucCookieInput', 'quarkCookieInput', 'feijiTokenInput', 'pan123TokenInput']) {
        ok(/type="password"/.test(h.get(id).markup) && h.html.includes(`for="${id}"`), `${id}: credential masked with an explicit label`);
    }
    ok(h.get('c189QRButton').className === h.get('mcloudQRButton').className, 'supported scan buttons share a style');
    ok(!/console\.|localStorage|debugJSON|尚未迁移|环境变量|F12|D1|Rust/.test(h.html), 'admin contains no debug logging, browser token persistence or development copy');
    ok(h.html.includes('自动续期始终开启') && h.html.includes('解析时按需尝试') && h.html.includes('续期不保证成功'), 'mandatory renewal accurately described as on-demand, not guaranteed');
    h.answer(() => ({ success: true, data: { ...status.data, guangya: { logged_in: true, verified: true, source: 'rawtoken', account_mask: 'rawtoken' }, feiji: { logged_in: true }, ilanzou: { logged_in: true } } }));
    await h.originalLoadAllLoginStatus();
    for (const prefix of ['gy', 'aliyun', 'uc', 'quark', 'feiji', 'ilanzou', 'pan123']) ok(h.get(prefix + '-status-badge').textContent === '暂不支持', 'unintegrated providers cannot be promoted by untrusted status payload');
    for (const id of unavailable) { ok(h.get(id).unavailable, 'unsupported actions remain unavailable after status load'); await h.click(id); }
    ok(h.requests.length === 1, 'only the shared status read was made');
    c.showLoginDetail('guangya'); ok(h.get('modalContent').textContent === '状态：暂不支持', 'unsupported detail never exposes misleading verified/configured metadata');
}
// One whitelist/model supplies overview, account page and live detail, including local expiry.
{
    const h = harness('admin'), c = h.context;
    const sentinel = 'rawtoken-DO-NOT-RENDER<svg/onload=synthetic()>', phone = '123****890';
    const hostile = { configured: true, logged_in: false, expired: true, verified: true, source: sentinel, account_mask: phone, login_time: 1767225600, expires_at: 1767312000,
        access_token: sentinel, rawtoken: sentinel, credentials: { value: sentinel }, loginInfo: { access_token: sentinel }, message: sentinel, data: sentinel, auto_refresh_enabled: false };
    h.answer(() => ({ code: 200, success: true, data: { cloud189: hostile, mcloud: { ...hostile, timestamp: 1767225600000, expires_at_ms: 1767312000000 } } }));
    await h.originalLoadAllLoginStatus();
    for (const provider of ['cloud189', 'mcloud']) {
        const label = provider === 'cloud189' ? 'c189StatusText' : 'mcloud-qr-status';
        ok(h.get(provider + '-status-badge').textContent === '授权待续期' && h.get(label).textContent === '授权待续期', 'expired local credential is pending renewal in both places, not forced rescan');
        const overview = h.get(provider + '-status-source').textContent, page = h.get(provider + '-account-meta').textContent;
        ok(overview === page && page.includes(phone) && page.includes('过期时间') && page.includes('使用时自动尝试') && page.includes('未在线验证'), 'same safe source/account/expiry/renewal metadata on both pages');
        ok(!page.includes(sentinel) && !page.includes('需重新登录') && page.includes('服务端授权') && page.includes('始终开启'), 'unknown source hidden and disabled renewal metadata cannot disable mandatory policy');
        c.showLoginDetail(provider);
        const detail = h.get('modalContent').textContent;
        ok(detail.includes(phone) && detail.includes('服务端授权') && detail.includes('授权待续期') && !detail.includes(sentinel), 'detail uses same sanitized model, never raw response or credentials');
        const model = vm.runInContext(`loginStatusCache.${provider}`, c);
        ok(detail.includes(model.login) && detail.includes(model.expiry) && !JSON.stringify(model).includes(sentinel), 'metadata cache contains only safe projections with identical times');
    }
    for (const [source, label] of [['qrcode', '扫码授权'], ['manual', '手动授权'], ['seed', '初始授权'], ['configured', '服务端授权'], ['on_demand_refresh', '使用时续期'], ['constructor', '服务端授权'], ['__proto__', '服务端授权'], [sentinel, '服务端授权']]) {
        h.answer(() => ({ success: true, data: { cloud189: { configured: true, source, account_mask: sentinel, login_time: sentinel, expires_at_ms: { token: sentinel }, loginInfo: { rawtoken: sentinel } }, mcloud: { configured: false, logged_in: false } } }));
        await h.originalLoadAllLoginStatus(); c.showLoginDetail('cloud189');
        const combined = h.get('cloud189-status-source').textContent + h.get('cloud189-account-meta').textContent + h.get('modalContent').textContent;
        ok(combined.includes(label) && !combined.includes(sentinel) && !combined.includes('[object Object]'), 'source is allowlisted and malicious metadata is not stringified');
        ok(h.get('mcloud-status-badge').textContent === '未授权' && h.get('mcloud-qr-status').textContent === '未授权', 'actual empty supported provider stays distinct from unsupported');
    }
    h.answer(() => ({ success: true, data: { cloud189: { disabled: true, logged_in: true, account_mask: sentinel }, mcloud: { migrated: false, logged_in: true } } }));
    await h.originalLoadAllLoginStatus();
    const count = h.requests.length;
    for (const provider of ['cloud189', 'mcloud']) {
        ok(h.get(provider + '-status-badge').textContent === '暂不支持', 'explicitly unavailable integration does not claim verified or configured');
        await c.startAuthQR(provider);
    }
    ok(h.requests.length === count, 'disabled supported integration also blocks direct scan start');
}
// Initial pending, HTTP/body/JSON/transport failures, and response order never retain fresh-looking old state.
{
    const h = harness('admin'), c = h.context;
    c.loadAllLoginStatus = h.originalLoadAllLoginStatus;
    let resolvePending;
    h.answer(() => new Promise(resolve => { resolvePending = resolve; }));
    const pending = c.loadAllLoginStatus(); c.showLoginDetail('cloud189');
    ok(h.get('cloud189-status-badge').textContent.includes('正在读取') && h.get('c189StatusText').textContent.includes('正在读取') && h.get('modalContent').textContent.includes('正在读取'), 'pending status is synchronized, including opened detail');
    resolvePending(status); await pending;
    ok(h.get('modalContent').textContent.includes('已配置'), 'open details refresh when shared status resolves');
    for (const failure of [{ ...status, __httpStatus: 503 }, { ...status, code: 403 }, { success: false, msg: 'rawtoken-service-error' }, { ...status, __invalidJson: true }, { success: true, data: [] }, null]) {
        h.answer(() => failure === null ? Promise.reject(new Error('rawtoken-network-error')) : failure);
        await c.loadAllLoginStatus();
        ok(h.get('cloud189-status-badge').textContent === '状态读取失败，请重试' && h.get('c189StatusText').textContent === '状态读取失败，请重试', 'HTTP/body/JSON/network failures are visible in both views');
        ok(h.get('cloud189-status-source').textContent === h.get('cloud189-account-meta').textContent && !h.get('modalContent').textContent.includes('rawtoken') && !h.get('modalContent').textContent.includes('已配置'), 'failure clears stale safe metadata and updates detail without raw error');
        ok(h.get('c189LogoutButton').disabled && h.get('mcloudLogoutButton').disabled, 'uncertain account state cannot be used as a confirmed logout target');
    }
    let resolveOld;
    h.answer(() => new Promise(resolve => { resolveOld = resolve; })); const old = c.loadAllLoginStatus();
    h.answer(() => ({ success: true, data: { cloud189: { configured: false }, mcloud: { configured: false } } })); await c.loadAllLoginStatus();
    resolveOld(status); await old;
    ok(h.get('cloud189-status-badge').textContent === '未授权' && h.get('c189StatusText').textContent === '未授权', 'old successful status cannot overwrite newer logout state');
    let rejectOld;
    h.answer(() => new Promise((resolve, reject) => { rejectOld = reject; })); const oldError = c.loadAllLoginStatus();
    h.answer(() => status); await c.loadAllLoginStatus(); rejectOld(new Error('old-error-rawtoken')); await oldError;
    ok(h.get('cloud189-status-badge').textContent === '已配置' && h.get('c189StatusText').textContent === '已配置', 'old failure cannot overwrite newer success');
    h.answer(() => new Promise(resolve => { resolveOld = resolve; })); const stale = c.loadAllLoginStatus();
    h.answer(() => ({ __httpStatus: 500, ...status })); await c.loadAllLoginStatus(); resolveOld(status); await stale;
    ok(h.get('cloud189-status-badge').textContent === '状态读取失败，请重试', 'old success cannot conceal the latest read failure');
    h.answer(() => status); const before = h.requests.length; await c.checkC189Status();
    ok(h.requests.length === before + 1 && h.requests.at(-1).fields.action === 'login_status', 'legacy refresh entry delegates to same single status read, not a second endpoint');
}
// Real mutation hooks (not stubbed) refresh both views with exactly one shared status request.
for (const provider of ['cloud189', 'mcloud']) {
    const h = harness('admin'), c = h.context, p = provider === 'cloud189' ? 'c189' : 'mcloud';
    c.loadAllLoginStatus = h.originalLoadAllLoginStatus;
    let configured = false, source = 'qrcode';
    h.answer(req => {
        const action = req.fields.action;
        if (action === 'login_status') return { success: true, data: { cloud189: { configured: provider === 'cloud189' && configured, source }, mcloud: { configured: provider === 'mcloud' && configured, source } } };
        if (action.endsWith('_qrcode')) return qr;
        if (action.endsWith('_check')) { configured = true; return { success: true, data: { status: 'confirmed' } }; }
        if (action.endsWith('_manual_save')) { configured = true; source = 'manual'; }
        if (action.endsWith('_logout')) configured = false;
        return { success: true };
    });
    const synchronized = label => h.get(provider + '-status-badge').textContent === label && h.get(provider === 'cloud189' ? 'c189StatusText' : 'mcloud-qr-status').textContent === label && h.get(provider + '-status-source').textContent === h.get(provider + '-account-meta').textContent;
    await c.startAuthQR(provider); await h.advance(3000);
    ok(synchronized('已配置') && h.requests.filter(r => r.fields.action === 'login_status').length === 1, 'QR confirmation synchronizes both views with one status request');
    ok(!h.get(p + 'LoginSuccess').classList.contains('hidden') && !h.get(p + '-qr-container').classList.contains('hidden') && h.get(p + 'QrcodeImage').textContent === '', 'success UI is consistent and old QR session image is cleared');
    const input = h.get(provider === 'cloud189' ? 'c189TokenInput' : 'mcloudAuthInput'); input.value = 'synthetic-not-a-credential';
    await (provider === 'cloud189' ? c.saveC189ManualLogin() : c.saveMCloudManualLogin());
    ok(input.value === '' && synchronized('已配置') && h.get(provider + '-account-meta').textContent.includes('手动授权'), 'manual save clears secret and synchronizes source in both views');
    ok(h.requests.filter(r => r.fields.action === 'login_status').length === 2, 'manual save performs one shared refresh');
    const before = h.requests.length; c.confirmAnswer = false;
    await (provider === 'cloud189' ? c.logoutC189() : c.logoutMCloud());
    ok(h.requests.length === before && synchronized('已配置'), 'canceling logout confirmation leaves both views and server untouched');
    c.confirmAnswer = true; await (provider === 'cloud189' ? c.logoutC189() : c.logoutMCloud());
    ok(synchronized('未授权') && h.get(p + 'LoginSuccess').classList.contains('hidden') && h.requests.filter(r => r.fields.action === 'login_status').length === 3, 'logout synchronizes empty state with one shared refresh and removes success banner');
    h.answer(() => ({ success: true, __httpStatus: 500, msg: 'rawtoken-operation-error' })); input.value = 'synthetic-not-a-credential';
    await (provider === 'cloud189' ? c.saveC189ManualLogin() : c.saveMCloudManualLogin());
    ok(input.value !== '' && synchronized('状态读取失败，请重试') && h.alerts.at(-1) === '保存失败，请重试', 'HTTP save failure does not falsely clear input, claim success or expose raw error');
    h.answer(() => ({ ...qr, __httpStatus: 500 })); await c.startAuthQR(provider);
    ok(h.timers.size === 0 && !h.alerts.at(-1).includes('rawtoken'), 'HTTP QR acquisition failure stops all timers and exposes no service error');
    h.answer(() => qr); await c.startAuthQR(provider); c.switchPage('records');
    ok(h.timers.size === 0 && h.get(p + 'QRCode').classList.contains('hidden'), 'page switch cancels QR clock and serial polling immediately');
}
console.log(`PASS: ${checks} frontend syntax/DOM/form/poll/settings/XSS/search/mobile/local-QR checks; offline synthetic inputs only.`);
