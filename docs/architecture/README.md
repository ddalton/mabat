# Architecture documents

| Read it | Where |
| --- | --- |
| **On GitHub** | [`overview/mabat-architecture.pdf`](overview/mabat-architecture.pdf), which GitHub shows page by page |
| **In the browser** | the [documentation site](https://ddalton.github.io/mabat/architecture/): a page per section, and the [whole document as HTML](https://ddalton.github.io/mabat/architecture/mabat-architecture.html) |
| **To print or share** | the PDF, also offered for download on every architecture page of the site |

GitHub shows `.html` files as source, so the HTML is read on the documentation site, which serves it as
designed: each A3 page a sheet, with a link to the PDF.

## overview — how Mabat loads, serves and saves typed aggregates

**`overview/mabat-architecture.pdf`** (12 pages, A3 landscape). Mabat at a glance; a view as a shape; the plan of
a view and its SQL; overrides and their checks; trees, shared values, graphs and recursion; concurrency on a pool
and a snapshot; JSON and GraphQL; writes and optimistic locking; change tracking against an ORM; the three
databases; and the contract. Every claim cites its rule of the [MPA specification](../mpa.md).

The ten figures are Visio pages in `overview/mabat-architecture.vsdx`, with an EMF per figure for Office. They are
drawn by `overview/mabat-visio.py` with the poster kit in this directory (`vsdxkit.py`, `dataflowkit.py`,
`vsdxemf.py`, from flint's documentation). The last figure is read from [`docs/mpa.json`](../mpa.json) at build
time, so it cannot drift from the specification's index.

```sh
docs/architecture/overview/build.sh              # gate the figures, render the PDF, check one page per section
docs/architecture/overview/build.sh --check      # gates and references only; no Chrome
docs/architecture/overview/build.sh --geometry   # measure every line of figure text against its box in Chrome
```

Edit the HTML (all prose and layout) or the script (all figures); the SVGs, EMFs, `.vsdx` and PDF are
regenerated. The documentation site (`docs/site`) publishes the same pages, with the figures and the PDF, from the
HTML at build time.
