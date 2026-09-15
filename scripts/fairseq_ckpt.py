"""Read a PyTorch/fairseq checkpoint into numpy arrays without importing torch.

torch.save (zip format, PyTorch >= 1.6) stores a pickle at ``<name>/data.pkl`` whose tensors are
persistent references to raw storages at ``<name>/data/<key>``. Rebuilding them needs nothing but
the pickle protocol, numpy and, for fairseq's ``cfg`` field, omegaconf.

Only what a conversion needs is supported: CPU tensors of the common dtypes and the objects a
fairseq checkpoint contains (Namespace, OrderedDict, omegaconf containers).
"""

from __future__ import annotations

import argparse
import collections
import pickle
import zipfile
from pathlib import Path
from typing import Any

import numpy as np

_DTYPES = {
    "FloatStorage": np.float32,
    "DoubleStorage": np.float64,
    "HalfStorage": np.float16,
    "BFloat16Storage": np.uint16,  # kept as raw bits; convert if you ever hit one
    "LongStorage": np.int64,
    "IntStorage": np.int32,
    "ShortStorage": np.int16,
    "CharStorage": np.int8,
    "ByteStorage": np.uint8,
    "BoolStorage": np.bool_,
}


class _StorageType:
    def __init__(self, name: str) -> None:
        self.name = name

    def __call__(self, *a, **k):  # torch.FloatStorage(...) is never called in practice
        return self


class _LazyStorage:
    def __init__(self, arr: np.ndarray) -> None:
        self.arr = arr


def _rebuild_tensor_v2(storage: _LazyStorage, offset: int, size, stride, *rest) -> np.ndarray:
    size = tuple(int(s) for s in size)
    stride = tuple(int(s) for s in stride)
    base = storage.arr
    if not size:
        return np.array(base[offset : offset + 1].reshape(()), copy=True)
    itemsize = base.dtype.itemsize
    view = np.lib.stride_tricks.as_strided(
        base[offset:], shape=size, strides=tuple(s * itemsize for s in stride), writeable=False
    )
    # Copy: the zip buffer is read-only and downstream code (e.g. quantization) writes in place.
    return np.array(view, copy=True, order="C")


def _rebuild_parameter(data, requires_grad, backward_hooks, *rest):
    return data


class _Unpickler(pickle.Unpickler):
    def __init__(self, file, zf: zipfile.ZipFile, prefix: str) -> None:
        super().__init__(file, encoding="utf-8")
        self._zf = zf
        self._prefix = prefix

    def find_class(self, module: str, name: str) -> Any:
        if module == "torch._utils" and name == "_rebuild_tensor_v2":
            return _rebuild_tensor_v2
        if module == "torch._utils" and name == "_rebuild_parameter":
            return _rebuild_parameter
        if module == "torch" and name.endswith("Storage"):
            return _StorageType(name)
        if module == "torch" and name == "Size":
            return tuple
        if module == "collections" and name == "OrderedDict":
            return collections.OrderedDict
        if module == "argparse" and name == "Namespace":
            return argparse.Namespace
        if module.startswith("omegaconf") or module.startswith("fairseq.dataclass") or module.startswith("fairseq"):
            # fairseq dataclass configs: build a plain Namespace-like shell so unpickling succeeds
            # even when fairseq itself is not installed.
            try:
                return super().find_class(module, name)
            except Exception:
                return _shell_class(module, name)
        return super().find_class(module, name)

    def persistent_load(self, pid):
        # ('storage', StorageType, key, location, numel)
        if isinstance(pid, tuple) and pid and pid[0] == "storage":
            _, stype, key, _location, numel = pid[:5]
            dtype = _DTYPES[stype.name]
            with self._zf.open(f"{self._prefix}/data/{key}") as f:
                buf = f.read()
            arr = np.frombuffer(buf, dtype=dtype, count=int(numel))
            return _LazyStorage(arr)
        raise pickle.UnpicklingError(f"unsupported persistent id: {pid!r}")


_SHELLS: dict[tuple[str, str], type] = {}


def _shell_class(module: str, name: str) -> type:
    key = (module, name)
    if key not in _SHELLS:
        cls = type(name, (), {"__module__": module})

        def __setstate__(self, state):
            if isinstance(state, dict):
                self.__dict__.update(state)
            else:
                self.__dict__["_state"] = state

        cls.__setstate__ = __setstate__  # type: ignore[attr-defined]
        _SHELLS[key] = cls
    return _SHELLS[key]


def load(path: str | Path) -> dict:
    path = Path(path)
    with zipfile.ZipFile(path) as zf:
        names = zf.namelist()
        pkl = next(n for n in names if n.endswith("/data.pkl"))
        prefix = pkl[: -len("/data.pkl")]
        with zf.open(pkl) as f:
            return _Unpickler(f, zf, prefix).load()


def to_plain(obj: Any) -> Any:
    """Recursively turn Namespace / omegaconf / shell objects into dicts for inspection."""
    if isinstance(obj, argparse.Namespace):
        return {k: to_plain(v) for k, v in vars(obj).items()}
    try:
        from omegaconf import DictConfig, ListConfig, OmegaConf

        if isinstance(obj, DictConfig | ListConfig):
            return OmegaConf.to_container(obj, resolve=True)
    except Exception:
        pass
    if isinstance(obj, dict):
        return {str(k): to_plain(v) for k, v in obj.items()}
    if isinstance(obj, list | tuple):
        return [to_plain(v) for v in obj]
    if isinstance(obj, np.ndarray):
        return f"<array {obj.shape} {obj.dtype}>"
    if hasattr(obj, "__dict__") and not isinstance(obj, type):
        return {k: to_plain(v) for k, v in vars(obj).items() if not k.startswith("_")}
    return obj
