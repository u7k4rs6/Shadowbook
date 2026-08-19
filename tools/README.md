# tools

## build_assets.py

Generates the README section graphics in `assets/`: `highlights.svg`,
`architecture.svg`, `verification.svg`, `benchmark.svg`, `findings.svg`.

```sh
python3 tools/build_assets.py
```

It writes straight into `assets/`, so the committed SVGs are this script's
output and nothing else. If you change a published number, change it here and
re-run — editing the SVG by hand puts the artwork and its producer out of sync,
which is the exact failure mode `FINDINGS.md` is about.

Where the numbers live:

| Figure | Function | What it shows |
|---|---|---|
| p50 / p99 / p99.9 insert latency | `benchmark()`, `marks` | must match the `insert_no_cross` table in `BENCH.md` |
| p50 count-up and subtitle | `highlights()`, third column | same figures, animated |
| operation count, divergences | `highlights()`, first two columns | must match the run in `FINDINGS.md` |

`benchmark()`'s `axis_max` is the axis top and must stay above the widest mark;
the four tick labels are derived from it.

### Fonts

The graphics embed a subset of Jost as base64 so they render on GitHub without a
network fetch. Subsetting needs `fontTools` and a copy of the Jost woff2 files;
because neither is normally present, the three subsets are committed in
`jost-faces.json` and used automatically. The subsetting is deterministic, so the
cache reproduces byte-identical SVGs.

To re-subset from source, point `JOST_FONTDIR` at a directory holding
`jost-latin-{300,400,500}-normal.woff2`; the cache is rewritten as a side effect.

### Not generated here

`assets/banner.svg` is the hero image and is not produced by this script. It is
maintained by hand and carries the p50 figure in both its artwork and its
`aria-label`; both need updating alongside `BENCH.md`.
