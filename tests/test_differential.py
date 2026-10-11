"""Recompiled code vs. the Musashi reference emulator (see tests/difftest/)."""

import shutil
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent / "difftest"))


@pytest.mark.parametrize("seed", [1, 2, 3])
def test_random_snippets_match_musashi(seed):
    pytest.importorskip("machine68k")
    if not shutil.which("cc"):
        pytest.skip("no C compiler")
    import difftest
    failures = difftest.run(ncases=800, seed=seed, length=4, verbose=False)
    assert not failures, "\n".join(failures[:10])
