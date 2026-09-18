# Porting specification — `xlit_np.py` beam search and numerics

Target of the port: `C:\Users\khaled\src\likhi\src\likhi\engine\xlit_np.py` (584 lines).
Model bundle: `C:\Users\khaled\src\likhi\models\indicxlit-np\` (`config.json`, `model.npz`,
`source_vocabulary.json`, `target_vocabulary.json`).

All facts below were verified by executing the code with
`C:\Users\khaled\src\likhi\.venv\Scripts\python.exe` (Python 3.12.10, NumPy 2.5.3) unless marked
**Uncertain**. Every `file:line` citation is against the file as it stands today; quoted code is
verbatim.

**Read this section first.** The single biggest porting risk is the fairseq EOS-rank rule at
[xlit_np.py:486-492](#43-the-eos-rank-rule--the-single-biggest-risk) (§4.3). Removing it — which looks like
an obvious simplification, because "why would you throw away a finished hypothesis?" — changes the
suggestion list for **15 of 26** test words, including `khacche → খাচ্ছে` becoming `খচ্ছে`. That is
exactly the class of bug that surfaces as bad word suggestions nobody can trace. The second biggest
risk is the length-normalisation divisor: it is `step + 1`, **not** `len(seq)`, and the two differ
by one because the divisor counts the EOS token that is never stored in the sequence.

---

## 1. Model shapes and parameter count

### 1.1 Hyper-parameters (`config.json`, verbatim)

```json
{
 "encoder_layers": 6,
 "decoder_layers": 6,
 "heads": 4,
 "dim": 256,
 "pre_norm": true,
 "activation": "gelu",
 "layernorm_embedding": true,
 "scale_embedding": true,
 "learned_pos": false,
 "padding_idx": 1,
 "source": "IndicXlit v1.0 en-indic (AI4Bharat, MIT)"
}
```

Derived at load time:

| Name | Value | Source |
|---|---|---|
| `dim` | 256 | `cfg["dim"]`, [xlit_np.py:133](#) |
| `heads` | 4 | `cfg["heads"]`, [xlit_np.py:134](#) |
| `head_dim` | 64 | `self.dim // self.heads`, [xlit_np.py:135](#) |
| `pre_norm` | `True` | [xlit_np.py:136](#) |
| `act` | `gelu` (tanh approximation) | `cfg["activation"].startswith("gelu")`, [xlit_np.py:137](#) |
| `embed_scale` | **16.0** exactly (`sqrt(256)`) | [xlit_np.py:138](#) |
| `scaling` | **0.125** exactly (`64 ** -0.5`) | [xlit_np.py:171](#) and [xlit_np.py:226](#) |
| `pos_offset` | **2** (`padding_idx + 1`) | [xlit_np.py:159](#) |
| `max_positions` | 256 (constructor default) | [xlit_np.py:128](#) |

### 1.2 Vocabularies

| | Size | `<s>` | `<pad>` | `</s>` | `<unk>` |
|---|---|---|---|---|---|
| source (`source_vocabulary.json`) | **54** | 0 | 1 | **2** (`src_eos`) | **3** (`src_unk`) |
| target (`target_vocabulary.json`) | **806** | 0 | **1** (`pad`) | **2** (`eos`) | **3** (`unk`) |

Source vocabulary, in full and in order (ids 0..53):

```
<s> <pad> </s> <unk> a i n h t r u e k l d s m o y p v g b c j w z f x q
madeupword0000 madeupword0001
__en__ __as__ __bn__ __brx__ __gom__ __gu__ __hi__ __kn__ __ks__ __mai__ __ml__ __mni__
__mr__ __ne__ __or__ __pa__ __sa__ __sd__ __si__ __ta__ __te__ __ur__
```

`__bn__` is source id **34**. Note the source alphabet is lowercase ASCII letters only — **no
digits, no apostrophe, no uppercase**. Anything else maps to `<unk>` (3).

Target vocabulary, 806 entries, all distinct. Token-length histogram (verified):

| length | count | which |
|---|---|---|
| 1 | **775** | ids 4..778, the actual script characters (Devanagari, Bengali, Tamil, Malayalam, …) |
| >1 | 31 | ids 0,1,2,3 (`<s> <pad> </s> <unk>`) and ids 779..805 (5 `madeupword*`, 22 `__lang__`) |

Consequence: ids **4..778 are exactly the single-code-point characters**, ids 0..3 and 779..805 are
exactly the multi-character specials. There is no overlap, so a Rust port can represent the target
vocabulary as `[Option<char>; 806]` with `None` at 0,1,2,3 and 779..805.

### 1.3 `model.npz` — every array, every shape

263 arrays, all stored as **`float16`**, all cast to `float32` at load
(`f = {k: w[k].astype(np.float32) for k in w.files}`, [xlit_np.py:154](#)).
File size on disk: **21,297,857 bytes** (`np.savez_compressed`).

**Top-level (7 arrays):**

| Name | Shape | Params |
|---|---|---|
| `encoder.embed_tokens.weight` | `(54, 256)` | 13,824 |
| `encoder.layernorm_embedding.weight` | `(256,)` | 256 |
| `encoder.layernorm_embedding.bias` | `(256,)` | 256 |
| `encoder.layer_norm.weight` | `(256,)` | 256 |
| `encoder.layer_norm.bias` | `(256,)` | 256 |
| `decoder.embed_tokens.weight` | `(806, 256)` | 206,336 |
| `decoder.layernorm_embedding.weight` | `(256,)` | 256 |
| `decoder.layernorm_embedding.bias` | `(256,)` | 256 |
| `decoder.layer_norm.weight` | `(256,)` | 256 |
| `decoder.layer_norm.bias` | `(256,)` | 256 |
| `decoder.output_projection.weight` | `(806, 256)` | 206,336 |

**Per encoder layer** `encoder.layers.{i}.` for `i` in 0..5 — 16 arrays, 789,760 params each:

| Suffix | Shape |
|---|---|
| `self_attn.q_proj.weight` | `(256, 256)` |
| `self_attn.q_proj.bias` | `(256,)` |
| `self_attn.k_proj.weight` | `(256, 256)` |
| `self_attn.k_proj.bias` | `(256,)` |
| `self_attn.v_proj.weight` | `(256, 256)` |
| `self_attn.v_proj.bias` | `(256,)` |
| `self_attn.out_proj.weight` | `(256, 256)` |
| `self_attn.out_proj.bias` | `(256,)` |
| `self_attn_layer_norm.weight` | `(256,)` |
| `self_attn_layer_norm.bias` | `(256,)` |
| `fc1.weight` | `(1024, 256)` |
| `fc1.bias` | `(1024,)` |
| `fc2.weight` | `(256, 1024)` |
| `fc2.bias` | `(256,)` |
| `final_layer_norm.weight` | `(256,)` |
| `final_layer_norm.bias` | `(256,)` |

**Per decoder layer** `decoder.layers.{i}.` for `i` in 0..5 — 26 arrays, 1,053,440 params each: the
same 16 as above, **plus** the cross-attention block with the identical shapes:

| Suffix | Shape |
|---|---|
| `encoder_attn.q_proj.weight` | `(256, 256)` |
| `encoder_attn.q_proj.bias` | `(256,)` |
| `encoder_attn.k_proj.weight` | `(256, 256)` |
| `encoder_attn.k_proj.bias` | `(256,)` |
| `encoder_attn.v_proj.weight` | `(256, 256)` |
| `encoder_attn.v_proj.bias` | `(256,)` |
| `encoder_attn.out_proj.weight` | `(256, 256)` |
| `encoder_attn.out_proj.bias` | `(256,)` |
| `encoder_attn_layer_norm.weight` | `(256,)` |
| `encoder_attn_layer_norm.bias` | `(256,)` |

Array-name ordering in the file is: encoder embed/ln-embed, encoder layers 0..5, `encoder.layer_norm.*`,
decoder embed/ln-embed, decoder layers 0..5, `decoder.layer_norm.*`, `decoder.output_projection.weight`.

### 1.4 Total parameter count

**11,487,744 parameters** across **263 arrays** (verified by summing `a.size` over `w.files`).

Breakdown that must reconcile:

```
encoder embed                     13,824
encoder layernorm_embedding          512
encoder layers  6 x 789,760      4,738,560
encoder.layer_norm                   512
                                 ---------
encoder subtotal                 4,753,408

decoder embed                    206,336
decoder layernorm_embedding          512
decoder layers  6 x 1,053,440    6,320,640
decoder.layer_norm                   512
decoder.output_projection        206,336
                                 ---------
decoder subtotal                 6,734,336

TOTAL                           11,487,744
```

> **CRITICAL — the output projection is NOT tied to the decoder embedding.**
> `self.out_proj = f.get("decoder.output_projection.weight", self.dec_embed)` ([xlit_np.py:157](#))
> falls back to the embedding only when the key is absent. In **this** bundle the key is present and
> the two matrices differ: `max |out_proj - dec_embed| = 1.9206543`. A port that assumes weight tying
> (a very common transformer shortcut, and the shapes are identical so nothing will crash) produces
> wrong logits on every step. Load both `(806, 256)` matrices.

Also verified: `dec_embed[1]` (the `<pad>` row) and `enc_embed[1]` are exactly zero vectors.

---

## 2. Numeric primitives — reproduce these formulas exactly

### 2.1 GELU ([xlit_np.py:44-52](#))

The activation used is **not** the exact erf GELU that fairseq uses. It is the Hendrycks & Gimpel
tanh approximation:

```python
_GELU_C = math.sqrt(2.0 / math.pi)          # xlit_np.py:30

def gelu(x):
    return 0.5 * x * (1.0 + np.tanh(_GELU_C * (x + 0.044715 * x * x * x)))
```

Constants: `_GELU_C = sqrt(2/pi) = 0.7978845608028654`, and `0.044715`.
Note the cube is written `x * x * x`, not `x ** 3`.

Measured max |gelu(x) − exact_gelu(x)| over `x ∈ linspace(-6, 6, 100001)` in float32: **4.734e-4**.
(The docstring at [xlit_np.py:48](#) claims 2.3e-4; the measured worst case over that interval is
4.73e-4. The claim is not load-bearing — reproduce the formula, not the docstring.)

`_erf` ([xlit_np.py:33-41](#), Abramowitz & Stegun 7.1.26) is **dead code on the inference path**.
It is kept "for reference and tests". A port does not need it unless it also ports the tests.
Its constants, if needed: `0.3275911`, `0.254829592`, `-0.284496736`, `1.421413741`, `-1.453152027`,
`1.061405429`.

`relu` ([xlit_np.py:55-56](#)) is `np.maximum(x, 0.0)` and is unreachable with this config
(`activation == "gelu"`).

### 2.2 Layer norm ([xlit_np.py:59-62](#))

```python
def layer_norm(x, g, b, eps: float = 1e-5):
    mu = x.mean(-1, keepdims=True)
    var = ((x - mu) ** 2).mean(-1, keepdims=True)
    return (x - mu) / np.sqrt(var + eps) * g + b
```

- `eps = 1e-5`, **inside** the sqrt (`sqrt(var + eps)`, not `sqrt(var) + eps`).
- Variance is the **biased** estimator (divide by N = 256, not N−1).
- Two-pass: mean first, then mean of squared deviations. A single-pass `E[x²] − E[x]²` port will
  drift in the last bits.
- Order of operations: `(x - mu) / sqrt(...)` **then** `* g` **then** `+ b`.

### 2.3 Log-softmax ([xlit_np.py:65-68](#))

```python
def log_softmax(x):
    m = x.max(-1, keepdims=True)
    z = x - m
    return z - np.log(np.exp(z).sum(-1, keepdims=True))
```

Verified over a full decoder pass: output is always finite; observed range on real inputs is
`[-69.68, -0.0049]`. It **cannot** return `-inf`, which is what makes the `gathered * mask` trick in
`score_candidates` safe (§5.4).

### 2.4 Sinusoidal positions ([xlit_np.py:71-82](#))

```python
def sinusoidal(num_positions, dim, padding_idx=1):
    half = dim // 2
    scale = math.log(10000) / (half - 1)
    freqs = np.exp(np.arange(half, dtype=np.float32) * -scale)
    n = num_positions + padding_idx + 1
    pos = np.arange(n, dtype=np.float32)[:, None] * freqs[None, :]
    emb = np.concatenate([np.sin(pos), np.cos(pos)], axis=1)
    if dim % 2:
        emb = np.concatenate([emb, np.zeros((n, 1), np.float32)], axis=1)
    emb[padding_idx] = 0
    return emb.astype(np.float32)
```

With `num_positions=256, dim=256, padding_idx=1`:

- `half = 128`
- `scale = math.log(10000) / 127 = 0.07252236513367073` — **divisor is `half - 1` = 127, not 128.**
  This is the fairseq convention and is off-by-one relative to the "Attention is All You Need"
  formulation. Getting this wrong shifts every frequency.
- `freqs[0] = 1.0`, `freqs[127] = 9.999999e-05` (float32).
- Table shape is `(258, 256)` — `num_positions + padding_idx + 1 = 256 + 1 + 1`.
- Layout is `concat([sin, cos], axis=1)`: columns 0..127 are sines, 128..255 are cosines. **Not**
  interleaved.
- **Row 1 is forced to all-zero** (`emb[padding_idx] = 0`). Row 0 is naturally `[0…0, 1…1]`
  (sin 0, cos 0) and is left alone.
- `dim` is even here, so the odd-dim padding branch never fires.
- Everything stays float32 (verified `dtype == float32`); the `math.log` float64 scalar does not
  promote the float32 array under NumPy's weak-scalar rules.
- `pos_offset = padding_idx + 1 = 2`, so **position p of a sequence reads table row `p + 2`**.
  Row 2 begins `[0.9092974, 0.95844567, 0.987359, 0.99927235, …]` — use this as a unit-test anchor.

### 2.5 Attention ([xlit_np.py:257-269](#))

```python
def _attend(self, q, k, v, causal=False):
    s = q @ k.swapaxes(-1, -2)
    if causal:
        tq, tk = s.shape[-2], s.shape[-1]
        mask = np.triu(np.ones((tq, tk), dtype=bool), k=tk - tq + 1)
        s = np.where(mask, -1e30, s)
    s = s - s.max(-1, keepdims=True)
    p = np.exp(s)
    p /= p.sum(-1, keepdims=True)
    return p @ v
```

- The causal fill value is **`-1e30`, not `-inf`**. Using `-inf` would produce NaN in a
  `max`-subtracted row that is entirely masked. `-1e30` is representable in float32.
- Mask offset `k = tk - tq + 1`. When `tq == tk` (the only case that occurs, in `_decode_full`)
  this is `k = 1`, i.e. the strict upper triangle: position i attends to 0..i inclusive.
- The softmax here is `exp(s - max) / sum`, **without** any epsilon.
- **No padding mask anywhere.** `_encode` has no source padding (single word, batch of 1) and
  `_decode_full` relies on causality to keep trailing `<pad>` positions from influencing real ones
  (§5.3).

### 2.6 Where the `1/sqrt(head_dim)` scaling is applied — two different places

This is subtle and affects float rounding.

**Self-attention (encoder, decoder self-attn), via the fused projection** ([xlit_np.py:173-196](#),
[xlit_np.py:247-255](#)):

```python
scaling = (self.dim // self.heads) ** -0.5          # 0.125
wqkv = np.ascontiguousarray(np.concatenate([wq * scaling, wk, wv], axis=1))
bqkv = np.concatenate([bq * scaling, bk, bv])
...
qkv = h @ a.wqkv + a.bqkv
```

So `q = h @ (wqᵀ_scaled) + (bq * 0.125)` — the scaling is baked into the **weights and the bias**
before the matmul. Verified: `wqkv[:, :256] == wq * 0.125` bit-exactly, `wqkv[:, 256:512] == wk`,
`wqkv[:, 512:] == wv`. `wqkv` is `(256, 768)`, `bqkv` is `(768,)`.

**Cross-attention (`encoder_attn`), applied after the matmul** ([xlit_np.py:329](#),
[xlit_np.py:367](#)):

```python
q = self._split((h @ c.wq + c.bq) * self.scaling)
```

Mathematically identical, **numerically not bit-identical**. Keep both forms as written if you want
to match the reference to the last bit. Note also that cross-attention's k/v are projected once in
`_encode` with the **unfused** `c.wk`/`c.wv` ([xlit_np.py:298](#)) even though a fused `wqkv` exists
on the same `_Attn` object.

All weight matrices are stored row-major as `(out, in)` in the npz and **transposed at load**:
`f[...].T.copy()` ([xlit_np.py:174-190](#), [xlit_np.py:200-203](#)), giving `(in, out)` for `x @ W`.

### 2.7 Head split / merge ([xlit_np.py:238-245](#))

```python
def _split(self, x):     # (..., T, 256) -> (..., 4, T, 64)
    *lead, t, _ = x.shape
    return x.reshape(*lead, t, self.heads, self.head_dim).swapaxes(-3, -2)

def _merge(self, x):     # (..., 4, T, 64) -> (..., T, 256)
    *lead, _h, t, _d = x.shape
    return x.swapaxes(-3, -2).reshape(*lead, t, self.dim)
```

Head `h` owns dim slice `[64h .. 64h+64)`. Standard contiguous-block layout.

### 2.8 Pre-norm residual order ([xlit_np.py:278-291](#) and mirrored in both decoder passes)

`pre_norm` is `True`, so each sub-block is:

```python
h = layer_norm(x, a.ln_g, a.ln_b)     # norm BEFORE
... sublayer ...
x = x + h                              # residual, no norm after
```

and the post-norm branches (`if not self.pre_norm: x = layer_norm(...)`) are dead with this config.
Final `encoder.layer_norm` / `decoder.layer_norm` are applied after the last layer
([xlit_np.py:292-293](#), [xlit_np.py:341-342](#), [xlit_np.py:379-380](#)) — both are present in
this bundle, so both fire.

Embedding path, identical in all three passes:
`x = 16.0 * embed[ids] + pos[2 : 2+T]`, then `layernorm_embedding`.

---

## 3. Source tokenisation — `encode_source` ([xlit_np.py:433-437](#))

```python
def encode_source(self, roman, lang="bn"):
    ids = [self.src_index.get(f"__{lang}__", self.src_unk)]
    ids += [self.src_index.get(ch, self.src_unk) for ch in roman]
    ids.append(self.src_eos)
    return ids
```

Layout: `[ <lang tag>, c0, c1, …, c_{n-1}, </s> ]`, length `len(roman) + 2`.
**No BOS.** The language tag occupies position 0 and therefore consumes positional row `pos[2]`.

Verified examples:

| input | ids | tokens |
|---|---|---|
| `encode_source("khacche", "bn")` | `[34, 12, 7, 4, 23, 23, 7, 11, 2]` | `__bn__ k h a c c h e </s>` |
| `encode_source("ab", "zz")` | `[3, 4, 22, 2]` | unknown lang tag → `<unk>` (3) |
| `encode_source("A1b", "bn")` | `[34, 3, 3, 22, 2]` | uppercase `A` and digit `1` → `<unk>` |

`beam_search` does **not** normalise `roman`; it passes it straight to `encode_source`. Case folding
and character filtering happen upstream in `IndicXlitNumpySystem.suggest` via
`normalize_roman` (§7).

---

## 4. `beam_search` — the complete algorithm

Signature ([xlit_np.py:439-449](#)):

```python
def beam_search(self, roman, *, lang="bn", beam=4, nbest=None, max_len=None,
                lenpen=1.0, enc_kv=None) -> list[tuple[str, float]]
```

### 4.1 Setup ([xlit_np.py:451-465](#))

```python
nbest = nbest or beam                                  # 451
if not roman:
    return []                                          # 452-453
max_len = max_len or min(60, 3 * len(roman) + 5)       # 454
if enc_kv is None:
    enc_kv = self.encode(roman, lang)                  # 455-456

banned = self._banned                                  # 458

tokens = np.full((1,), self.eos, dtype=np.int64)       # 461  decoder starts from </s>
scores = np.zeros((1,), dtype=np.float32)              # 462
seqs: list[list[int]] = [[]]                           # 463
cache = [None] * len(self.dec_layers)                  # 464
finished: list[tuple[float, list[int]]] = []           # 465
```

Exact constants:

- `nbest = nbest or beam` — a caller passing `nbest=0` gets `beam`; Python truthiness, not
  `is None`. Same for `max_len=0` on line 454.
- **`max_len = min(60, 3 * len(roman) + 5)`** — constants `60`, `3`, `5`. Verified table:
  `len 1 → 8`, `len 2 → 11`, `len 5 → 20`, `len 10 → 35`, `len 18 → 59`, `len ≥ 19 → 60`.
  `len(roman)` counts only the roman characters; the lang tag and `</s>` are not counted.
- **Initial state is one hypothesis**: token `</s>` (id 2), score `0.0` (float32), **empty**
  sequence, empty KV cache, empty `finished` list. The starting `</s>` is never stored in `seqs`
  and never appears in the output word.
- The empty-input guard returns `[]` **before** any model work. Verified `beam_search("") == []`.

### 4.2 Per step ([xlit_np.py:467-510](#))

```python
for step in range(max_len):
    lp = self._decode_step(tokens, step, cache, enc_kv) + banned   # 468   (B, V)
    if step == 0:
        lp[:, self.eos] = -np.inf   # min_len = 1                  # 469-470
    cand = scores[:, None] + lp                                    # 471   (B, V)
    flat = cand.ravel()                                            # 472
    k = min(2 * beam, flat.size)                                   # 473
    top = np.argpartition(-flat, k - 1)[:k]                        # 474
    top = top[np.argsort(-flat[top])]                              # 475
    vsize = lp.shape[1]                                            # 476
```

**The `min_len` rule at step 0** ([xlit_np.py:469-470](#)): at `step == 0` only, the EOS column of
`lp` is overwritten with `-inf` **after** `banned` has been added and **before** the `scores`
broadcast. This is fairseq's `min_len = 1`: the model must emit at least one real character. It is
an assignment into `lp`, not an addition — so it is unconditional, and it applies to every row (at
step 0 there is only one row anyway). At every later step EOS is a legal candidate.

**Score accumulation dtype.** `scores` is float32, `lp` is float32, so `cand` and `flat` are
float32. Per-candidate scores are then widened to Python float64 by `float(flat[idx])`
([xlit_np.py:483](#)) and narrowed back to float32 when the surviving beam is rebuilt
(`np.asarray(new_scores, dtype=np.float32)`, [xlit_np.py:505](#)). The round trip is lossless, so
the net effect is: **beam scores accumulate in float32, one addition per step, left to right.**
Accumulate in f32 in Rust, not f64.

**Candidate selection — `2 * beam`, ordering, and ties** ([xlit_np.py:473-475](#)):

1. `flat` is the row-major flattening of `(B, V)`; index `idx` decodes as
   `b, t = divmod(idx, vsize)` where `vsize = V = 806` ([xlit_np.py:482](#)).
2. `k = min(2 * beam, flat.size)`. With `beam=4`: `k = 8` at every step
   (`flat.size` is 806 at step 0 and 3224 afterwards).
3. `np.argpartition(-flat, k - 1)[:k]` returns the indices of the `k` **largest** values of `flat`
   (introselect on the negated array), **in unspecified relative order**.
4. `top[np.argsort(-flat[top])]` then sorts those `k` indices into **descending `flat` order**.
   `np.argsort` defaults to `kind="quicksort"` (introsort) and is **not stable**.

Verified tie behaviour: `np.argsort(-a)` for `a = [1,1,2,2,0.5,2,1]` gives `[3 2 5 1 0 6 4]` while
`kind="stable"` gives `[2 3 5 0 1 6 4]` — the default genuinely reorders equal elements.

**Does it matter in practice? No — but only because ties do not occur.** Verified:

- At step 0 for `"khacche"`, of 806 entries 776 are finite and **all 776 are distinct**
  (max duplicate count = 1).
- Over 26 real words, forcing `kind="stable"` changed **0/26** results.

Rust guidance: sort descending by score with the **flat index ascending as the tiebreak** (i.e. a
total order), and treat any observed tie as a place where the Rust and Python outputs are allowed to
differ. Do not attempt to emulate introselect/introsort. See **Uncertain** §8.1.

The only *systematic* ties are the `-inf` entries (29 banned tokens per row, plus EOS at step 0).
Those sort to the very end of `flat`-descending order and are skipped by the `isfinite` guard, so
their relative order is unobservable. For `beam ≤ 388` they never even enter `top`, because every
row has 777 finite entries (776 at step 0).

**The per-candidate loop** ([xlit_np.py:481-497](#)):

```python
for j, idx in enumerate(top):
    b, t = divmod(int(idx), vsize)
    sc = float(flat[idx])
    if not np.isfinite(sc):
        continue
    if t == self.eos:
        if j < beam:
            finished.append((sc / ((step + 1) ** lenpen), seqs[b]))
        continue
    if len(new_tokens) < beam:
        new_tokens.append(t)
        new_scores.append(sc)
        new_seqs.append(seqs[b] + [t])
        new_src.append(b)
```

Rules, in order of evaluation:

1. `j` is the **rank among all `2*beam` candidates**, counting from 0, and it is incremented for
   every candidate including the ones that are skipped as non-finite or dropped as low-ranked EOS.
   It is **not** a rank among surviving non-EOS candidates.
2. Non-finite scores are skipped entirely (never finished, never extended) but still consume a `j`.
3. EOS candidates never extend a hypothesis (`continue` in both branches).
4. Non-EOS candidates are appended in descending-score order until `beam` of them exist; the rest
   are silently discarded. The new beam is therefore the top-`beam` non-EOS candidates by
   unnormalised cumulative score.
5. `new_seqs.append(seqs[b] + [t])` builds a **new list**; `seqs[b]` is never mutated. That is what
   makes `finished.append((…, seqs[b]))` safe — the stored reference can never change afterwards.
   In Rust, clone the prefix or use `Rc<Vec<u16>>` with structural sharing.

### 4.3 The EOS-rank rule — THE SINGLE BIGGEST RISK

```python
if t == self.eos:
    # fairseq only finalizes an EOS that ranks within the top `beam` candidates;
    # lower-ranked EOS entries are dropped, otherwise weak short words fill the
    # finished list and end the search before longer correct words complete.
    if j < beam:
        finished.append((sc / ((step + 1) ** lenpen), seqs[b]))
    continue
```
— [xlit_np.py:486-492](#)

**An EOS candidate is finalised if and only if its rank `j` among the `2*beam` sorted candidates is
strictly less than `beam`.** An EOS at `j >= beam` is thrown away completely: the hypothesis is
neither finished nor carried forward, and that branch of the search dies.

This is not an optimisation. It is load-bearing, for two compounding reasons: it keeps weak short
completions out of `finished`, and — because the loop breaks as soon as `len(finished) >= beam`
(§4.4) — it stops those weak completions from ending the search before a longer, better word has a
chance to finish.

**Measured impact.** Running the identical beam search with and without the `j < beam` guard over
26 real romanizations, **15 of 26 produce a different suggestion list**:

| roman | with the rule (correct) | without the rule (wrong) |
|---|---|---|
| `khacche` | খাচ্ছে, খচ্ছে, খাচ্চে, খচ্চে | খচ্ছে, খচ্চে, খাচ্ছ, খচ্ছ |
| `bangla` | বাংলা, বাঙ্গলা, ব্যাংলা, বাংলায় | বাংলা, বংলা, বাংল, বংল |
| `kritagyota` | কৃতজ্ঞতা, কৃতাজ্ঞতা, কৃতজ্ঞটা, কৃতাজ্ঞটা | কৃতযোতা, কৃতযোত, কৃৎযোত, কৃতজ্ঞত |
| `kotha` | কথা, কোথা, কোঠা, কোটা | কথা, কোথ, কোঠ, কথ |
| `sristi` | সৃষ্টি, শ্রীষ্টি, শ্রীস্তি, সৃষ্টিতে | সৃষ্টি, সৃষ্টী, সৃষ্টির, সৃষ্টিত |
| `byabosthapona` | ব্যবস্থাপনা, ব্যাবস্থাপনা, ব্যবস্তাপনা, ব্যবস্থাপনার | ব্যবস্থাপনা, ব্যবস্তাপনা, ব্যৱস্থাপনা, ব্যবস্থাপন |

(also differs: `qqqq`, `bangladesh`, `onubhuti`, `porikkha`, `prottasha`, `dhonnobad`, `shopno`,
`britto`, `uddessho`. Identical for: `amar`, `protishruti`, `shubhechchha`, `ovinondon`,
`durbhagyoboshoto`, `nirbachon`, `ghurnijhor`, `boi`, `jol`, `pakhi`, `shomvob`.)

The rule fires often: for `"khacche"` at beam 4, 2 EOS candidates were dropped and 6 kept over 7
steps. In the verified trace, step 4 `j=7` dropped `খচ্ছ</s>` and step 5 `j=5` dropped `খাচ্ছ</s>`.

### 4.4 Early stop ([xlit_np.py:498-502](#))

```python
if len(finished) >= beam or not new_tokens:
    break
```

**There are exactly two stop conditions, and no others:**

1. `len(finished) >= beam` — enough hypotheses have terminated. Note `>=`, not `==`: a single step
   can push `finished` past `beam`.
2. `not new_tokens` — no finite non-EOS candidate survived. In practice unreachable: at most `B`
   of the `2*beam` candidates can be EOS (one per row) and `B <= beam`, so at least `beam` non-EOS
   candidates always remain.
3. The `for step in range(max_len)` bound.

The code carries an explicit warning against adding a third condition ([xlit_np.py:500-502](#)):

```python
# No other early stop: a live hypothesis' length-normalized score can still improve as
# it grows, so comparing it against finished ones is not a valid bound (it dropped the
# model's own best answer, খাচ্ছে for "khacche", at beam 4).
```

**Do not add the standard fairseq "best possible score of any live hypothesis ≤ worst finished
score" pruning.** With `lenpen = 1.0` the normalised score of a live hypothesis is not monotone in
its length, so that bound is invalid here. This was tried and it dropped the correct answer.

Consequence of `>= beam` with the `j < beam` rule: `finished` can hold at most `2*beam - 1` entries
(it was `< beam` at the start of the step, and a step adds at most `beam`). Observed for `beam=4`:
`finished` lengths of 4, 5 and 6.

### 4.5 Beam re-ordering and KV cache ([xlit_np.py:503-510](#))

```python
sel = np.asarray(new_src)
tokens = np.asarray(new_tokens, dtype=np.int64)
scores = np.asarray(new_scores, dtype=np.float32)
seqs = new_seqs
for i in range(len(cache)):
    if cache[i] is not None:
        cache[i][0] = cache[i][0][sel]
        cache[i][1] = cache[i][1][sel]
```

Ordering that matters:

- The cache is **appended to inside `_decode_step`** ([xlit_np.py:318-322](#)) for **all `B` current
  rows**, and only **then** reordered here by `sel`. So at the end of step `s` the cache holds
  `s + 1` time steps, gathered along the batch axis.
- `sel` is a **gather with repeats**: the same source beam can appear several times (e.g. at step 0,
  `sel = [0,0,0,0]` expands `B` from 1 to 4). Beams can also shrink or reorder.
- Cache tensors are `(B, heads=4, t, head_dim=64)`; `cache[i][sel]` gathers axis 0.
- `cache[i] is None` on the very first step; `_decode_step` sets it to `[k, v]` rather than
  concatenating.

### 4.6 Length normalisation

```python
finished.append((sc / ((step + 1) ** lenpen), seqs[b]))      # xlit_np.py:491
```

**`normalized = cumulative_logprob / (step + 1) ** lenpen`**, `lenpen` default `1.0`.

The divisor is `step + 1`, where `step` is the 0-based loop index. At step `s` every live sequence
in `seqs` has exactly `s` tokens, so:

> **divisor = (number of characters in the hypothesis) + 1.** The `+ 1` is the EOS token, which is
> counted in the length but never stored in `seqs` and never rendered into the word.

`sc` is `float(flat[idx])` — a float64 widening of a float32 value — and `(step + 1) ** lenpen` is a
Python float, so **the division is done in float64** and `finished` holds float64 scores. Do the
division in f64 in Rust.

Verified against the independent teacher-forced path (§5), which agrees to ~1e-6:

| roman | word | `beam_search` score | `score_candidates` raw | raw / (len+1) |
|---|---|---|---|---|
| khacche | খাচ্ছে | −0.07394757 | −0.51763314 | −0.51763314/7 = −0.07394759 |
| khacche | খচ্ছে | −0.21034900 | −1.26209283 | −1.26209283/6 = −0.21034880 |
| amar | আমার | −0.06302822 | −0.31514108 | −0.31514108/5 = −0.06302822 |
| protishruti | প্রতিশ্রুতি | −0.00059859 | −0.00718286 | −0.00718286/12 = −0.00059857 |

`lenpen` sensitivity (same input, `beam=4`, `"khacche"`) — note the ranking of খাচ্চে vs খচ্চে flips
between `lenpen=0.0` and `lenpen=0.5`:

| lenpen | result |
|---|---|
| 0.0 | খাচ্ছে −0.51763296, খচ্ছে −1.26209402, খচ্চে −2.90191579, খাচ্চে −2.93871498 |
| 0.5 | খাচ্ছে −0.19564687, খচ্ছে −0.51524773, খাচ্চে −1.11072986, খচ্চে −1.18470216 |
| 1.0 | খাচ্ছে −0.07394757, খচ্ছে −0.21034900, খাচ্চে −0.41981643, খচ্চে −0.48365263 |
| 1.5 | খাচ্ছে −0.02794955, খচ্ছে −0.08587462, খাচ্চে −0.15867569, খচ্চে −0.19745036 |

### 4.7 Fallback when nothing finished ([xlit_np.py:511-514](#))

```python
if not finished:  # ran out of length: take live hyps
    finished = [
        (float(s) / (max_len**lenpen), q) for s, q in zip(scores, seqs, strict=True)
    ]
```

Three things a port must get right:

1. The trigger is `finished` being **empty**, not "fewer than `nbest`". If even one hypothesis
   finished, the fallback never runs and the output can be shorter than `nbest`.
2. The divisor is **`max_len ** lenpen`, not `len(q) ** lenpen` and not `(len(q) + 1) ** lenpen`.**
   When the loop completed all `max_len` iterations these coincide with `len(q)`; when the loop
   exited via `not new_tokens` they do not. Reproduce `max_len`.
3. The un-terminated hypotheses keep their raw `scores` (float32), widened by `float(s)`, and are
   divided in float64. No EOS log-prob is added.

`zip(..., strict=True)` will raise if `scores` and `seqs` ever differ in length; they cannot.

Verified fallback outputs (forced with a small `max_len`):

```
"khacche" max_len=1 -> খ −0.004947562, ক −5.350771, ভ −9.408209, ঝ −10.482838
"khacche" max_len=4 -> খাচ্ −0.107760020, খচ্ছ −0.315372884, খচ্চ −0.725270748, খাঁচ −1.412401319
"bangladesh" max_len=3 -> বাং −0.002549825, বংল −1.771675428, বাঙ −2.193790754, বঙ্ −2.484376431
```

With the default `max_len` the fallback is unreachable for any realistic input — in the verified
runs every word finished at least four hypotheses.

### 4.8 Final sort, dedup and output ([xlit_np.py:515-525](#))

```python
finished.sort(key=lambda x: -x[0])
out: list[tuple[str, float]] = []
seen: set[str] = set()
for sc, q in finished:
    word = "".join(self.tgt_vocab[i] for i in q if i != self.unk)
    if word and word not in seen:
        seen.add(word)
        out.append((word, sc))
    if len(out) >= nbest:
        break
return out
```

- **`list.sort` is Timsort and is STABLE.** The key is `-x[0]` (negated float64 score), so ties are
  broken by **insertion order into `finished`**, which is: step ascending, then candidate rank `j`
  ascending within a step. In Rust use a stable sort (or `sort_by` with a
  `partial_cmp` on the negated score and no index tiebreak) applied to a list built in exactly that
  order. `sort_unstable_by` is wrong here.
- **`<unk>` (id 3) is filtered out of the rendered string, not skipped as a hypothesis.** `unk` is
  **not** in the banned mask (verified `banned[3] == 0.0`), so the decoder may legally emit it; when
  it does, the character vanishes from the word while its log-prob stays in the score and its
  position still counts toward the length divisor. Scanned 325 inputs (300 random roman strings plus
  all 25 two-letter combinations of `qxzfv`) and found **0** hypotheses containing `<unk>` — rare,
  but the code path exists and must be ported. See §8.3.
- Empty words are skipped (`if word and …`) without consuming an `nbest` slot. A hypothesis whose
  tokens are all `<unk>` renders as `""` and is dropped.
- Dedup is on the **exact rendered string**, before any Unicode normalisation. The first (highest
  scoring) occurrence wins and keeps its score.
- The `len(out) >= nbest` check runs **after** the append, so the loop can end on a non-appending
  iteration only by exhausting `finished`.
- The returned list can be **shorter than `nbest`** (verified: `nbest=10` on `"amar"` returned 4).

### 4.9 The banned mask ([xlit_np.py:227-234](#)) — full membership rule

```python
# Tokens the decoder must never emit: pad, bos, language tags, dictionary padding words.
banned = np.zeros(len(self.tgt_vocab), dtype=np.float32)
banned[self.pad] = -np.inf
banned[self.tgt_index["<s>"]] = -np.inf
for t, i in self.tgt_index.items():
    if (t.startswith("__") and t.endswith("__")) or t.startswith("madeupword"):
        banned[i] = -np.inf
self._banned = banned
```

The rule is the union of:

1. `self.pad` — the id of the literal token `"<pad>"` → **1**.
2. the id of the literal token `"<s>"` → **0**.
3. every token `t` with `t.startswith("__") and t.endswith("__")` — **both** conditions, `and`.
   (A hypothetical token `"__x"` or `"x__"` would not be banned; `"__"` itself would be, since
   `"__".startswith("__")` and `"__".endswith("__")` are both true.)
4. every token `t` with `t.startswith("madeupword")` — prefix only, no suffix condition.

`self.tgt_index` is built as `{t: i for i, t in enumerate(self.tgt_vocab)}` ([xlit_np.py:146](#)),
so for duplicate vocabulary entries the **last** id would win. Verified: all 806 target entries are
distinct, so this cannot bite for this bundle.

**Resolved membership for this model — exactly 29 ids:**

```
0   <s>
1   <pad>
779 madeupword0000   780 madeupword0001   781 madeupword0002
782 madeupword0003   783 madeupword0004
784 __en__   785 __as__   786 __bn__   787 __brx__  788 __gom__  789 __gu__
790 __hi__   791 __kn__   792 __ks__   793 __mai__  794 __ml__   795 __mni__
796 __mr__   797 __ne__   798 __or__   799 __pa__   800 __sa__   801 __sd__
802 __si__   803 __ta__   804 __te__   805 __ur__
```

**Explicitly NOT banned:** `</s>` (2) — it is the terminator — and **`<unk>` (3)**.
Verified: `banned[2] == 0.0`, `banned[3] == 0.0`.

Mechanics:

- The mask is `float32` and is **added**, not multiplied or used as a boolean:
  `lp = self._decode_step(...) + banned` ([xlit_np.py:468](#)). Non-banned entries add `0.0`,
  which is a bit-exact no-op.
- The log-probabilities are **not renormalised** after banning. A port must not re-run softmax over
  the allowed subset.
- The mask is used **only in `beam_search`**. `_decode_full` / `score_candidates` do **not** apply
  it (verified: `decode_full` gives finite `-31.89` at `<pad>` and `-32.47` at `__bn__`).

The same rule is duplicated at `scripts/build_rust_data.py:281-287`, which pre-computes the mask
into the `model.lkw` bundle so the Rust side reads it rather than rebuilding it.

### 4.10 Verified golden trace — `beam_search("khacche", beam=4)`

Use this as the first integration test. Format: `j, source beam b, token t, cumulative score`.

```
step 0  B=1 k=8
  j=0 b=0 t=353 'খ'   -0.004948  KEEP
  j=1 b=0 t=128 'ক'   -5.350771  KEEP
  j=2 b=0 t=281 'ভ'   -9.408209  KEEP
  j=3 b=0 t=509 'ঝ'  -10.482838  KEEP
  j=4..7                          discarded (beam full)
step 1  B=4 k=8
  j=0 b=0 t=67  'া'   -0.422661  KEEP   (prefix খ)
  j=1 b=0 t=275 'চ'   -1.082497  KEEP   (prefix খ)
  j=2 b=1 t=67  'া'   -5.915315  KEEP   (prefix ক)
  j=3 b=1 t=85  '্'   -6.607183  KEEP   (prefix ক)
step 2  keeps: খাচ -0.429965, খচ্ -1.083258, খাঁ -5.648768, কাচ -5.998356
step 3  keeps: খাচ্ -0.431040, খচ্ছ -1.261492, খচ্চ -2.901083, খাঁচ -5.649605
step 4  keeps: খাচ্ছ -0.516818, খচ্ছে -1.262039, খচ্চে -2.901804, খাচ্চ -2.937608
        j=7 t=2 '</s>' -9.832392 on prefix খচ্ছ  -> EOS DROPPED (j >= beam)
step 5  j=0 keep খাচ্ছে -0.517564
        j=1 t=2 '</s>' -1.262094 prefix খচ্ছে -> FINALISED  -1.262094/6 = -0.210349
        j=2 t=2 '</s>' -2.901916 prefix খচ্চে -> FINALISED  -2.901916/6 = -0.483653
        j=3 keep খাচ্চে -2.938583
        j=5 t=2 '</s>' -9.817532 prefix খাচ্ছ -> EOS DROPPED (j >= beam)
        j=4,6 keep খাচ্ছি, খাচ্চি ; j=7 discarded
step 6  j=0 t=2 '</s>' -0.517633 prefix খাচ্ছে -> FINALISED  -0.517633/7 = -0.073948
        j=1 t=2 '</s>' -2.938715 prefix খাচ্চে -> FINALISED  -2.938715/7 = -0.419816
        j=2 t=2 '</s>' -7.969903 prefix খাচ্ছি -> FINALISED  -7.969903/7 = -1.138558
        j=3 t=2 '</s>' -10.442292 prefix খাচ্চি -> FINALISED -10.442292/7 = -1.491756
        BREAK: finished=6 >= beam=4
```

Final (after stable sort and dedup, `nbest=4`):

```
খাচ্ছে  -0.07394756589617048
খচ্ছে   -0.21034900347391763
খাচ্চে  -0.4198164258684431
খচ্চে   -0.48365263144175213
```

Further verified results (`beam=4`, `lenpen=1.0`, default `max_len`):

```
amar         আমার -0.063028216, আমর -0.635211170, অমার -0.639333868, অমর -0.792307138
bangla       বাংলা -0.025075272, বাঙ্গলা -0.365196675, ব্যাংলা -0.501328707, বাংলায় -0.638138890
kritagyota   কৃতজ্ঞতা -0.049302, কৃতাজ্ঞতা -0.109887, কৃতজ্ঞটা -0.487478, কৃতাজ্ঞটা -0.500659
protishruti  প্রতিশ্রুতি -0.000598591, প্রতিশ্রুতী -0.506657084, প্রতিশ্রূতি -0.516862233, প্রতীশ্রুতি -0.583218535
beam=1:      khacche -> খাচ্ছে -0.07394779579980033   (note: differs from the beam=4 value in the
                                                       7th decimal — different summation path)
```

---

## 5. `score_candidates` — teacher-forced batch scoring

Signature ([xlit_np.py:387-394](#)):

```python
def score_candidates(self, roman, words, *, lang="bn", enc_kv=None) -> np.ndarray
```

Returns `log P(word | roman)` per word: the **raw sum** over characters including `</s>`, with **no
length normalisation** and **no banned mask**.

### 5.1 Body, verbatim ([xlit_np.py:399-429](#))

```python
if not words:
    return np.zeros(0, dtype=np.float32)
if enc_kv is None:
    enc_kv = self.encode(roman, lang)
ids: list[list[int]] = []
ok = np.ones(len(words), dtype=bool)
for i, w in enumerate(words):
    seq = []
    for ch in w:
        j = self.tgt_index.get(ch)
        if j is None:
            ok[i] = False
            break
        seq.append(j)
    ids.append(seq + [self.eos])
t = max(len(s) for s in ids)
b = len(ids)
inp = np.full((b, t), self.pad, dtype=np.int64)
out = np.full((b, t), self.pad, dtype=np.int64)
inp[:, 0] = self.eos
for i, s in enumerate(ids):
    n = len(s)
    out[i, :n] = s
    if n > 1:
        inp[i, 1:n] = s[:-1]
lp = self._decode_full(inp, enc_kv)  # (B, T, V)
gathered = np.take_along_axis(lp, out[:, :, None], axis=2)[:, :, 0]
mask = out != self.pad
total = (gathered * mask).sum(axis=1)
total[~ok] = -np.inf
return total.astype(np.float32)
```

### 5.2 Tokenisation and the `ok` flag

- Words are split **per Python character (code point)**, looked up directly in `tgt_index`. Since
  target ids 4..778 are exactly the single-character tokens, this is a plain char→id table.
- On the first unmappable character, `ok[i] = False` and the loop `break`s — so `seq` is the
  **truncated prefix**, and `ids[i] = prefix + [eos]`. The truncated row is still built, still
  batched, still run through the decoder; only at the very end is its total overwritten with `-inf`
  ([xlit_np.py:428](#)). A port must keep the truncated row in the batch (or at least must not let
  its omission change `t`, which is computed over the truncated `ids`).
- `ok` is a per-word boolean array; `total[~ok] = -np.inf` is the only use.

### 5.3 Exact `inp` / `out` layout

For a word with token ids `c0..c_{m-1}` (`m` characters), `ids[i] = [c0, …, c_{m-1}, eos]`,
`n = m + 1`. With `t = max_i n_i` and `pad = 1`, `eos = 2`:

```
row i:  inp = [ eos, c0, c1, …, c_{m-1},   pad, pad, … ]   (length t)
        out = [ c0,  c1, c2, …, eos,       pad, pad, … ]   (length t)
                ^^^ position 0 of every row is eos, set before the per-row fill
```

`inp` is `out` shifted right by one with `</s>` as the BOS marker — the same convention as
`beam_search`'s initial `tokens = [eos]`. Trailing positions `n..t-1` are `pad` in **both**.

Verified for `words = ["খাচ্ছে", "আমার"]` (`t = 7`):

```
inp = [[  2 353  67 275  85 384 103]
       [  2 350 150  67  87   1   1]]
out = [[353  67 275  85 384 103   2]
       [350 150  67  87   2   1   1]]
```

Note the `if n > 1` guard on line 422: a zero-character word gives `n = 1` and the slice
`inp[i, 1:1] = s[:-1]` would be an empty-to-empty assignment anyway, so the guard is defensive only.

**Why no padding mask is needed.** `_decode_full` applies only a causal mask. Trailing `pad`
positions are strictly to the right of the real tokens, so they can never influence a real
position's output, and their own (garbage) outputs are removed by `mask = out != self.pad`. The
encoder side has no padding at all — the batch dimension is candidates, and every candidate shares
the same single-word source. A port must not add padding-aware attention; it would change nothing
mathematically but would change the float results.

### 5.4 Gather, mask and sum

- `np.take_along_axis(lp, out[:, :, None], axis=2)[:, :, 0]` picks `lp[b, s, out[b, s]]` → `(B, T)`.
- `mask = out != self.pad` is boolean. `gathered * mask` relies on `False → 0.0`.
  **This is only safe because `log_softmax` never returns `-inf`** (§2.3): verified
  `np.float32(-inf) * False == nan`. If a port introduces `-inf` log-probs (for example by applying
  the banned mask here — don't), every padded row becomes `NaN`.
- `.sum(axis=1)` is NumPy's **pairwise** summation over float32, not a naive left-to-right
  accumulation. For a typical `T ≈ 12` the two agree exactly (verified identical for
  `protishruti`/`প্রতিশ্রুতি`: `-0.007182859815657139` both ways), so this is not a practical concern
  at these lengths — unlike the 806-element `sum` inside `log_softmax` (§6).
- The result is already float32; `.astype(np.float32)` is a no-op except it copies.
- `total[~ok] = -np.inf` is applied **after** the sum, so an invalid word's decoder work is wasted
  but harmless.

### 5.5 Edge cases, verified

| call | result | note |
|---|---|---|
| `score_candidates(r, [])` | `array([], dtype=float32)` | early return, no encoder run |
| `score_candidates("khacche", ["zz"])` | `-inf` | `z`/`Z` not in the Bengali-side vocab |
| `score_candidates("khacche", [""])` | **`-17.601156`** | the **empty word scores finite** |
| `score_candidates("khacche", ["খাচ্ছে", "", "Q"])` | `[-0.51763314, -17.601158, -inf]` | |

The empty-word case is a live trap: `ok[i]` stays `True` (the character loop never runs), `ids[i]`
is `[eos]`, and the "score" returned is `log P(</s> | roman)` — the probability the model assigns to
producing nothing. It is finite and can be *larger* than a real word's score for a long roman input.
Callers must filter empty strings themselves. Also note `-17.601156` vs `-17.601158` between the two
calls above: the batch width `t` changed, so the decoder's matmul shapes changed, so the last bits
changed. That is normal and expected.

### 5.6 Cross-check between the two paths

`beam_search` and `score_candidates` are independent implementations of the same probability, and
they agree. `core.py:401` relies on this:

```python
# The beam already knows log P(word | roman): its scores are length-normalized
# (sum / (chars + EOS)), so undo that to match score_candidates' raw sums.
ft.xlit_logp = float(lp) * (len(word) + 1)
```

Verified agreement to < 2e-6 for all 16 (roman, word) pairs in §4.6's table. **Use this as the
Rust port's self-test**: implement both paths, then assert
`|beam_norm * (len(word) + 1) - score_candidates(word)| < 1e-5`. It catches the length-divisor
off-by-one, the `inp`/`out` shift, and the positional-offset immediately.

(It also shows why `<unk>` in a hypothesis would be a bug for the *caller*: `len(word)` counts
rendered characters, so a dropped `<unk>` makes core.py's un-normalisation wrong by one factor.
See §8.3.)

---

## 6. Float-reproducibility budget

The goldens are checked on **ranking equality plus a score tolerance**, not bit equality — see
`scripts/dump_goldens.py:10-17`:

> transformer outputs (beam search, teacher-forced scores) are float32 matrix products, and a
> different summation order changes the last bits. Those are checked on *ranking* — same words in
> the same order — with scores compared to a tolerance

Known summation-order sensitivities, measured:

| site | reduction | measured naive-vs-NumPy gap |
|---|---|---|
| `log_softmax` `np.exp(z).sum(-1)` over V=806 f32 | pairwise | **3.05e-5** absolute on a sum of ~59 |
| `layer_norm` `x.mean(-1)` over 256 f32 | pairwise | ~4.5e-8 |
| `score_candidates` `.sum(axis=1)` over T≈12 f32 | pairwise | 0 at these lengths |
| beam score accumulation | explicit, one add per step | exact, left-to-right |

Practical guidance: the gaps above are ~1e-5 while the gaps between competing candidate scores are
~0.1–0.5 (see the trace in §4.10 — `খাচ্ছে` beats `খচ্ছে` by 0.14). Ranking is safe. Do **not** chase
bit-exactness; do assert ranking equality against `tests/goldens/xlit_beam.jsonl` and
`xlit_score.jsonl` with an absolute score tolerance of about 1e-4.

Pre-computed bundle: `scripts/build_rust_data.py` already emits `models/rust/model.lkw` with the
transposes, the fused `wqkv`/`bqkv` (scaling folded into q), the `(258, 256)` sinusoidal table and
the 29-entry banned mask **computed once in the same NumPy that produced the reference outputs**
(`build_rust_data.py:18-22`, `:216`, `:242-243`, `:281-287`). Prefer reading that file over
recomputing any of it in Rust — it removes a whole class of rounding divergence, and it is the
reason engine startup does no arithmetic.

---

## 7. `IndicXlitNumpySystem` — the adapter above the beam ([xlit_np.py:528-583](#))

Included because it changes what a caller actually sees.

```python
def _suggest(self, roman: str, k: int) -> tuple[str, ...]:
    hyps = self.model.beam_search(roman, beam=max(self.beam, k), nbest=max(self.beam, k))
    hyps = [(canonical(w), s) for w, s in hyps]
    if not hyps:
        return ()
    if self.word_prob is None:
        return tuple(w for w, _ in hyps)[:k]
    model_p = np.exp(np.array([s for _, s in hyps], dtype=np.float64))
    model_p /= model_p.sum() or 1.0
    lm_p = np.array([self.word_prob.get(w, 0.0) for w, _ in hyps], dtype=np.float64)
    if lm_p.sum() > 0:
        lm_p /= lm_p.sum()
    mixed = self.alpha * model_p + (1 - self.alpha) * lm_p
    order = np.argsort(-mixed, kind="stable")
    return tuple(hyps[i][0] for i in order)[:k]
```

- `beam = nbest = max(self.beam, k)` — the requested count raises the beam width.
- `canonical()` (NFC + candrabindu reorder + modern khanda-ta, `textnorm.py:55-63`) is applied
  **after** `beam_search`'s dedup, and there is **no second dedup**. Two distinct model outputs that
  canonicalise to the same string would both survive. Verified: 0 collisions over 19 inputs at
  beam 8, so this is latent rather than active.
- `model_p = exp(normalized_score)` — it exponentiates the **length-normalised** scores, then
  renormalises them to sum to 1. That is not a probability distribution in any principled sense; it
  is the formula, reproduce it.
- `model_p.sum() or 1.0` is Python `or` on a float: a sum of exactly `0.0` falls back to `1.0`.
- `lm_p` is renormalised **only if** its sum is `> 0`.
- `mixed = alpha * model_p + (1 - alpha) * lm_p`, `alpha` default **0.9**.
- `np.argsort(-mixed, kind="stable")` — **explicitly stable** here (unlike line 475), so ties keep
  beam order.
- `suggest` calls `self._cached(normalize_roman(roman), k)`, an `lru_cache(maxsize=cache_size)`
  with `cache_size` default **8192**, keyed on `(normalized_roman, k)`. `normalize_roman`
  (`textnorm.py:96-101`) case-folds and strips everything outside `[a-z0-9']`.
- The rescoring branch is off by default (`rescore=False`); when on, `self.name` becomes
  `"indicxlit-np+rerank"` and `word_prob` keys are `canonical()`-ed at load.

Note that the production engine (`core.py:390-402`) bypasses this adapter and calls
`beam_search(r, beam=self.beam, nbest=self.beam, enc_kv=enc_kv)` directly, then
`score_candidates(r, words, enc_kv=enc_kv)` for candidates the beam missed
(`core.py:443`), sharing one encoder run.

---

## 8. Uncertain

Things I could not establish with certainty. Treat each as a place to add a test rather than a
place to guess.

### 8.1 Tie-breaking in `argpartition` + `argsort`

`np.argpartition` (introselect) and `np.argsort(kind="quicksort")` (introsort) are both **unstable**
and their behaviour on equal keys is not documented, not guaranteed across NumPy versions, and not
reproducible in another language.

What I verified: no ties occur in practice — 776/776 distinct finite values at step 0 for
`"khacche"`, and forcing `kind="stable"` changed **0/26** word results. What I did **not** verify:
that ties never occur for any input. Two structurally different hypotheses can in principle reach
bit-identical float32 cumulative scores.

Recommendation: in Rust, impose a total order (descending score, then ascending flat index) so the
port is at least self-consistent and deterministic, and accept that a tie is an allowed divergence
point. Do not attempt to replicate NumPy's introselect. If a golden ever fails on a tie, the golden
is the thing that should be relaxed.

### 8.2 Whether `finished` can ever hold two entries with equal normalised scores

The final `finished.sort` is stable and therefore *does* define behaviour for ties (insertion order:
step, then `j`). I could not construct such a tie, so I could not test that the stable-sort
behaviour is actually observable. Port it as stable anyway — it is free.

### 8.3 `<unk>` emission

`<unk>` (id 3) is not banned and would be silently stripped from the rendered word
([xlit_np.py:519](#)). I scanned 325 inputs and found zero occurrences, so I cannot say what the
downstream consequences look like in practice. Two known consequences if it ever happens:

- the length divisor `(step + 1)` counts the `<unk>` position but the rendered word does not, so
  `core.py:401`'s `float(lp) * (len(word) + 1)` un-normalisation is wrong by a factor;
- a hypothesis consisting only of `<unk>` renders as `""` and is dropped by the `if word` guard.

Whether this is a latent bug or deliberate is not recorded anywhere I could find. A Rust port should
reproduce the behaviour and, separately, add an assertion or a counter so it becomes visible.

### 8.4 The `not new_tokens` early stop

I argued in §4.4 that it is unreachable (at most `B ≤ beam` of `2*beam` candidates can be EOS, and
`beam` non-EOS candidates always remain), and I did not observe it in any run. I have not proven it
for pathological `beam` values — e.g. `beam > 388`, where at step 0 `k = min(2*beam, 806) = 806`
would include the 30 non-finite entries. The production callers use `beam = 4` or
`max(beam, k)` with small `k`, so this is theoretical. Keep the check; it costs nothing.

### 8.5 The GELU error claim

The docstring at [xlit_np.py:48-50](#) says the tanh approximation is "within 2.3e-4" of exact GELU
and "measured as having no effect on any evaluation set". My measurement over
`linspace(-6, 6, 100001)` in float32 gives a max absolute error of **4.734e-4**. I did not find the
measurement the docstring refers to, and I did not re-run any evaluation set. The formula is what
matters and it is unambiguous; the constant in the prose is not.

### 8.6 `max_positions`

`XlitTransformer.__init__` takes `max_positions: int = 256` ([xlit_np.py:128](#)) and nothing in the
repo passes anything else, so the table is `(258, 256)`. Source sequences use rows
`2 .. 2 + len(roman) + 1`, decoder steps use rows `2 .. 2 + max_len - 1 ≤ 61`. A roman input longer
than 255 characters would index past the table and raise. `normalize_roman` does not truncate, and
`dump_goldens.py:58` deliberately includes `"x" * 64`, which is fine. I did not test the >255 case.

---

## 9. Constant checklist for the implementer

Every magic value a port must reproduce exactly:

```
dim               256          heads             4            head_dim         64
encoder_layers    6            decoder_layers    6
embed_scale       16.0         attn scaling      0.125
layer_norm eps    1e-5         (inside sqrt, biased variance over 256)
GELU              0.5*x*(1+tanh(0.7978845608028654*(x + 0.044715*x*x*x)))
causal mask fill  -1e30        (NOT -inf)
mask offset k     tk - tq + 1
sinusoid scale    log(10000)/(half-1) = log(10000)/127 = 0.07252236513367073
sinusoid layout   concat([sin, cos], axis=1); row padding_idx forced to 0
pos table rows    num_positions + padding_idx + 1 = 258
padding_idx       1            pos_offset        2
src vocab         54           tgt vocab         806
src: pad 1  eos 2  unk 3       __bn__ 34
tgt: bos 0  pad 1  eos 2  unk 3
banned ids        {0, 1, 779..805}  -> 29 ids, value -inf, ADDED to log-probs
beam (default)    4            nbest = nbest or beam
max_len           min(60, 3*len(roman) + 5)
candidates/step   2*beam,   k = min(2*beam, B*V)
min_len rule      lp[:, eos] = -inf at step 0 only
EOS finalised iff rank j < beam
early stop        len(finished) >= beam  OR  no non-EOS candidate  OR  step == max_len
lenpen (default)  1.0
normalisation     cumulative / (step + 1)**lenpen          (float64 division)
fallback norm     cumulative / max_len**lenpen             (float64 division)
final sort        STABLE, descending by normalised score
render            drop token id 3 (<unk>); skip empty strings; dedup on exact string
adapter alpha     0.9          lru_cache size    8192
model params      11,487,744 across 263 float16 arrays
```
