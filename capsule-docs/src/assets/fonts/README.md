# Fonts

Geist and Geist Mono **1.9.2**, the variable cuts, copied unmodified from the
[`geist`](https://www.npmjs.com/package/geist) npm package
(`dist/fonts/geist-{sans,mono}/`), which [vercel/geist-font](https://github.com/vercel/geist-font)
publishes. `Geist-Italic-Variable.woff2` is upstream's `Geist-Italic[wght].woff2`, renamed so the
URL needs no escaping. Licence: [`OFL.txt`](OFL.txt) (OFL-1.1); attribution is in the repo-root
`NOTICE`.

`src/styles/global.css` declares them with `@font-face`. To update, replace the three files from
the new release's npm tarball and change the version here, in `global.css`'s comment and in
`NOTICE`.
