"""Keep BLAS single-threaded for the engine.

The transliteration model works on tiny matrices (a few hundred rows). Multi-threaded BLAS gains
nothing there and, on a shared machine, thread pools from several NumPy processes fight each
other; one keystroke was measured at 2-4 s under that contention versus ~100 ms alone.
Call `limit_blas_threads()` before NumPy is imported.
"""

from __future__ import annotations

import os

_VARS = ("OPENBLAS_NUM_THREADS", "OMP_NUM_THREADS", "MKL_NUM_THREADS", "NUMEXPR_NUM_THREADS")


def limit_blas_threads(n: int = 1) -> None:
    for var in _VARS:
        os.environ.setdefault(var, str(n))
