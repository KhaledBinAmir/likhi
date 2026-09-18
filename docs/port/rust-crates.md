# Likhi engine: Rust crate and technique selection

**Status:** research spec for the Python→Rust port of the Likhi engine.
**Written:** 2026-09-18.
**Audience:** the Rust implementer. This is meant to be read end to end before the first line of
`Cargo.toml` is written.

Everything in this document that is stated as a number was **measured on this machine today**, not
recalled. Where I could not measure, the claim is marked and moved to [§11 Uncertain](#11-uncertain).
Where a decision is counter-intuitive — and two of them are — the measurement that forced it is
reproduced inline.

---

## 0. The one-paragraph answer

Do **not** read the `.npz` at runtime; convert to a flat f32 blob at build time and `mmap` it
(74 ms → 10 ms on x86_64, 115 ms → 10 ms on i686). Do **not** use `matrixmultiply` or `ndarray` for
the decoder step; at beam=4 they are *slower than a plain Rust triple loop*, by up to 8.2× on i686 —
write one hand-rolled AVX+FMA kernel specialised for m=4 and keep `matrixmultiply` only for the
teacher-forced scoring pass where m≈192. Use `fst` (or `rsmarisa`, with caveats) rather than a naive
sorted table, because a flat table costs 57.8 MB against marisa's 12.0 MB for the same data. Use
`rusqlite` with `bundled` — it costs 1.26 MiB (i686) / 1.57 MiB (x86_64) and there is no
alternative on Windows. Use `ureq` with `native-tls` (→ SChannel), **not** rustls: 448 KiB instead
of 1.04 MiB, it uses the Windows certificate store, and it needs no vendored crypto. None of these
are i686 cross-compilation hazards: every crate below was built for `i686-pc-windows-msvc` on this
machine with nothing installed beyond VS 2022 Build Tools.

---

## 1. Measurement environment

Everything below was produced on:

| | |
|---|---|
| CPU | Intel Core i9-10900K @ 3.70 GHz, 10C/20T, Comet Lake |
| SIMD available | `sse2=true avx=true fma=true avx2=true` (**no AVX-512** — Comet Lake has none) |
| RAM | 31.8 GB |
| C: | Samsung SSD 970 EVO Plus 250 GB, NVMe |
| OS | Windows 11 Enterprise 10.0.26200 |
| rustc | `1.98.1 (48a229cea 2026-09-01)` |
| cargo | `1.98.1 (797e8a9bc 2026-08-05)` |
| rustup | `1.29.1 (d95a37b6a 2026-08-13)` |
| targets installed | `i686-pc-windows-msvc`, `x86_64-pc-windows-msvc` |
| toolchain | `stable-x86_64-pc-windows-msvc` |
| MSVC | VS 2022 Build Tools at `C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools` |
| **not** installed | `nasm`, `cmake`, `perl`, `clang` — none are on PATH or anywhere on C: |
| Python baseline | `C:\Users\khaled\src\likhi\.venv\Scripts\python.exe`, NumPy from the bundled runtime |

The "not installed" row matters: it is what makes the cross-compilation claims in §8 *tested* rather
than assumed. Every Rust build below succeeded with exactly this toolchain.

**Caveat that applies to every timing in this document:** the i9-10900K is a desktop CPU with AVX2
and FMA. A pilot user on an older Core i5 without FMA, or on a machine that only has SSE2, will not
see these numbers. §4.6 specifies the required runtime fallback.

**Caveat on "best of N":** all Rust timings are best-of-9-batches of 3000 reps (or best-of-7 × 300
for the large shapes), warm caches, warm branch predictors. They are a *floor*, not a p95. Treat
them as ratios between techniques, which is what they are used for here, not as the latency the IME
will show.

---

## 2. Verified version table

Every version below was read from `crates.io` today (2026-09-18) or from the `Cargo.lock` that
`cargo` produced on this machine. Release dates are the release date of the listed version.

| Crate | Version | Released | License | Role | i686 OK? |
|---|---|---|---|---|---|
| `memmap2` | **0.9.11** | 2026-06-22 | MIT/Apache-2.0 | mmap the weight blob and the lexicon | ✅ built |
| `bytemuck` | **1.25.2** | 2026-07-19 | Zlib/Apache-2.0/MIT | zero-copy `&[u8]` → `&[f32]` | ✅ built |
| `half` | **2.7.1** | 2025-10-14 | MIT/Apache-2.0 | f16 → f32 at build/first-run | ✅ built |
| `matrixmultiply` | **0.3.11** | 2026-07-14 | MIT/Apache-2.0 | sgemm for the **scoring** pass only | ✅ built |
| `ndarray` | **0.17.2** | 2026-01-10 | MIT/Apache-2.0 | *not recommended* (see §4.4) | ✅ built |
| `fst` | **0.4.7** | 2021-06-06 | MIT/Unlicense | lexicon replacement for marisa | ✅ built |
| `rsmarisa` | **0.4.2** | 2026-06-01 | BSD-2-Clause | marisa-format reader (risky, see §5.4) | ⚠️ untested |
| `zip` | **8.6.0** | 2026-04-25 | MIT | *only* if you insist on reading `.npz` | ✅ built |
| `flate2` | **1.1.10** | 2026-08-28 | MIT/Apache-2.0 | (pulled in by `zip`) | ✅ built |
| `rusqlite` | **0.40.2** | 2026-08-08 | MIT | personal store | ✅ built + ran |
| `libsqlite3-sys` | **0.38.2** | — | MIT | (pulled in by `rusqlite`) | ✅ built |
| bundled SQLite | **3.53.2** | — | public domain | — | ✅ verified at runtime |
| `ureq` | **3.4.2** | 2026-09-13 | MIT/Apache-2.0 | telemetry upload | ✅ built + live TLS |
| `native-tls` | **0.2.18** | — | MIT/Apache-2.0 | TLS via SChannel | ✅ built + live TLS |
| `schannel` | **0.1.29** | — | MIT | (pulled in by `native-tls`) | ✅ built + live TLS |
| `rustls` | 0.23.45 | — | MIT/Apache-2.0/ISC | *rejected*, see §7.3 | ✅ built |
| `ring` | 0.17.14 | 2026-08-09 | complex (see §7.4) | (pulled in by `rustls`) | ✅ built |
| `attohttpc` | 0.31.0 | 2026-05-25 | **MPL-2.0** | *rejected*, see §7.5 | not tested |
| `wide` | 1.7.1 | 2026-09-14 | Zlib/Apache-2.0/MIT | *not needed*, see §4.5 | not tested |
| `aws-lc-rs` | 1.18.1 | 2026-09-01 | ISC/Apache-2.0 | **do not use**, see §8.2 | ✗ hazard |

"✅ built" means: a crate depending on it compiled to a linked `.exe` for
`--target i686-pc-windows-msvc` on this machine, in this session.

---

## 3. Q1 — Reading the 20 MB `.npz` vs a build-time flat blob

### 3.1 What the file actually is

`dist/runtime/models/indicxlit-np/model.npz`, measured:

```
file size                21,297,857 bytes
zip members                     263  (one .npy per tensor)
compress_type                     8  (deflate) for every member
uncompressed total       23,009,152 bytes
```

The 33,664-byte gap between 23,009,152 and the sum of the tensor payloads is exactly
`263 × 128` — every `.npy` member carries a 128-byte header.

**Every tensor is stored as `<f2`, i.e. little-endian IEEE-754 binary16.** This is not obvious from
the Python and it is the first thing a naive port gets wrong: `xlit_np.py:154` reads

```python
f = {k: w[k].astype(np.float32) for k in w.files}
```

so the on-disk representation is f16 and the in-memory representation is f32. 11,487,744 parameters
→ **45,950,976 bytes of f32**.

Shapes (all little-endian, C-contiguous):

| Tensor family | Shape | Count |
|---|---|---|
| `encoder.embed_tokens.weight` | (54, 256) | 1 |
| `decoder.embed_tokens.weight` | (806, 256) | 1 |
| `decoder.output_projection.weight` | (806, 256) | 1 |
| `{enc,dec}.layers.N.{self_attn,encoder_attn}.{q,k,v,out}_proj.weight` | (256, 256) | 96 |
| `…_proj.bias` | (256,) | 96 |
| `…fc1.weight` | (1024, 256) | 12 |
| `…fc1.bias` | (1024,) | 12 |
| `…fc2.weight` | (256, 1024) | 12 |
| `…fc2.bias` | (256,) | 12 |
| every `layer_norm` / `layernorm_embedding` `.weight`/`.bias` | (256,) | 28 |

263 arrays total. `decoder.output_projection.weight` **is present as its own array** — do not assume
weight tying; `xlit_np.py:157` falls back to `dec_embed` only if the key is absent, and here it is
not absent.

### 3.2 Python baseline (what we are trying to beat)

Measured with the venv Python:

```
np.load (opens the zip, reads the central directory)     11.9 ms
decompress + f16->f32 for all 263 arrays                154.9 ms
XlitTransformer.__init__ total                          169.0 ms
```

169 ms of cold start before a single key can be transliterated. For a process that lives next to a
TSF DLL and must be ready before the user's first keystroke, that is the number to kill.

### 3.3 Rust measurements

Four strategies, same data, same machine, warm page cache, `opt-level=3 lto=true codegen-units=1`:

```
                                   x86_64        i686
npz: zip 8.6 + inflate + f16→f32   74.1 ms     115.3 ms
mmap 46 MB f32 blob (map call)      5.04 ms      0.097 ms
  + first-touch of every page      10.3 ms      10.2 ms
std::fs::read 46 MB f32 blob       15.5 ms      15.4 ms
mmap 23 MB f16 blob + convert      24.9 ms      21.4 ms
```

(The 5.04 ms vs 0.097 ms difference in the bare `map()` call is measurement order — x86_64 ran
first and paid the cold open. Treat the `map()` call itself as sub-millisecond on both.)

### 3.4 Recommendation

**Convert at build time to a flat, 64-byte-aligned f32 blob and `mmap` it.**

| Strategy | Startup | On-disk | RAM class |
|---|---|---|---|
| read `.npz` at runtime | 74–115 ms | 21.3 MB | 46 MB private, committed |
| `std::fs::read` f32 blob | 15.4 ms | 46.0 MB | 46 MB private, committed |
| mmap f16 blob + convert | 21–25 ms | 23.0 MB | 46 MB private, committed |
| **mmap f32 blob** | **~10 ms** | **46.0 MB** | **46 MB file-backed, clean** |

The last column is the argument that actually decides it, and it is not visible in the timings.
A `Vec<f32>` built by any of the first three strategies is *private committed* memory: it counts
against the system commit charge, it can be paged to the pagefile but never discarded, and every
instance of the process pays for its own copy. A file-backed read-only mapping is *clean*: the OS
can drop those pages under memory pressure and reload them from the file, it never touches the
pagefile, and the pages are shared between processes mapping the same file. For a project whose
standing instruction is lightweight-first, a 46 MB file-backed mapping is categorically cheaper
than a 46 MB heap allocation even though both report ~46 MB of working set on a quiet machine.

**The 46 MB on disk is the price.** Two ways to avoid paying it in the installer:

- Ship the 23 MB f16 blob and have first run write the f32 blob beside it as a cache. First run
  pays 21–25 ms, every subsequent run pays ~10 ms, the installer stays at 23 MB. This is the
  recommended shape if installer size is a hard constraint.
- Ship the f32 blob and let the installer's own compression handle it. f32 weights deflate to
  roughly the f16 size because the low mantissa bytes are zero after an f16→f32 widening — but
  I did **not** measure this, see §11.

### 3.5 Blob format the build step must emit

Do not invent an ad-hoc format. Specify it and check it:

```
offset 0    magic          "LIKHIW01"          8 bytes
offset 8    n_tensors      u32 LE              = 263
offset 12   flags          u32 LE              bit0: 0 = f32, 1 = f16
offset 16   index_offset   u64 LE
offset 24   blob_crc32     u32 LE              CRC-32 of the payload region
offset 28   _pad           u32                 → payload starts at 64
```

Payload: each tensor 64-byte aligned, C-contiguous, little-endian. Index: a JSON or
length-prefixed table of `(name, offset, ndim, dims[])`.

Three rules the implementer must not skip:

1. **Alignment.** `bytemuck::cast_slice::<u8, f32>` will **panic** if the source slice is not
   4-byte aligned. `memmap2` maps at a page boundary so offset 0 is fine, but every tensor's own
   offset must also be a multiple of 4. Use 64 to also get cache-line alignment for the SIMD
   kernels. The f32 export I generated needed **zero** padding bytes (45,950,976 exactly, because
   every tensor's row is 256 or 1024 f32 = 1024 or 4096 bytes) — but do not rely on that surviving
   a model change; write the padding logic anyway.
2. **Endianness.** Every source array is `<f2`/`<f4` — little-endian. Both targets are
   little-endian, so a plain cast is correct, but assert it (`cfg!(target_endian = "little")`) so
   the assumption is recorded.
3. **Validate before casting.** `mmap` of a truncated or corrupt file produces a valid `&[u8]` and
   an invalid `&[f32]`. Check `magic`, `n_tensors`, file length ≥ `index_offset`, and every
   tensor's `offset + len*4 ≤ file_len` before any cast. A wrong offset here is a silent read of
   garbage floats, which surfaces as bad suggestions, not as a crash.

### 3.6 mmap gotchas on Windows

- **A mapped file cannot be replaced or deleted.** Windows holds a section object; `MoveFileEx` and
  `DeleteFile` over the model file will fail with `ERROR_USER_MAPPED_FILE` while the IME is
  running. The updater must therefore either write the new model under a new name and switch a
  pointer, or stop the engine process first. This is a real deployment constraint, not a detail.
- **A truncated or vanished backing file turns a read into `EXCEPTION_IN_PAGE_ERROR`**, which is a
  structured exception Rust cannot catch and which will kill the process. This is why the model
  must live on local disk. Do **not** mmap a model on a network share or a OneDrive-synced folder.
  If the install location cannot be guaranteed local, fall back to `std::fs::read` (15.4 ms) which
  fails cleanly as an `io::Error`.
- `memmap2::Mmap::map` is `unsafe` for exactly these reasons. The `unsafe` block should carry a
  comment naming them.

### 3.7 If you read the `.npz` anyway

Only reason to: keeping one artefact that both Python and Rust consume during the transition.
`zip = { version = "8.6", default-features = false, features = ["deflate"] }` builds for i686 and
works. Parsing a `.npy` member: bytes 0–5 are `\x93NUMPY`, 6–7 are the version, and for v1.0 bytes
8–9 are a **little-endian u16 header length**; the array data starts at `10 + header_len`. The
header is a Python dict literal — `{'descr': '<f2', 'fortran_order': False, 'shape': (256, 256), }`.
Parse `descr` and `shape` properly; do not hardcode, and do **not** assume the 128-byte header is
universal (it is what NumPy pads to today, it is not in the format).

---

## 4. Q2 — f32 matmul for the 256-dim / 4-head / 6+6-layer transformer

**This is the section where the obvious answer is wrong.**

### 4.1 The shapes that actually occur

From `xlit_np.py`, with `dim=256`, `heads=4`, `head_dim=64`, `ffn=1024`, `|V_tgt|=806`,
`|V_src|=54`, beam=4:

| Where | Operation | m | k | n | Times per word |
|---|---|---|---|---|---|
| `_decode_step` `_qkv` (`:249`) | `h @ a.wqkv` fused q\|k\|v | 4 | 256 | **768** | 6 layers × ~8 steps |
| `_decode_step` self-attn out (`:323`) | `@ a.wo` | 4 | 256 | 256 | 6 × 8 |
| `_decode_step` cross q (`:329`) | `h @ c.wq` | 4 | 256 | 256 | 6 × 8 |
| `_decode_step` cross out (`:331`) | `@ c.wo` | 4 | 256 | 256 | 6 × 8 |
| `_decode_step` ffn (`:337`) | `@ fnn.w1` | 4 | 256 | **1024** | 6 × 8 |
| `_decode_step` ffn (`:337`) | `@ fnn.w2` | 4 | 1024 | 256 | 6 × 8 |
| `_decode_step` logits (`:343`) | `x @ out_proj.T` | 4 | 256 | **806** | 1 × 8 |
| `_encode` (`:281`) | fused qkv | T≈12 | 256 | 768 | 6 × 1 |
| `_encode` (`:288`) | ffn | T≈12 | 256 | 1024 / 1024→256 | 6 × 1 |
| `_encode` (`:298`) | cross-attn K,V prep | T≈12 | 256 | 256 | 12 × 1 |
| `_decode_full` (`:360–381`) | teacher forcing, B=16 words × T≈12 | **192** | 256 | 768/1024/806 | 6 × 1 |

So the engine runs **two completely different matmul regimes**:

- **m = 4** (the beam decode step) — thin, called ~48–72 times per word, dominates latency.
- **m ≈ 192** (`score_candidates`, `core.py:443`, `model_scored=16` by default, `core.py:220`)
  — fat, called once per word.

A single gemm choice cannot serve both. **The port must use two kernels.**

### 4.2 Measured: four techniques, both targets

Row-major A (m×k), row-major B (k×n), C = A·B. Best of 9 batches × 3000 reps, in **milliseconds**.

**x86_64-pc-windows-msvc**

| Shape | naive loops | `matrixmultiply` | AVX+FMA axpy | **hand m=4 AVX+FMA** |
|---|---|---|---|---|
| dec fused qkv `4×256×768` | 0.0802 | 0.0594 | 0.0468 | **0.0296** |
| dec out_proj `4×256×256` | 0.0237 | 0.0218 | 0.0168 | **0.0090** |
| dec fc1 `4×256×1024` | 0.1135 | 0.3444 | 0.0730 | **0.0436** |
| dec fc2 `4×1024×256` | 0.1044 | 0.0816 | 0.0623 | **0.0459** |
| dec logits `4×256×806` | 0.0884 | 0.0549 | 0.0545 | **0.0339** |
| enc fused qkv `12×256×768` | 0.2386 | **0.0890** | 0.1621 | n/a |
| enc fc1 `12×256×1024` | 0.3458 | 0.3615 | **0.1974** | n/a |
| score qkv `192×256×768` | 3.9395 | **0.6266** | 2.4615 | n/a |
| score logits `192×256×806` | 4.2843 | **0.6417** | 2.6425 | n/a |

**i686-pc-windows-msvc**

| Shape | naive loops | `matrixmultiply` | AVX+FMA axpy | **hand m=4 AVX+FMA** |
|---|---|---|---|---|
| dec fused qkv `4×256×768` | 0.0817 | 0.2862 | 0.0533 | **0.0348** |
| dec out_proj `4×256×256` | 0.0245 | 0.0371 | 0.0154 | **0.0119** |
| dec fc1 `4×256×1024` | 0.1537 | 0.3919 | 0.0656 | **0.0484** |
| dec fc2 `4×1024×256` | 0.1078 | 0.1447 | 0.0784 | **0.0600** |
| dec logits `4×256×806` | 0.0930 | 0.3075 | 0.0642 | **0.0366** |
| enc fused qkv `12×256×768` | 0.2408 | 0.3710 | **0.1506** | n/a |
| enc fc1 `12×256×1024` | 0.3211 | 0.4841 | **0.1968** | n/a |
| score qkv `192×256×768` | 5.6280 | **1.8545** | 2.3119 | n/a |
| score logits `192×256×806` | 4.3303 | **2.0223** | 2.6475 | n/a |

### 4.3 What these numbers mean

1. **`matrixmultiply` is a pessimisation at m=4.** On i686 it is 8.2× slower than the hand kernel
   for the fused qkv (0.2862 vs 0.0348) and 8.4× slower for the logits (0.3075 vs 0.0366). It is
   even 3.5× slower than a *naive triple loop* (0.2862 vs 0.0817). The cause is structural, not a
   tuning problem: `matrixmultiply` packs both A and B into blocked buffers on every call. For
   `4×256×1024` that is 262,144 f32 of B-packing to produce 4,096 f32 of output. The packing is
   pure overhead and it is paid 48–72 times per word.
2. **`matrixmultiply` has no pre-packing API.** I checked the docs for 0.3.11: the entire public
   surface is `sgemm`, `dgemm`, `cgemm`, `zgemm`. There is no `pack` / `prepacked` entry point, so
   you cannot hoist the B-packing of a *constant weight matrix* out of the loop. That closes the
   only route by which it could have been made to work here.
3. **`matrixmultiply` is the right answer at m≈192**, by 3–6×. `score_candidates` runs once per
   word over 16 candidates; on x86_64 that is 0.63 ms instead of 2.46 ms (hand axpy) or 3.94 ms
   (naive). Keep it, for that call only.
4. **i686 is *not* penalised once you write the kernel yourself.** The hand m=4 kernel runs at
   0.0348 ms on i686 against 0.0296 ms on x86_64 — a 17% gap. `matrixmultiply` on the same shape
   is 4.8× slower on i686 than on x86_64 (0.2862 vs 0.0594). The reason is register pressure:
   32-bit x86 exposes 8 YMM registers, 64-bit exposes 16. `matrixmultiply`'s 8×8 AVX microkernel
   wants 16; the m=4 kernel below wants 5 (four accumulators plus one B vector) and fits
   comfortably in 8. **Write the kernel to fit in 8 YMM registers and the i686 build stops being
   the slow one.**
5. **Numerical agreement is not a concern.** The hand kernel's maximum absolute deviation from the
   naive loop across all five decode shapes was 1.192e-6, relative 4.86e-7. Candidate score gaps in
   this engine are ~0.5 (`xlit_np.py:48-50` says as much about the GELU approximation). This is six
   orders of magnitude below anything that could reorder a suggestion.

### 4.4 `ndarray` — do not use it for this

`ndarray` 0.17.2's `dot` for 2-D f32 delegates to `matrixmultiply`, so it inherits every problem
above and adds a thin wrapper cost. Measured side by side it was 0.0655–0.6479 on x86_64 where
`matrixmultiply` was 0.0630–0.6266, and 0.2867–2.6924 on i686 where `matrixmultiply` was
0.2862–2.4524 — i.e. the same or slightly worse, never better, on all 11 shapes.

The tensors in this model are 1-D, 2-D, and (in attention) logically 4-D `(B, heads, T, head_dim)`.
The 4-D shapes are produced by `_split`/`_merge` (`xlit_np.py:238-245`), which are pure
`reshape` + `swapaxes` — free in NumPy, and free in Rust too if you keep the data in one flat
`Vec<f32>` and index it yourself. `ndarray` buys you nothing here and costs a dependency, compile
time, and the temptation to allocate a fresh array per operation. **Use flat `Vec<f32>` /
`&mut [f32]` with explicit strides.**

### 4.5 SIMD approach: `std::arch` intrinsics, not the `wide` crate

`wide` 1.7.1 chooses its vector width from **compile-time** target features. On
`i686-pc-windows-msvc` and `x86_64-pc-windows-msvc` the baseline is SSE2, so `wide::f32x8` compiles
to two SSE2 registers and you get no AVX unless you also set `-C target-feature=+avx,+fma` — which
would make the binary crash on any machine without AVX. For a shipped IME that is unacceptable.

Use `core::arch` intrinsics inside a `#[target_feature(enable = "avx,fma")]` function, dispatched at
runtime by `is_x86_feature_detected!`. Verified working on **both** targets:

```rust
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64 as arch;
#[cfg(target_arch = "x86")]
use std::arch::x86 as arch;
```

`is_x86_feature_detected!` is available on `target_arch = "x86"` as well as `"x86_64"` — confirmed
by running the probe binary on i686, which printed `arch = x86 / avx=true fma=true avx2=true
sse2=true`.

### 4.6 The kernel to write

This is the exact code that produced the "hand m=4 AVX+FMA" column. It is reproduced in full
because paraphrasing it would lose the register-pressure property that makes it fast on i686.

```rust
/// C (4 x n, row-major) = A (4 x k, row-major) * B (k x n, row-major).
/// Four accumulators + one B vector = 5 YMM registers, so it fits in i686's 8.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx,fma")]
unsafe fn m4_avx_fma(k: usize, n: usize, a: &[f32], b: &[f32], c: &mut [f32]) {
    let (c0, rest) = c.split_at_mut(n);
    let (c1, rest) = rest.split_at_mut(n);
    let (c2, c3) = rest.split_at_mut(n);
    for r in [&mut *c0, &mut *c1, &mut *c2, &mut *c3] { r.fill(0.0); }
    let mut j = 0;
    while j + 8 <= n {
        let mut acc0 = arch::_mm256_setzero_ps();
        let mut acc1 = arch::_mm256_setzero_ps();
        let mut acc2 = arch::_mm256_setzero_ps();
        let mut acc3 = arch::_mm256_setzero_ps();
        for p in 0..k {
            let bv = arch::_mm256_loadu_ps(b.as_ptr().add(p * n + j));
            acc0 = arch::_mm256_fmadd_ps(arch::_mm256_set1_ps(*a.get_unchecked(p)), bv, acc0);
            acc1 = arch::_mm256_fmadd_ps(arch::_mm256_set1_ps(*a.get_unchecked(k + p)), bv, acc1);
            acc2 = arch::_mm256_fmadd_ps(arch::_mm256_set1_ps(*a.get_unchecked(2*k + p)), bv, acc2);
            acc3 = arch::_mm256_fmadd_ps(arch::_mm256_set1_ps(*a.get_unchecked(3*k + p)), bv, acc3);
        }
        arch::_mm256_storeu_ps(c0.as_mut_ptr().add(j), acc0);
        arch::_mm256_storeu_ps(c1.as_mut_ptr().add(j), acc1);
        arch::_mm256_storeu_ps(c2.as_mut_ptr().add(j), acc2);
        arch::_mm256_storeu_ps(c3.as_mut_ptr().add(j), acc3);
        j += 8;
    }
    while j < n {                       // scalar tail; n = 806 is not a multiple of 8
        let (mut s0, mut s1, mut s2, mut s3) = (0f32, 0f32, 0f32, 0f32);
        for p in 0..k {
            let bv = *b.get_unchecked(p * n + j);
            s0 = a.get_unchecked(p).mul_add(bv, s0);
            s1 = a.get_unchecked(k + p).mul_add(bv, s1);
            s2 = a.get_unchecked(2*k + p).mul_add(bv, s2);
            s3 = a.get_unchecked(3*k + p).mul_add(bv, s3);
        }
        c0[j] = s0; c1[j] = s1; c2[j] = s2; c3[j] = s3;
        j += 1;
    }
}
```

Notes that are load-bearing:

- `n = 806` (the logits width) is **not** a multiple of 8. The scalar tail is not optional.
- The B matrix is read **row-major, k×n**. The Python already stores the weights this way: every
  `.weight` is transposed once at load (`xlit_np.py:174-190`, `.T.copy()`), and `wqkv` is built with
  `np.ascontiguousarray` (`:181`). The build step must emit the **already-transposed, contiguous**
  matrices, and the fused `wqkv` with the q-scaling already folded in — see §9.3.
- `m` is exactly 4 only while all four beams are alive. `beam_search` (`xlit_np.py:493`) caps
  `new_tokens` at `beam`, but the beam can shrink when candidates are non-finite (`:484`) or
  finalise on EOS (`:486-491`). Write the kernel generically over `m ∈ 1..=4` (four separate
  monomorphised bodies, or one with a compile-time `const M: usize`), do not assume 4.
- The `#[target_feature]` function must be called under `is_x86_feature_detected!("avx") &&
  is_x86_feature_detected!("fma")`. Detect once at engine construction, store a function pointer,
  do not re-detect per call.

### 4.7 Required fallback chain

| Detected | Decode step (m≤4) | Scoring pass (m≈192) |
|---|---|---|
| AVX + FMA | `m4_avx_fma` | `matrixmultiply::sgemm` |
| AVX, no FMA | same kernel with `_mm256_add_ps(_mm256_mul_ps(..))` | `matrixmultiply::sgemm` |
| SSE2 only | naive axpy loop (LLVM autovectorises it; measured 0.0817 ms i686) | `matrixmultiply::sgemm` |

The SSE2 fallback is the plain triple loop from §4.2's first column — written so the inner loop is a
contiguous AXPY over `n`, which LLVM vectorises with the baseline ISA. Do not write the textbook
`for i { for j { for p { c[i][j] += a[i][p]*b[p][j] } } }`: that strides B by `n` and will not
vectorise.

`matrixmultiply` does its own runtime detection, but **only with the `std` feature enabled** — the
docs state "Runtime CPU feature detection is available only when `std` is enabled." Do not
`default-features = false` it.

Do **not** enable `matrixmultiply`'s `threading` feature. It spawns a pool sized to the physical
CPU count and is meaningless at these sizes; for an IME it is a per-keystroke thread-pool wakeup
against a ~30 ms budget.

### 4.8 The 80 ms budget

Python NumPy beam search, beam=4, best-of-5 on this machine:

| roman | ms | top-1 |
|---|---|---|
| `ki` | 18.4 | কি |
| `amar` | 23.3 | আমার |
| `onek` | 24.5 | অনেক |
| `tomake` | 31.7 | তোমাকে |
| `khacche` | 33.2 | খাচ্ছে |
| `valobasha` | 42.1 | ভালোবাশা |
| `bangladesh` | 44.1 | বাংলাদেশ |
| `protisruti` | 52.1 | প্রতিশ্রুতি |

A rough Rust projection for `protisruti` (11 chars → `max_len = min(60, 3*11+5) = 38`, but EOS
finalisation typically stops around 11–13 steps): 12 steps × 6 layers × (qkv 0.0296 + out 0.0090 +
cross-q 0.0090 + cross-out 0.0090 + fc1 0.0436 + fc2 0.0459) ms + 12 × logits 0.0339 ms
≈ **10.3 ms** of matmul on x86_64, ≈ **13.3 ms** on i686. Add attention (small: T≤13, head_dim=64),
softmax, layer norms, and the `score_candidates` pass (~0.63 ms × 3 shapes × 6 layers ≈ 11 ms).
The ~80 ms target is comfortable; the risk is not the gemm, it is allocation churn and cache-slicing
in the beam reorder (`xlit_np.py:507-510`).

**Budget this explicitly:** at 48–72 decode matmuls per word, using `matrixmultiply` instead of the
hand kernel would add roughly 0.25 ms × 6 layers × 12 steps ≈ **18 ms on i686** for the fused-qkv
call alone. That is the difference between meeting and missing the budget.

---

## 5. Q3 — mmap + binary search over a sorted string→record table

### 5.1 What the Python actually does — and it is not just exact lookup

`core.py` loads six `marisa_trie.RecordTrie` files (`core.py:184-200`):

| File | Record format | Entries | Bytes on disk |
|---|---|---|---|
| `unigrams.marisa` | `<III` → (wiki, subs, chat) | 258,392 | 1,355,816 |
| `romans.marisa` | `<Ib` → (count, source_bits) | 1,280,777 | 12,041,904 |
| `keys.marisa` | `<I` → (score,) | 516,606 | 3,542,768 |
| `prefixes.marisa` | `<I` → (score,) | 124,267 | 807,264 |
| `bigrams.marisa` | `<I` → (count,) | 618,619 | 3,460,360 |
| `bigram_totals.marisa` | `<I` → (total,) | 221,100 | 1,054,640 |
| **total** | | | **22,262,752** |

The access patterns are **not** all point lookups:

| Call site | Operation |
|---|---|
| `core.py:259`, `:266`, `:285`, `:283` | `.get(key)` — exact lookup |
| `core.py:315` | `word in self.uni` — membership |
| `core.py:322` | `self.romans.items(r + "\t")` — **prefix range** |
| `core.py:330` | `self.prefixes.items(f"r:{r}\t")` — **prefix range** |
| `core.py:334` | `self.romans.items(r)` — **prefix range** |
| `core.py:366`, `:371` | `self.keys.items(prefix)` / `items(prefix + "\t")` — **prefix range** |
| `core.py:368` | `self.prefixes.items(f"k:{level[0]}:{k}\t")` — **prefix range** |
| `core.py:203` | `self.uni.items()` — full scan, once at init |

A sorted table with binary search **does** serve all of these: a prefix range is
`lower_bound(prefix)` to `lower_bound(prefix_successor)`, which is exactly what `items(prefix)`
computes. So the data structure question is not about capability. It is about size.

### 5.2 The size problem with a flat table

Measured by walking the real tries:

| | marisa | flat sorted table |
|---|---|---|
| `romans` keys (UTF-8) | — | 46,236,344 bytes (avg 36.1 B/key) |
| `romans` + 4 B offset index + 5 B record | — | **57,763,337 bytes** |
| `romans.marisa` | **12,041,904 bytes** | — |
| `keys` keys (UTF-8) | — | 16,097,502 bytes |
| `keys` + 8 B per entry | — | **20,230,350 bytes** |
| `keys.marisa` | **3,542,768 bytes** | — |

A flat sorted table is **4.8× larger** for `romans` and **5.7×** for `keys`. The reason is obvious
once you look at a key: the `romans` keys are `"<roman>\t<bangla word>"` with enormous shared
prefixes —

```
amantranpatra\tআমন্ত্রণপত্র
amantranpatrati\tআমন্ত্রণপত্রটি
amantranpatratio\tআমন্ত্রণপত্রটিও
amantranpatrai\tআমন্ত্রণপত্রই
```

A trie/FST stores that shared structure once. A flat table stores it 1,280,777 times. Going from
22 MB of lexicon to ~90 MB would be a direct violation of the lightweight-first constraint.

### 5.3 Recommendation: `fst` 0.4.7

`fst` (BurntSushi) is a memory-mappable finite-state transducer with exactly the operations needed:

- `Map::get(&[u8]) -> Option<u64>` — exact lookup.
- `Map::range().ge(prefix).lt(successor)` and `Map::search(automaton)` — prefix ranges, streamed in
  **lexicographic byte order**.
- Built from a **sorted** iterator; the build is streaming, so the build step needs no giant
  in-memory map.
- `Map::new(mmap)` over a `memmap2::Mmap` — zero-copy, no deserialisation step at all. Startup cost
  for the whole lexicon becomes the mmap call, not the 59.3 ms Python spends loading the six
  `.marisa` files (measured: 11.15 + 14.00 + 9.70 + 7.94 + 8.83 + 7.69 ms).

Built for i686 in this session (version 0.4.7). It is from 2021 and has had no releases since, but
it is the index format under ripgrep and Tantivy and is stable rather than abandoned.

**The one real limitation: `fst::Map` values are a single `u64` per key.** Map the records:

| Trie | Record | Packing into u64 |
|---|---|---|
| `romans` | `(u32 count, i8 src)` | `(count as u64) << 8 \| (src as u8 as u64)`. `src` is a bitmask of 1/2/4 (`build_lexicon.py:34`), fits in 3 bits. |
| `keys`, `prefixes`, `bigrams`, `bigram_totals` | `(u32,)` | direct. |
| `unigrams` | `(u32 wiki, u32 subs, u32 chat)` | **Does not fit.** Store an ordinal in the FST and put three `u32` arrays in a side blob, indexed by that ordinal. Measured maxima: wiki ≤ 155,204, subs ≤ 62,511, chat ≤ 7,865 — but do not bit-pack on those, they are properties of this build, not of the format. |

`fst` will not preserve the `.marisa` files; the build step must regenerate the lexicon into `.fst`.
That is a build-pipeline change, not a runtime one — see §5.5 for the ordering consequence.

### 5.4 `rsmarisa` — the alternative, and why I would not start there

`rsmarisa` 0.4.2 (2026-06-01, BSD-2-Clause) is a pure-Rust port of marisa-trie that claims binary
compatibility with the C++ format. If it works, it keeps the existing `.marisa` artefacts *and*
the existing iteration order, which removes the tie-break risk in §5.5 entirely.

Against it: **4,868 total downloads, 7 versions ever published.** That is a crate with essentially
no production exposure. I did **not** build or test it. For a component that decides which word the
user sees, "it says it is binary compatible" is not evidence.

**Recommendation:** default to `fst`. Spike `rsmarisa` for half a day in parallel; if it reads the
existing `romans.marisa` and reproduces `marisa_trie`'s `items()` output byte-for-byte on a sample
of 10,000 prefixes, it becomes the lower-risk option *because it preserves ordering*. Decide on the
evidence, not on the download count.

### 5.5 ⚠️ The ordering hazard — read this twice

**`marisa_trie`'s `items(prefix)` does not return keys in lexicographic order.** Verified:

```
first 8 items for prefix 'ama':
['amantranpatra\tআমন্ত্রণপত্র', 'amantranpatra\tআমন্ত্রনপত্র',
 'amantranpatra\tঅমন্ত্রণপত্র', 'amantranpatrati\tআমন্ত্রণপত্রটি',
 'amantranpatratio\tআমন্ত্রণপত্রটিও', 'amantranpatrai\tআমন্ত্রণপত্রই',
 'amantranpatrao\tআমন্ত্রণপত্রও', 'amantranpatre\tআমন্ত্রণপত্রে']
sorted? False
```

(`amantranpatratio` before `amantranpatrai` — that is LOUDS node order, not byte order.)

`fst` returns lexicographic order. The two orders differ. Where does that matter?

- **It does not matter** for `completions` (`core.py:346`, `completions.sort(reverse=True)` on the
  4-tuple `(score, word, count, gap)`) or for `exact`/`longer` (`core.py:374-375`, `sort(reverse=True)`
  on `(score, word)`). Both sort keys include `word`, so the result is a total order independent of
  input order. Good.
- **It absolutely matters** for the final ranking. `core.py:525`:

  ```python
  ranked = sorted(feats.items(), key=lambda kv: -self.score(kv[0], kv[1], roman, context))
  ```

  `feats` is a plain `dict` (`core.py:310`), so it iterates in **insertion order**, and insertion
  order is trie iteration order (`f(word)` is first called at `core.py:324`, inside the
  `self.romans.items(r + "\t")` loop). Python's `sorted` is **stable**. Therefore any two candidates
  with **equal score** are output in marisa's LOUDS order.

  The same applies to `core.py:530-535` (`fast_suggest`/model-free path) and `core.py:571-577`.

So: changing the lexicon backend changes the order of exactly-tied candidates. Whether that is
observable depends on how often two candidates tie to the last bit of an f64 score, which given
`personal_bonus` and `math.log` terms is rare but not impossible — and *is* systematic for
candidates that share every feature.

**Required action for the port:** make the tie-break explicit rather than emergent. Sort by
`(-score, word)` — i.e. add the candidate word as a deterministic secondary key — and change the
Python to match *before* the port, so the two implementations can be diffed. Do not leave the port
inheriting an ordering it cannot reproduce.

### 5.6 `memmap2` vs `std::fs::read` for the lexicon

Use `memmap2`. Rationale is the same as §3.4 plus one more: with `fst` the mapping is not just
storage, it *is* the index — `fst::Map::new(mmap)` is O(1) and lookups fault in only the pages they
touch. A prefix query for `"b"` matched 128,068 entries in the Python (230 ms for six such scans);
with an mmapped FST the untouched majority of the 12 MB never enters the working set.

`std::fs::read` of the whole lexicon would be ~22 MB of private committed memory and ~8 ms of I/O.
That is the fallback for a non-local install path (§3.6), nothing more.

---

## 6. Q4 — SQLite from Rust, preserving the existing DB file

### 6.1 What must be preserved

`personal.py:35-43` creates:

```sql
CREATE TABLE IF NOT EXISTS selections (roman TEXT, word TEXT, count REAL, last REAL, PRIMARY KEY (roman, word))
CREATE TABLE IF NOT EXISTS words (word TEXT PRIMARY KEY, count REAL, last REAL)
PRAGMA journal_mode=WAL
```

The live file on this machine, `%LOCALAPPDATA%\Likhi\personal.sqlite`, 77,824 bytes, with a
**4,136,512-byte WAL** alongside. Header inspected byte by byte:

```
magic                "SQLite format 3\0"
page size            4096
write version        2      (WAL)
read version         2      (WAL)
text encoding        1      (UTF-8)
schema format        4
```

### 6.2 Verified: `rusqlite` opens it

I built a probe for **`i686-pc-windows-msvc`** against a copy of the real database and ran it:

```
rusqlite 0.40.2 / bundled sqlite 3.53.2
TABLE selections: CREATE TABLE selections (roman TEXT, word TEXT, count REAL, last REAL, PRIMARY KEY (roman, word))
TABLE words: CREATE TABLE words (word TEXT PRIMARY KEY, count REAL, last REAL)
selections=368 words=358
  row: "janoargulo" "জানোয়ারগুলো" count=1 last=1789503347.0072174
  row: "emke" "এমকে" count=1 last=1789503372.0425413
  row: "electronics" "ইলেকট্রনিক্স" count=1 last=1789503379.0345774
user_version = 0
```

Schema, WAL, UTF-8 and the `REAL` timestamps with full f64 precision all survive. No migration is
needed.

### 6.3 Binary size cost of `bundled`

Measured, same profile (`opt-level=3, lto=true, codegen-units=1, panic=abort, strip=true`):

| Binary | i686 | x86_64 |
|---|---|---|
| empty `fn main(){println!()}` | 100,352 | 113,664 |
| + memmap2 + bytemuck + half + fst + matrixmultiply + ndarray | 188,928 | 205,824 |
| + `rusqlite` with `bundled` | 1,509,888 | 1,847,296 |
| **`rusqlite` bundled delta** | **1,320,960 B ≈ 1.26 MiB** | **1,641,472 B ≈ 1.57 MiB** |

For reference the other core crates together cost 88,576 B ≈ 87 KiB on i686.

**Is 1.26 MiB acceptable?** Yes, and there is no alternative. `rusqlite` without `bundled` links
against a system `sqlite3.lib`, which Windows does not ship, so you would have to vendor a DLL —
same bytes, plus a second file to sign and ship. A different embedded KV store (`redb`, `sled`)
would not read the existing `personal.sqlite`, and `personal.py:121-137` exposes
`export_jsonl` as a user-facing promise that the store is inspectable. Keep SQLite.

Ways to trim it if it matters later (all **unmeasured**, see §11): `libsqlite3-sys` respects
`SQLITE_OMIT_*` / `SQLITE_MINIMUM_FILE_DESCRIPTOR` style defines via the `LIBSQLITE3_FLAGS`
environment variable; the engine uses no FTS, no JSON1, no R-Tree, no virtual tables. Do not enable
`bundled-full`, `bundled-sqlcipher`, `vtab`, `csvtab`, `session`, `serialize`, `load_extension`, or
`modern_sqlite` — the workload is four statements.

### 6.4 ⚠️ `execute()` on a PRAGMA that returns a row — verified failure

Python does `self.db.execute("PRAGMA journal_mode=WAL")` (`personal.py:36`) and it just works.
The direct Rust translation **fails**. Measured, verbatim:

```
execute(PRAGMA journal_mode=WAL)     -> Err: Execute returned results - did you mean to call query?
query_row(PRAGMA journal_mode=WAL)   -> "wal"
execute_batch(PRAGMA journal_mode=WAL;) -> Ok
```

`PRAGMA journal_mode=<mode>` returns the *resulting* mode as a row. `Connection::execute` refuses
any statement that produces rows. Use `execute_batch`, or `query_row` if you want to assert the
result is `"wal"` — which you should, because a `journal_mode` change silently falls back to
`delete` if the database is on a network share or is currently in a transaction.

### 6.5 Other translation points for `personal.py`

- `count REAL, last REAL` → `f64` on both sides. `time.time()` values are ~1.79e9 and need the full
  53-bit mantissa; `_decayed` (`personal.py:93-94`) multiplies by `exp(-decay * dt)`. Do **not**
  read these as `f32`.
- `HALF_LIFE_DAYS = 90.0` (`personal.py:26`) and `self.decay = math.log(2) / (half_life_days * 86400.0)`
  (`personal.py:44`). Reproduce as `std::f64::consts::LN_2 / (90.0 * 86400.0)` =
  `8.9184874...e-8`. Compute it, do not paste a rounded literal.
- `_decayed` clamps: `max(0.0, now - last)` — a clock that went backwards must not *increase* a
  count. Keep the clamp.
- `PersonalStore.__init__` passes `check_same_thread=False` (`personal.py:35`) and the engine holds
  one connection across threads. In Rust that is a `Mutex<Connection>` or a per-thread connection;
  `rusqlite::Connection` is `Send` but not `Sync`. With WAL, multiple connections are fine and
  avoid a lock on the keystroke path — but `learn()` (`personal.py:51-74`) does two SELECTs and two
  INSERT-OR-REPLACEs plus a `commit()`, and `core.py:247-254` calls it on every committed word.
  That is not on the per-keystroke path, but it is on the per-word path; measure it.
- `learn()` stores `w = chosen if chosen == roman else canonical(chosen)` (`personal.py:55`) — the
  raw-Latin passthrough is stored *uncanonicalised* on purpose. `core.py:421-422` relies on it.
- The 4.1 MB WAL next to a 78 KB database says nothing is checkpointing. Python never calls
  `wal_checkpoint`. The port should, on clean shutdown — `PRAGMA wal_checkpoint(TRUNCATE)` — or the
  file grows without bound on a machine that is never cleanly closed.

---

## 7. Q5 — HTTP POST with TLS, no async runtime

### 7.1 What has to be sent

`telemetry.py:202-226`, `_send_http`: one POST per chunk, body is newline-delimited JSON capped at
`MAX_CHUNK_BYTES = 256 * 1024` (`telemetry.py:42`), timeout `HTTP_TIMEOUT_S = 10.0`
(`telemetry.py:43`), headers:

```
Content-Type:    application/x-ndjson
X-Likhi-Install: <install_id>
X-Likhi-Stream:  metrics | events
X-Likhi-Seq:     <n>
X-Likhi-Version: 1
X-Likhi-Key:     <key>        (only when configured)
```

Success is any 2xx; anything else raises and the sync offset is **not** advanced
(`telemetry.py:290-292`). Runs on a background thread only (`telemetry.py:238`). No streaming, no
redirects expected, no cookies, no keep-alive requirement. This is about as small an HTTP surface
as exists.

### 7.2 Recommendation: `ureq` 3.4.2 with `native-tls`

```toml
ureq = { version = "3", default-features = false, features = ["native-tls"] }
```

Measured i686 binaries, same profile, delta over the 100,352-byte empty baseline:

| Configuration | i686 bytes | delta |
|---|---|---|
| empty baseline | 100,352 | — |
| `ureq` default (`rustls` + `ring` + `gzip` + `webpki-roots`) | 1,193,984 | +1,093,632 (1.04 MiB) |
| `ureq` `default-features=false, features=["rustls"]` | 1,137,152 | +1,036,800 (0.99 MiB) |
| **`ureq` `default-features=false, features=["native-tls"]`** | **565,760** | **+465,408 (455 KiB)** |

`native-tls` on Windows resolves to `schannel` 0.1.29 — the OS TLS stack. No vendored crypto, no
assembly, no extra build tools, and **half the binary**.

Live end-to-end test, i686 binary, real network:

```
TLS OK, server said 405
```

(a real HTTPS POST to `https://example.com/ingest`; 405 is the server rejecting the method, which
means handshake, request and response all completed).

### 7.3 ⚠️ The `native-tls` root-certificate trap — verified

ureq's default TLS config sets root certs to its bundled WebPKI roots regardless of provider. Point
it at `native-tls` without changing that and **every request fails**:

```
err native-tls: unable to find any user-specified roots in the final cert chain
```

The fix, verified working:

```rust
let cfg = ureq::Agent::config_builder()
    .tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)   // <-- required
            .build())
    .timeout_global(Some(std::time::Duration::from_secs(10)))     // HTTP_TIMEOUT_S
    .user_agent("likhi/1.0")
    .build();
let agent: ureq::Agent = cfg.into();
```

`RootCerts::PlatformVerifier` is not a nicety. It is what makes the telemetry upload work behind a
corporate TLS-inspecting proxy, where the interception CA is in the Windows certificate store and
is **not** in Mozilla's bundle. A pilot deployment inside a company is precisely where a bundled
root store fails and an OS root store succeeds. This is a second, independent reason to prefer
`native-tls` over `rustls` here.

`ConfigBuilder` timeout methods, confirmed against the 3.4.2 docs: `timeout_global`,
`timeout_per_call`, `timeout_resolve`, `timeout_connect`, `timeout_send_request`,
`timeout_await_100`, `timeout_send_body`, `timeout_recv_response`, `timeout_recv_body` — all
`Option<Duration>`, all default `None` except `timeout_await_100` (1 s). Use `timeout_global` to
mirror `HTTP_TIMEOUT_S`.

Status handling: ureq 3.x returns `Err(ureq::Error::StatusCode(u16))` for non-2xx by default rather
than an `Ok` response, which matches `telemetry.py:225-226`'s "raise on non-2xx" exactly. Map both
that and transport errors to "do not advance the offset".

### 7.4 Why not `rustls`

- Twice the binary (+1.04 MiB vs +455 KiB).
- Bundled Mozilla roots fail behind enterprise TLS interception unless you add `platform-verifier`,
  which adds *more* dependencies on top.
- It pulls `ring` 0.17.14, whose licence is a non-standard composite (a BoringSSL-derived
  ISC/MIT/OpenSSL mix) that has to be enumerated in `THIRD-PARTY.md`. `schannel` is plain MIT.
- It is not a build hazard (see §8.1) — but it buys nothing on a Windows-only target.

`rustls` would be the right answer if Likhi ever targets Linux or macOS. It does not.

### 7.5 Why not `attohttpc`

`attohttpc` 0.31.0 is smaller in source (4,053 lines) and would probably produce a comparable
binary. Two reasons not to:

1. **Licence: `MPL-2.0`.** Likhi is MIT (`LICENSE`, `THIRD-PARTY.md`). MPL-2.0 is file-level weak
   copyleft — linking into an MIT binary is permitted, but any modification to attohttpc's own
   source files must be published under MPL-2.0, and the obligation has to be documented. Every
   other crate in this spec is MIT / Apache-2.0 / Zlib / BSD / Unlicense. Introducing the project's
   only copyleft dependency for a component that posts one JSON chunk an hour is a poor trade.
2. Less exposure: 34.0M downloads against ureq's 203.6M.

If it is adopted anyway, note that its default TLS feature is `native-tls`, which is the right
default — but add the MPL-2.0 entry to `THIRD-PARTY.md` in the same commit.

### 7.6 No async runtime, confirmed

`ureq` 3.x is synchronous and blocking by design; nothing in the dependency tree of the
`native-tls` configuration is `tokio` or `async-std`. The resolved tree was `ureq 3.4.2`,
`ureq-proto 0.6.4`, `http 1.5.0`, `native-tls 0.2.18`, `schannel 0.1.29` — that is the whole of it.
Call it from the existing background thread (`telemetry.py:238`: "Runs on a background thread only.
Never call it from a keystroke path.") and the rule carries over unchanged.

---

## 8. Cross-compilation hazards for `i686-pc-windows-msvc`

Asked for explicitly, so here it is consolidated. **Tested on this machine, with no `nasm`, no
`cmake`, no `perl` and no `clang` installed.**

### 8.1 Not hazards — verified to build and run for i686

| Crate | Evidence |
|---|---|
| `memmap2` 0.9.11 | built + ran (mmapped 46 MB, 0.097 ms) |
| `bytemuck` 1.25.2 | built + ran |
| `half` 2.7.1 | built + ran (11.5M f16→f32 in 21.4 ms) |
| `fst` 0.4.7 | built |
| `matrixmultiply` 0.3.11 | built + ran (but see §8.4 — a *performance* hazard) |
| `ndarray` 0.17.2 | built + ran |
| `zip` 8.6.0 + `flate2` 1.1.10 + `miniz_oxide` | built + ran (inflated the real npz) |
| `rusqlite` 0.40.2 `bundled` + `libsqlite3-sys` 0.38.2 | built + opened the real `personal.sqlite` |
| `ureq` 3.4.2 + `native-tls` 0.2.18 + `schannel` 0.1.29 | built + live HTTPS POST returned 405 |
| `ureq` 3.4.2 + `rustls` 0.23.45 + **`ring` 0.17.14** | built |
| `core::arch` AVX/FMA intrinsics under `target_arch = "x86"` | built + ran + numerically verified |
| `is_x86_feature_detected!` on `target_arch = "x86"` | ran, printed `avx=true fma=true avx2=true sse2=true` |

**`ring` deserves a specific note because the internet will tell you otherwise.** ring's
`BUILDING.md` says: *"For any target for which ring has assembly language implementations of
primitives (32- and 64-bit Intel, and 32- and 64-bit ARM), Perl must be installed"* and *"For
Windows x86 and x86-64 targets only, `target/tools/windows/nasm/nasm[.exe]` is used as the
assembler."* Those requirements apply to building ring **from its git repository**. The crates.io
package ships **pre-assembled object files**. Verified in
`~/.cargo/registry/src/.../ring-0.17.14/pregenerated/`:

```
aesni-x86-win32n.o     12,061
chacha-x86-win32n.o     8,912
ghash-x86-win32n.o      4,700
vpaes-x86-win32n.o      7,654
x86-mont-win32n.o       3,713
```

17 `.o` files in total, including a full 32-bit Windows set. This is why `ureq` + `rustls` + `ring`
linked for i686 on a machine with neither perl nor nasm. So ring is **not** a build hazard — it is
still rejected, on size and root-store grounds (§7.3, §7.4).

### 8.2 Real hazard: `aws-lc-rs`

`aws-lc-rs` 1.18.1 is `rustls`'s other crypto provider and the default in some configurations. It
builds BoringSSL-derived C and assembly through **CMake**, and on Windows x86 needs **NASM**.
Neither is installed here and neither is implied by "MSVC toolchain". 32-bit Windows is not among
its well-trodden targets.

**Action:** if anything in the tree pulls `rustls`, pin the provider explicitly and add a
`Cargo.toml` comment. Never let `aws-lc-rs` be selected by feature unification.

### 8.3 Real hazard: any crate that shells out to `cmake`

`cmake` is not installed. `libsqlite3-sys` uses the `cc` crate (compiles `sqlite3.c` with MSVC,
found automatically via `vswhere`) and worked — `cc` is fine. `cmake`-driven builds are not.
Audit `cargo tree` for `cmake` as a build-dependency before committing to any new crate.

### 8.4 Real hazard: **i686 register pressure**, not portability

This is the one that will actually bite, and it is a *performance* hazard that compiles cleanly and
silently costs you the latency budget:

```
dec fused qkv 4×256×768, matrixmultiply:   x86_64 0.0594 ms    i686 0.2862 ms   (4.8× worse)
dec fused qkv 4×256×768, hand m=4 kernel:  x86_64 0.0296 ms    i686 0.0348 ms   (1.2× worse)
```

32-bit x86 exposes 8 YMM registers; 64-bit exposes 16. Any kernel written for 16 spills on i686.
**Design every hot kernel to fit in 8 vector registers** and the 32-bit build stops being a second
class citizen. See §4.6.

### 8.5 Non-hazard worth knowing: `half` needs a feature for `bytemuck`

`bytemuck::cast_slice::<u8, half::f16>` fails to compile with a trait-bound error until you write
`half = { version = "2", features = ["bytemuck"] }`. It cost a compile cycle here; it will cost one
there. (`half` also has a `zerocopy` feature if you prefer that crate.)

---

## 9. Constants and formulas the port must reproduce exactly

Reproduced verbatim, not paraphrased. Every one of these changes which word the user sees.

### 9.1 Model geometry — `dist/runtime/models/indicxlit-np/config.json`

```json
{"encoder_layers": 6, "decoder_layers": 6, "heads": 4, "dim": 256,
 "pre_norm": true, "activation": "gelu", "layernorm_embedding": true,
 "scale_embedding": true, "learned_pos": false, "padding_idx": 1}
```

Derived: `head_dim = 256 / 4 = 64`; `embed_scale = sqrt(256) = 16.0` (`xlit_np.py:138`);
`scaling = (dim // heads) ** -0.5 = 64 ** -0.5 = 0.125` (`xlit_np.py:171`);
`pos_offset = padding_idx + 1 = 2` (`xlit_np.py:159`). Source vocab 54, target vocab 806.

### 9.2 Formulas

GELU — the **tanh approximation**, not the exact erf (`xlit_np.py:44-52`). fairseq's `gelu` is the
exact form; this is a deliberate inference-time approximation measured as having no effect:

```python
_GELU_C = math.sqrt(2.0 / math.pi)
return 0.5 * x * (1.0 + np.tanh(_GELU_C * (x + 0.044715 * x * x * x)))
```

Layer norm, `eps = 1e-5`, **biased variance** (divide by N, not N−1) (`xlit_np.py:59-62`):

```python
mu = x.mean(-1, keepdims=True)
var = ((x - mu) ** 2).mean(-1, keepdims=True)
return (x - mu) / np.sqrt(var + eps) * g + b
```

Log-softmax (`xlit_np.py:65-68`):

```python
m = x.max(-1, keepdims=True); z = x - m
return z - np.log(np.exp(z).sum(-1, keepdims=True))
```

Sinusoidal positions — fairseq's layout is **`[sin(all halves) || cos(all halves)]`**, not
interleaved, and the `padding_idx` row is zeroed *after* construction (`xlit_np.py:71-82`):

```python
half = dim // 2
scale = math.log(10000) / (half - 1)            # note: half - 1, i.e. 127
freqs = np.exp(np.arange(half, dtype=np.float32) * -scale)
n = num_positions + padding_idx + 1
pos = np.arange(n, dtype=np.float32)[:, None] * freqs[None, :]
emb = np.concatenate([np.sin(pos), np.cos(pos)], axis=1)
emb[padding_idx] = 0
```

With `dim = 256`: `half = 128`, `scale = ln(10000)/127 = 0.07252...`. `max_positions = 256` default
(`xlit_np.py:128`) so the table has `256 + 1 + 1 = 258` rows. **`half - 1` is not a typo — do not
"fix" it to `half`.**

Attention causal mask value is `-1e30`, not `-inf` (`xlit_np.py:264-265`):

```python
mask = np.triu(np.ones((tq, tk), dtype=bool), k=tk - tq + 1)
s = np.where(mask, -1e30, s)
```

### 9.3 Weight preparation at load — `xlit_np.py:171-196`

The q-scaling is folded into the fused projection, **including the bias**:

```python
scaling = (self.dim // self.heads) ** -0.5          # 0.125
wq = f[f"{p}.q_proj.weight"].T.copy()
wqkv = np.ascontiguousarray(np.concatenate([wq * scaling, wk, wv], axis=1))
bqkv = np.concatenate([bq * scaling, bk, bv])
```

So `wqkv` is `(256, 768)` with columns `[q*0.125 | k | v]` and `bqkv` is `(768,)` with
`[bq*0.125 | bk | bv]`. **The cross-attention q is *not* pre-scaled** — it is scaled at use
(`xlit_np.py:329`, `:367`): `q = self._split((h @ c.wq + c.bq) * self.scaling)`. Two different code
paths for the same constant; reproduce both.

`out_proj`, `fc1`, `fc2` are all stored transposed at load (`.T.copy()`, `xlit_np.py:190`, `:200`,
`:202`). The build step should emit them already transposed and contiguous.

### 9.4 Banned target tokens — `xlit_np.py:227-234`

```python
banned = np.zeros(len(self.tgt_vocab), dtype=np.float32)
banned[self.pad] = -np.inf
banned[self.tgt_index["<s>"]] = -np.inf
for t, i in self.tgt_index.items():
    if (t.startswith("__") and t.endswith("__")) or t.startswith("madeupword"):
        banned[i] = -np.inf
```

Added to the log-probs each step (`:468`). Note `</s>` and `<unk>` are **not** banned; `<unk>` is
dropped when the word is joined (`:519`: `if i != self.unk`).

### 9.5 Beam search — `xlit_np.py:439-525`

- `max_len = max_len or min(60, 3 * len(roman) + 5)` (`:454`)
- `min_len = 1`, enforced as `lp[:, self.eos] = -np.inf` at `step == 0` (`:469`)
- decoder starts from `</s>`, not `<s>` (`:461`)
- candidate pool `k = min(2 * beam, flat.size)` (`:473`)
- **EOS is only finalised when it ranks within the top `beam`** (`:486-491`) — the comment there
  explains why: lower-ranked EOS entries would fill the finished list and end the search before
  longer correct words complete
- score normalisation `sc / ((step + 1) ** lenpen)`, `lenpen = 1.0` (`:491`)
- stop when `len(finished) >= beam or not new_tokens` (`:498`)
- **no other early stop** (`:500-502`): "a live hypothesis' length-normalized score can still
  improve as it grows… it dropped the model's own best answer, খাচ্ছে for `khacche`, at beam 4."
  Do not add a pruning bound.
- if nothing finished, fall back to live hypotheses scored `float(s) / (max_len ** lenpen)` (`:512-513`)
- dedupe by surface string, first occurrence wins (`:517-522`)

### 9.6 Engine constants — `core.py`

`DEFAULT_WEIGHTS` (`core.py:54-74`) — the runtime file `models/lexicon/weights.json` overrides
these (`core.py:212-214`), and `LIKHI_WEIGHTS` overrides that. Reproduce the defaults anyway:

```
unigram 1.0        rom_exact 2.5       rom_exact_log 0.8    rom_exact_fast 2.5
rom_prefix 0.3     key_fine 1.2        key_coarse 0.6       key_prefix -1.0
gap -0.4           xlit_logp 0.5       xlit_top1 1.5        xlit_top3 0.7
avro 0.8           oov -3.0            personal_sel 5.0     personal_word 0.6
bigram 0.7         latin_base -6.0     latin 0.0
```

Other constants:

| Constant | Value | Site |
|---|---|---|
| `UNKNOWN_CONFIDENT_LOGP` | `-11.0` | `core.py:77` |
| `FAST_TRUST_ROM_EXACT` | `5` | `core.py:83` |
| `max_per_channel` | `30` | `core.py:175` |
| `model_scored` | `16` | `core.py:176` |
| `beam` | `4` | `core.py:175` |
| cache size | `4096` | `core.py:243` |
| `_uni_total` | **`25776948.0`** | computed, `core.py:203-205` |
| `_floor` | **`-17.75813834268104`** | computed, `core.py:206` |
| unigram mixture | `wiki + 3*subs + 20*chat` | `core.py:204`, `:263`, `:270`, `build_lexicon.py:270` |
| bigram backoff | `math.log(0.4)` | `core.py:288` |
| xlit_logp clamp | `max(-40.0, ft.xlit_logp)` | `core.py:493` |
| unscored xlit penalty | `-15.0` | `core.py:496` |
| confidence ramp | `min(1.0, max(0.0, -xl / 3.0))` | `core.py:477`, `:510` |

`_uni_total` and `_floor` were computed by summing `a + 3b + 20c` over all 258,392 unigram records:
**25,776,947**, then `float(total) + 1.0 = 25776948.0`, then
`log(0.5 / 25776948.0) = -17.75813834268104`. That sum fits in `u32` today (max single record
342,722; maxima wiki 155,204 / subs 62,511 / chat 7,865) but **accumulate in `u64`** — a larger
Wikipedia dump would overflow `u32` and the failure mode is a silently wrong prior for every word.

`personal_bonus` (`core.py:154-161`):

```python
return w["personal_sel"] * math.log1p(count) * (0.5 + share)
```

`_static_prescore` (`core.py:448-459`) — note the hardcoded weights here are **not** the tunable
ones and must stay hardcoded:

```python
s = self.unigram_logp(word)
s += 3.0 * bool(ft.rom_exact) + 0.5 * math.log1p(ft.rom_exact)
s += 1.5 * ft.key_fine + 0.8 * ft.key_coarse
s += 2.5 * bool(ft.xlit_rank) + 1.0 * ft.avro
s -= 1.0 * (ft.key_fine_prefix or ft.key_coarse_prefix or bool(ft.rom_prefix)) * (not ft.rom_exact)
```

Beam-score un-normalisation (`core.py:400-402`) — subtle and easy to lose:

```python
ft.xlit_logp = float(lp) * (len(word) + 1)
```

The beam returns `sum / (chars + EOS)`; this multiplies back by `len(word) + 1` so it matches the
raw sums `score_candidates` produces. `len(word)` is **Python character count**, i.e. Unicode
scalar values — in Rust that is `word.chars().count()`, **not** `word.len()`.

### 9.7 Lexicon build constants — `build_lexicon.py`

```
SRC_DAKSHINA, SRC_AKSHARANTAR, SRC_BANGLATLIT = 1, 2, 4     (:34)
SHORT_ROMAN_PREFIX = 3                                      (:169)
SHORT_KEY_PREFIX = 2                                        (:170)
PREFIX_TOP = 24                                             (:171)
prefix score  = count * 10_000 + min(lex.get(word, 0), 9_999)    (:195)
runtime score = count * 10_000 + min(self._lex_score(word), 9_999)  (core.py:339)
record clamps = min(c, 2**31 - 1)                           (:263, :274, :208)
key score     = wiki + 3*subs + 20*chat                     (:270)
```

Key namespaces inside the tries: `"<roman>\t<word>"` in `romans`; `"f:<key>\t<word>"` /
`"c:<key>\t<word>"` in `keys` (first letter of "fine"/"coarse", `:274`); `"r:<prefix>\t<word>"` and
`"k:<f|c>:<keyprefix>\t<word>"` in `prefixes` (`:192`, `:202`); `"<prev>\t<word>"` in `bigrams`
(`core.py:285`). **The separator is a literal TAB (U+0009) everywhere.** Splitting is
`split("\t", 1)` — maxsplit 1, so a tab inside a word would not break it.

The `len(r) <= 3` / `len(k) <= 2` switches at `core.py:328` and `core.py:365` must match
`SHORT_ROMAN_PREFIX`/`SHORT_KEY_PREFIX`. They are currently hardcoded in two files; unify them in
the port.

### 9.8 Telemetry constants — `telemetry.py`

```
MAX_LEN = 32                                    (:36)
_RE_UNSAFE = re.compile(r"[\d@:/\\]")           (:37)
FLUSH_EVERY = 20                                (:41)
MAX_CHUNK_BYTES = 256 * 1024                    (:42)
HTTP_TIMEOUT_S = 10.0                           (:43)
hour bucket = time.strftime("%Y-%m-%dT%H", time.localtime(...))   (:49)
app counter key = "app_" + re.sub(r'[^a-zA-Z0-9._-]', '', app)[:24]   (:125)
counter name sanitiser = re.sub(r"[^a-z0-9_]", "", counter)[:32]      (:157)
latency cap = 20000 samples                     (:126)
p50 = s[len(s) // 2]                            (:173)
p95 = s[min(len(s) - 1, int(len(s) * 0.95))]    (:174)
install_id = uuid.uuid4().hex[:16]              (:92)
position bucket = f"pos_{min(index, 9)}"        (:119)
```

`_RE_UNSAFE` matches `\d` — in Python 3 with a `str` pattern that is **Unicode** digits, so
Bengali `০-৯` (U+09E6–U+09EF) match too. Rust's `regex` `\d` is also Unicode by default, so
`[\d@:/\\]` ports across directly — but if anyone reaches for `char::is_ascii_digit`, the
redaction silently weakens. The chunk cut is `chunk.rfind(b"\n")` and a chunk with no newline is
skipped entirely (`telemetry.py:277-280`).

---

## 10. Recommended `Cargo.toml`

```toml
[package]
name = "likhi-engine"
version = "0.1.0"
edition = "2021"
license = "MIT"

[dependencies]
memmap2       = "0.9.11"
bytemuck      = "1.25"
half          = { version = "2.7", features = ["bytemuck"] }   # feature required, see §8.5
fst           = "0.4.7"
matrixmultiply = "0.3.11"                                       # scoring pass ONLY, see §4.3
rusqlite      = { version = "0.40", features = ["bundled"] }
ureq          = { version = "3.4", default-features = false, features = ["native-tls"] }
serde_json    = "1"

# NOT dependencies, deliberately:
#   ndarray       - delegates to matrixmultiply, no benefit here (§4.4)
#   wide          - compile-time SIMD width, cannot runtime-dispatch AVX (§4.5)
#   rustls / ring - 2x binary, bundled roots fail behind corporate TLS proxies (§7.4)
#   aws-lc-rs     - needs cmake + nasm; i686-pc-windows-msvc build hazard (§8.2)
#   attohttpc     - MPL-2.0, the only copyleft in the tree (§7.5)
#   zip / flate2  - only if the .npz is read at runtime; the build-time blob removes them (§3.7)

[profile.release]
opt-level     = 3
lto           = true
codegen-units = 1
panic         = "abort"
strip         = true
```

`panic = "abort"` is what the size measurements used. If the engine runs in-process with the TSF
DLL, reconsider: aborting the host application on a panic in an IME is worse than unwinding.
If the engine is a separate process (as `dist/likhi/likhi_ime.py` and `server.py` imply), keep
`abort`.

Expected binary, extrapolating the measured deltas:
100 KiB base + 87 KiB core crates + 1.26 MiB rusqlite + 455 KiB ureq ≈ **1.9 MiB on i686**, plus
23 MB (f16) or 46 MB (f32) of weights and ~22 MB of lexicon.

---

## 11. Uncertain

Flagged deliberately. Each of these is a half-day of measurement, and each would change a number
above.

1. **Cold-cache startup.** Every load timing in §3.3 is warm page cache. C: here is an NVMe SSD; a
   pilot machine may have a SATA SSD or (like the second disk in this box) a 5400 rpm HDD. mmap of
   a 46 MB f32 blob moves 2× the bytes of the 21 MB npz, so on slow storage the npz could win on
   *first* run after boot even while losing badly on every subsequent run. **Measure on the slowest
   target machine before finalising the f32-vs-f16 blob choice.** I could not clear the Windows
   standby list without admin rights in this session.
2. **Compressed size of the f32 blob.** I claimed in §3.4 that an f32 blob compresses to roughly the
   f16 size in an installer because the widened mantissa bytes are zero. I did not test it. If it
   is true, ship f32 and the installer cost is nil; if it is false, the first-run-cache scheme is
   required. One `7z a` away from being known.
3. **Working-set measurement of mmap vs heap.** The file-backed/clean vs private/committed argument
   in §3.4 is mechanism, not measurement. `psutil` is not installed in the venv and I did not wire
   up `GetProcessMemoryInfo` in the Rust probe. The mechanism is not in doubt; the magnitude of the
   benefit under real memory pressure is.
4. **`rsmarisa` correctness.** Not built, not tested, 4,868 downloads. §5.4 recommends a
   time-boxed spike precisely because I cannot vouch for it.
5. **`fst` build time and file size for this lexicon.** I verified `fst` compiles and its API fits
   the access patterns. I did **not** build an FST from the 1,280,777 `romans` entries, so I cannot
   state the resulting file size. It should land between marisa's 12.0 MB and the flat 57.8 MB,
   probably nearer marisa — but "probably" is exactly the word §5 is meant to avoid. **Build one
   before committing to the lexicon format.**
6. **`LIBSQLITE3_FLAGS` size savings.** §6.3's suggestion to trim SQLite with `SQLITE_OMIT_*` is
   untested. The 1.26 MiB figure is for the default `bundled` build.
7. **Whether the tie-break ordering in §5.5 is observable in practice.** I established that the
   mechanism exists (marisa order is not lexicographic; Python's sort is stable; `feats` is
   insertion-ordered). I did **not** measure how many candidate pairs actually tie to the last bit
   on a real evaluation set. The fix — an explicit `(-score, word)` sort key — is cheap enough that
   it should be applied regardless, but the *size* of the risk is unknown.
8. **Beam width at runtime.** §4.6 assumes m ∈ 1..=4. `core.py:216` reads `LIKHI_BEAM` from the
   environment, so a pilot config could set beam=8 and silently fall off the specialised kernel.
   Either generalise the kernel to m ∈ 1..=8 or clamp and log.
9. **Attention, softmax and layer-norm cost.** §4.8's budget accounts only for the gemms. At
   T ≤ 13 and head_dim 64 the attention matmuls are tiny, but `log_softmax` over 806 logits ×
   4 beams × ~12 steps, and 4 layer norms × 6 layers × 12 steps, are not free and were not measured.
10. **Numerical reproducibility against the Python.** The hand kernel matches the naive loop to
    4.9e-7 relative (§4.3), but neither was compared against NumPy's actual output on real weights.
    NumPy's `@` uses a different summation order, and `log_softmax` / `layer_norm` reductions differ
    again. Before trusting the port, run both engines over the Dakshina dev set and diff the
    *ranked candidate lists*, not the scores. A reordering at position 1 is a bug; a 1e-6 score
    difference is not.

---

## Appendix A — how to reproduce every measurement

Probe crates, benchmark sources and the exported blobs are in this session's scratchpad:

```
…\scratchpad\rustprobe\          core crates, i686 + x64 baseline sizes
…\scratchpad\probe_sqlite\       rusqlite bundled; opens a copy of the real personal.sqlite
…\scratchpad\probe_ureq\         ureq default (rustls + ring)
…\scratchpad\probe_ureq_min\     ureq default-features=false, features=["rustls"]
…\scratchpad\probe_ureq_nt\      ureq native-tls + PlatformVerifier; live HTTPS POST
…\scratchpad\probe_bench\        npz vs mmap load; naive vs matrixmultiply vs ndarray
…\scratchpad\probe_simd\         naive vs matrixmultiply vs AVX+FMA vs hand m=4 kernel
…\scratchpad\probe_empty\        empty-binary size baseline
…\scratchpad\make_blob.py        exports weights_f32.bin / weights_f16.bin from model.npz
…\scratchpad\uni_total.py        computes _uni_total and _floor from unigrams.marisa
```

**The scratchpad is session-scoped and will not survive.** Nothing in this document depends on it:
the m=4 kernel is reproduced in full in §4.6 and the blob exporter in Appendix B. Re-deriving the
probe crates from §2's version table takes about twenty minutes; the two artefacts the port
actually needs are already here.

---

## Appendix B — the build-time blob exporter

This is the script that produced `weights_f32.bin` (45,950,976 B) and `weights_f16.bin`
(22,975,488 B) from `model.npz`, and hence every load timing in §3.3. It is deliberately minimal —
it does **not** yet write the header from §3.5, and it does **not** pre-transpose the weights per
§9.3. Both are required before it becomes the real build step; they are left out here so that what
is shown is exactly what was measured.

```python
import numpy as np, json, os

p = r"dist\runtime\models\indicxlit-np\model.npz"
w = np.load(p)
names = list(w.files)          # 263 tensors, insertion order preserved

def pad64(b):
    while len(b) % 64:
        b.append(0)

for dtype, fname in ((np.float32, "weights_f32.bin"), (np.float16, "weights_f16.bin")):
    blob, index = bytearray(), []
    for k in names:
        a = np.ascontiguousarray(w[k].astype(dtype))
        pad64(blob)                                   # 64-byte align every tensor
        index.append({"name": k, "off": len(blob), "shape": list(a.shape)})
        blob += a.tobytes()
    open(fname, "wb").write(bytes(blob))
    json.dump(index, open(fname.replace(".bin", "_index.json"), "w"))
    print(fname, len(blob), "bytes")
```

Three things to add before shipping it, each of which is specified elsewhere in this document and
none of which is optional:

1. The `LIKHIW01` header and CRC-32 from §3.5, so the Rust side can validate before casting.
2. The transposes from §9.3 — `out_proj`, `fc1`, `fc2` transposed, and the fused `wqkv` built as
   `concat([wq * 0.125, wk, wv], axis=1)` with the matching `bqkv`. Doing this at build time is
   what lets the Rust loader be a pointer cast instead of 263 transposes.
3. An assertion that every emitted offset is a multiple of 4 (`bytemuck::cast_slice` panics
   otherwise) and that the total matches the sum of the tensor sizes plus padding.
