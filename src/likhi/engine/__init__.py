"""Core engine: normalization, candidate generation, ranking, personalization.

Pure Python. Heavy optional pieces (CTranslate2 model, marisa tries) are imported lazily so the
package imports and the deterministic parts run without them.
"""
