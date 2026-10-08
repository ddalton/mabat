# The Mabat documentation site

Built with [Starlight](https://starlight.astro.build). The guides are in `src/content/docs`; the MPA specification,
the design document, the changelog and the reference pages are written from the repository's files at build time by
`scripts/sync.mjs`, so they are never edited here.

```sh
npm ci
npm run dev      # http://localhost:4321/mabat/
npm run build    # into dist/
npm run check    # every internal link and anchor of dist/ resolves
```

The `Docs` workflow builds the site on every change. It publishes to GitHub Pages only when run by hand with
"deploy" checked.
