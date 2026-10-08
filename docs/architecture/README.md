# Architecture documents

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
