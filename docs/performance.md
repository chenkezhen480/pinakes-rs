# Pinakes: performance measurements

Every speed and memory figure behind the decisions on search, gathered in one place (2026-10-12).
How search works is in [guide.md](guide.md) ("Searching"); this file keeps the numbers, how
they were taken, and how to take them again.

## The machine

Development laptop: Intel i5-11500H (6 cores, 12 logical CPUs), 16 GB, Windows 11; release
builds; the service's compute budget of 4 threads (`[server] threads`, the default). Windows
Defender is on, which matters (see "Measurement notes").

## Where a search spends its time

A search at 100,000 pages (about 400,000 page vectors), median:

| Step | Time | Notes |
|---|---|---|
| Name matching, prepare, relations walk, assembling | under 0.3 ms | at every size |
| Keyword search (BM25, both slots) | 10 to 13 ms | 2.3 ms at 10,000 pages |
| Embedding the query (8-bit model) | 14 to 16 ms | 29 ms with the fp32 model; independent of the wiki's size |
| Vector search, graph index, both slots | about 3 ms | exact scan instead below 100,000 rows: 6 ms at 10,000 pages |

So a semantic search is mostly the query's embedding, and the vector index is a small part of it.

## How many vectors a wiki has

Each page gets one vector for its head (title, aliases, keywords, summary) and one per section
with text, so a page has about 4: the benchmark's 100,000 pages have **400,027 vectors**
(50,000 pages: 199,834; 10,000 pages: 39,959). In memory a vector is 768 signed bytes plus a
scale (int8): **about 300 MB for 400,000**; the graph index over them adds **107 MB** (the
file, 60 MB, holds only neighbour lists and 64-bit keys). SQLite keeps the float32 values,
about 1.2 GB at that size.

## The vector index: our HNSW against existing libraries

Spike of 2026-10-12 (branch `hnsw-spike`, `spike/hnsw-bench/`, with its raw results): could an
in-process Rust vector index replace `search::hnsw`?

**How.** Real 768-dimension text vectors, not synthetic ones: Czech Wikipedia paragraphs embedded
with `intfloat/multilingual-e5-base` (Hugging Face dataset `karmiq/wikipedia-embeddings-cs-e5-base`,
CC BY-SA), a page's paragraphs standing for a page's units (2.8 to 3.2 a page). 200,000, 299,011
and 400,000 rows; 1,000 queries from pages not in the index; truth is the exact top 10 by float
dot product, and recall@10 is measured against it. Each library in its own process, built on 4
threads with the same graph parameters (16 neighbours, 32 on the lowest layer, 200 candidates
while building); latency per query on one thread. Then: the search filtered to a fifth of the
pages (a caller's audiences), save and load, and removing a tenth of the pages' rows (no removed
row may come back; none did, for any library).

**400,000 rows** (about 100,000 pages):

| | Build | Memory added | File | Remove 40,000 rows | Best recall@10 | Recall 0.95: p50 / p95 |
|---|---|---|---|---|---|---|
| Exact scan, int8 (below the threshold) | none | 0 | none | instant | 0.965 | 23 ms (1 thread) |
| **Ours** (`search::hnsw`) | 138 s | 107 MB | 60 MB | 4.8 s, rows gone | 0.963 | **0.68 / 0.95 ms** (width 200) |
| Ours, f16 re-rank | 136 s | 107 MB + 600 MB copy | 60 MB | 4.2 s | 0.994 | 0.90 / 1.18 ms |
| usearch, f32 | 278 s | 1,332 MB | 1,288 MB | 0.01 s, marked only | 0.997 | 0.62 / 0.82 ms (width 64) |
| hannoy (LMDB) | 771 s | 1,786 MB | 1,720 MB | 33 s (rebuild) | 0.995 | 2.07 / 3.01 ms (width 100) |

Filtered to a fifth of the pages, recall 0.95 takes 0.9 to 1.2 ms for ours, 1.2 to 1.4 ms for
usearch-f32, 2.1 to 2.7 ms for hannoy.

**200,000 and 299,011 rows**, recall 0.95, p50 (memory added):

| | 200,000 | 299,011 |
|---|---|---|
| Ours | 0.55 ms (46 MB) | 0.38 ms (79 MB) |
| usearch, f32 | 0.58 ms (666 MB) | 0.68 ms (991 MB) |
| usearch, its own int8 | never (best 0.874) | never (best 0.880) |
| hannoy | 1.10 ms (907 MB) | 1.28 ms (1,338 MB) |
| hnsw_rs | 1.77 ms (1,141 MB) | 1.69 ms (1,709 MB) |

**Decided: keep our own HNSW.** It reaches a given recall about as fast as the fastest library,
holds 12 to 17 times less memory (it keeps only neighbour lists over the snapshot's own rows; the
others keep their own copy of every vector), builds 2 to 6 times faster, and really removes
deleted rows. usearch's SIMD library (numkong 7.8.5) doesn't compile with MSVC and its int8 caps
recall at 0.88; hannoy is a second store with slow builds and rebuilds on removal; hnsw_rs
panicked with its dot-product distance and can't remove points. SeekStorm is a whole search engine
(its vectors in a leveled IVF), LEANN has no Rust port and costs seconds a query, Capsa keeps
vectors in libSQL and embeds over HTTP.

**Quantization depends on the model.** e5's vectors are anisotropic (two random paragraphs have
a cosine of 0.77), so per-row int8 caps recall at 0.965 even for an exact scan; subtracting a mean
vector first raises it to 0.985. jina-embeddings-v2-base-zh, the service's model, is nearly
isotropic (random cosine 0.097): per-row int8 keeps recall@10 at 0.989 on the test set's 906
lines, so nothing was changed.

**On the service's own model** (`meaning_testset`, real vectors): the graph against the exact
scan on 3,093 pages (10,881 rows) gives recall@10 0.993 and the same first page 423 times of 424
(8-bit model; 0.983 on 17,841 rows with the fp32 model in M9).

## The query embedding

The fp32 model reads its 641 MB of weights for every query, so it is bound by memory bandwidth:
30 ms with 2, 4, 6 or 8 threads (42 with 12). Measured on the test set (111 pages, 375 units,
212 questions, 159 of them naming the page they expect), 4 threads:

| Model | Query p50 / p95 | Expected page first / in top 5 | Same first page as fp32 | Cosine to fp32 |
|---|---|---|---|---|
| fp32 `model.onnx` (until 2026-10-12) | 27.8 to 30.0 / 33.8 to 37.4 ms | 139 / 156 | | |
| **8-bit weights, int8 compute** (`MatMulNBits`, blocks of 32, accuracy level 4; now) | **13.7 / 19.3 ms** | **139 / 156** | 206 / 212 | 0.998 |
| 4-bit weights, same | 13.6 / 21.1 ms | 139 / 156 | 196 / 212 | 0.924 |
| 8-bit weights, fp32 compute | 249 ms | 139 / 156 | 211 / 212 | 0.9996 |
| The publisher's `model_quantized.onnx` | 10.5 / 13.5 ms | 125 / 154 | 160 / 212 | 0.73 |
| Dynamic int8 | 11.0 / 14.4 ms | 127 / 157 | 170 / 212 | 0.75 |

The 8-bit model takes 19, 16, 13 and 14 ms with 2, 4, 6 and 8 threads.

**Checked again in the service** (fastembed 7.1, onnxruntime 1.28, 4 threads; `tests/quant_compare.rs`
on the spike branch): every question three times, alternating the models: fp32 29.3 ms (p95
36.7), **8-bit 13.9 ms (p95 20.2)**; cosine to the fp32 vector 0.9993 on average for queries
(lowest 0.9967) and 0.9986 for page sections (lowest 0.9926). Every question asked of two
services, one per model: `semantic` 175/212 for both, `both` and `auto` 177/212 for both, no
question judged differently; the same first page in 364 of 369, 381 of 384 and 379 of 382 slots.

**Page embedding** is bound by computing, not by reading weights: 20.3 sections a second with
fp32 against 28.4 with 8 bits on the test set's sections (1.4 times), 22 a second on the
benchmark's longer filler pages.

**Memory of the loaded model**: 682 MB with fp32, **390 MB** with 8 bits (16 MB more while
embedding). On disk the service keeps only the 8-bit file, 316 MB.

**The quantizer** (`embed::quantize`) runs in 0.9 s; all 256 tensors of its result equal those
onnxruntime's own Python quantizer makes (`MatMulNBitsQuantizer`, `bits=8, block_size=32,
is_symmetric=True, accuracy_level=4`); the result is pinned by SHA-256.

## End to end

`tests/bench.rs`, with every search's query also embedded by the real 8-bit model
(`PINAKES_TEST_EMBEDDING=1`), so the times include it. Medians (95th percentile):

| Pages | Keyword search | Semantic | Both |
|---|---|---|---|
| 10,000 | 2.3 ms (3.7) | **20.0 ms (24.1)**: embedding 13.3, exact scan 5.8 | 21.5 to 22.5 ms (28.4 to 29.2) |
| 100,000 | 13.3 ms (28.3) | 37.6 ms (60.4): embedding 33.2, graph 2.9 | 38.5 to 41.1 ms (64.7 to 66.0) |

The 100,000-page row is slowed by Windows Defender (see below): the embedding took 28 to 33 ms
there against 15 to 16 ms alone. On a machine not busy scanning, a semantic search at 100,000
pages is the embedding plus about 3.4 ms, **about 18 to 20 ms**.

Before the 8-bit model (fp32, M9 benchmark): semantic search about 31 ms at 50,000 pages and 33
to 34 ms at 100,000 (the embedding measured alone and added); about 37 ms at 10,000 pages.

**The query embedded alongside the keywords** (`both` mode, `par::join`), against one after the
other, in the same process:

| Pages | Side by side | One after the other | Saved |
|---|---|---|---|
| 10,000 | 22.5 and 21.5 ms (p95 29.2, 28.4) | 23.4 ms (p95 28.4) | 1 to 2 ms |
| 100,000 | 41.1 and 38.5 ms (p95 64.7, 66.0) | 47.2 ms (p95 75.4) | about 7 ms (p95 about 10) |

At 10,000 pages the keyword search takes 3 ms, and sharing the machine slows the embedding by 1
to 2 ms, so little is saved; at 100,000 pages a keyword search of 12 ms is hidden.

Other figures of the same runs: a one-page write 0.17 to 0.19 s at 10,000 pages and 1.2 to 1.4 s
at 100,000; reopening a tenant (its vectors from SQLite) 3 s at 10,000 pages and 25 to 27 s at
100,000.

## Against "100 million vectors in 1 ms"

Vector databases such as Milvus publish figures like that. They measure something else:

| | Typical published figure | Ours |
|---|---|---|
| What is timed | The vector search alone; query vectors computed beforehand | The whole search, embedding the query included (14 ms of it) |
| Dimensions | Often 96 or 128 (Deep1B, SIFT) | 768, six to eight times the work per comparison |
| Hardware | Servers, data split across query nodes searched in parallel, sometimes GPU indexes | One laptop, one thread per query |
| Compression | Product quantization, or recall lowered for speed | int8, recall 0.95 and up |
| Latency | Often throughput under load, or batch averages | One request's median and 95th percentile |

Timed the same way, the vector search alone, ours takes 0.68 ms at 400,000 768-dimension vectors
on one thread. HNSW's cost grows roughly with the logarithm of the size, so 100 million would cost
perhaps one to two times that, if it fit in memory: 100 million 768-dimension vectors are 307 GB
as floats and 77 GB as int8, plus about 20 GB of graph, which is why such systems shard across
machines. 100 million vectors would be a tenant of 25 million pages; Pinakes is built for one
tenant in one process, up to about 100,000 pages, where the vector search is under a millisecond
and the query's embedding is what is left.

## Measurement notes

- **Windows Defender** scans the files a benchmark has just written (100,000 pages and their
  history objects) for minutes, using 2 to 3 logical CPUs (sampled with
  `typeperf "\Process(MsMpEng)\% Processor Time"`). onnxruntime's four threads wait for the
  slowest, so the embedding doubles, while keyword search and graph walks hardly change. Holding
  7 GB more memory in the process left the embedding at 15 ms, so memory is not the cause.
- The first history save of a large wiki writes every page (about 2.5 minutes at 100,000 pages)
  and slows everything meanwhile: `PINAKES_BENCH_SETTLE=240` waits for it.
- Don't build or run other heavy work during a benchmark. The 100,000-page benchmark needs
  several GB of memory; with WSL holding 7.6 GB besides, the machine ran out.

## Taking the numbers again

- End to end: `PINAKES_TEST_EMBEDDING=1 PINAKES_BENCH_PAGES=10000,100000 PINAKES_BENCH_SETTLE=240
  cargo test --release --test integration bench:: -- --ignored --nocapture` (about 25 minutes;
  without `PINAKES_TEST_EMBEDDING` the semantic times leave out the embedding).
- Answers and recall with the real model: `PINAKES_TEST_EMBEDDING=1 cargo test --release --test
  integration meaning_testset:: -- --nocapture` (about 9 minutes; `PINAKES_TEST_GRAPH_ROWS=1` forces
  the graph index).
- The library comparison and the model comparison: branch `hnsw-spike`, `spike/hnsw-bench/`
  (`REPORT.md`, `scripts/`, `results/`).
