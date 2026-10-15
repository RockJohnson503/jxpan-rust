# Vendored QR encoder

- Package: `qrcode-generator` **2.0.4**, by Kazuhiko Arase, MIT.
- Upstream repository: https://github.com/kazuhikoarase/qrcode-generator
- Package manifest/API reference: https://github.com/kazuhikoarase/qrcode-generator/blob/master/js/package.json and the package's `README.md`.
- Exact npm source archive: https://registry.npmjs.org/qrcode-generator/-/qrcode-generator-2.0.4.tgz
- npm `gitHead`: `83b7e8fe3fddd3b0368dbafd6ce56995bd25e3c8`.
- npm archive integrity (also pinned in `tools/package-lock.json`): `sha512-mZSiP6RnbHl4xL2Ap5HfkjLnmxfKcPWpWe/c+5XxCuetEenqmNFf1FH/ftXPCtFG5/TDobjsjz6sSNL0Sr8Z9g==`.
- `qrcode.js` is an unmodified, byte-for-byte copy of `dist/qrcode.js` from that npm package. Its original copyright/license header is retained.
- Full MIT license: `qrcode.LICENSE`, from https://github.com/kazuhikoarase/qrcode-generator/blob/83b7e8fe3fddd3b0368dbafd6ce56995bd25e3c8/LICENSE (the npm archive contains no standalone LICENSE file).

SHA-256:

```text
79ec86f82856005b1c887905cfccfcfbec3821ca61c7fd5a952faa5f778f791c  qrcode.js
3a850fa5f08101db6f40676c2786e10bd2cd5fff7b12ffdf1e0c434d4e49d90c  qrcode.LICENSE
```

The HTML pages load this encoder from same-origin `/assets/qrcode.js`. They set the included UTF-8 encoder, then call `qrcode(0, 'M').addData(value)`, `make()`, and `createDataURL(4, 16)`. QR session URLs are encoded locally as `data:image/gif;base64,...`; no third-party QR image request or remote fallback is used. Existing 180/200-pixel display dimensions are retained.

`tools/verify-frontend.mjs` checks these hashes and executes the actual vendored script in its browser-like VM. `qrcode-generator` is an exact-pinned tools dev dependency for reproducible provenance only; production rendering needs the vendored file, not npm or a CDN. This does not vendor the pages' unrelated, pre-existing CSS/chart/crypto CDNs.
