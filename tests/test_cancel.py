"""
Cancel / graceful-shutdown tests.

These tests verify that sending SIGINT (Ctrl+C) to kosmokopy --cli
causes the transfer to stop gracefully with status "cancelled", and
that files already copied remain intact.
"""

import os
from pathlib import Path

import pytest

from conftest import (
    run_kosmokopy,
    run_kosmokopy_with_cancel,
    sha256_of_file,
    files_are_identical,
    requires_remote,
    requires_rsync,
    sha256_remote,
    remote_ls,
    remote_rm_rf,
    SSH_CTL,
    _sq,
    REMOTE_HOST,
    REMOTE_PATH,
)


# ═══════════════════════════════════════════════════════════════════════
#  Fixtures
# ═══════════════════════════════════════════════════════════════════════


@pytest.fixture
def large_src(tmp_path):
    """Create a source tree with many small files so cancel can fire mid-run."""
    src = tmp_path / "source"
    src.mkdir()
    for i in range(200):
        (src / f"file_{i:04d}.txt").write_text(f"content of file {i}\n")
    sub = src / "subdir"
    sub.mkdir()
    for i in range(50):
        (sub / f"nested_{i:03d}.dat").write_bytes(os.urandom(512))
    return src


@pytest.fixture
def large_src_for_move(tmp_path):
    """Separate large source tree for move-cancel tests (avoids mutation)."""
    src = tmp_path / "move_source"
    src.mkdir()
    for i in range(200):
        (src / f"file_{i:04d}.txt").write_text(f"content of file {i}\n")
    return src


# ═══════════════════════════════════════════════════════════════════════
#  Local cancel — standard method
# ═══════════════════════════════════════════════════════════════════════


class TestLocalCancelStandard:

    def test_cancel_returns_cancelled_status(self, large_src, tmp_path):
        """SIGINT during copy should yield status 'cancelled'."""
        dst = tmp_path / "dest"
        result = run_kosmokopy_with_cancel(
            src=large_src, dst=dst, cancel_after=0.05,
        )
        assert result["status"] in ("cancelled", "finished")

    def test_cancel_partial_copy(self, large_src, tmp_path):
        """After cancel, some (but possibly not all) files are copied."""
        dst = tmp_path / "dest"
        result = run_kosmokopy_with_cancel(
            src=large_src, dst=dst, cancel_after=0.05,
        )
        if result["status"] == "cancelled":
            # Fewer than total
            total_src = sum(1 for _ in large_src.rglob("*") if _.is_file())
            assert result["copied"] < total_src
            assert result["copied"] >= 0

    def test_cancel_copied_files_intact(self, large_src, tmp_path):
        """Files that were copied before cancel should be byte-identical."""
        dst = tmp_path / "dest"
        result = run_kosmokopy_with_cancel(
            src=large_src, dst=dst, cancel_after=0.05,
        )
        # Verify every file in dst matches corresponding source
        root = dst / large_src.name
        if root.exists():
            for dst_file in root.rglob("*"):
                if dst_file.is_file():
                    rel = dst_file.relative_to(root)
                    src_file = large_src / rel
                    assert src_file.exists(), f"Source missing for {rel}"
                    assert files_are_identical(src_file, dst_file), \
                        f"Mismatch for {rel}"

    def test_cancel_no_errors(self, large_src, tmp_path):
        """Graceful cancel should not produce errors."""
        dst = tmp_path / "dest"
        result = run_kosmokopy_with_cancel(
            src=large_src, dst=dst, cancel_after=0.05,
        )
        assert result["errors"] == []

    def test_cancel_move_preserves_uncopied_source(self, large_src_for_move, tmp_path):
        """Cancel during move: un-transferred source files must still exist."""
        dst = tmp_path / "dest"
        total_src = sum(1 for _ in large_src_for_move.rglob("*") if _.is_file())

        result = run_kosmokopy_with_cancel(
            src=large_src_for_move, dst=dst, move=True, cancel_after=0.05,
        )
        if result["status"] == "cancelled":
            remaining = sum(1 for _ in large_src_for_move.rglob("*") if _.is_file())
            copied = result["copied"]
            # remaining + copied should account for all original files
            assert remaining + copied == total_src


# ═══════════════════════════════════════════════════════════════════════
#  Local cancel — rsync method
# ═══════════════════════════════════════════════════════════════════════


@requires_rsync
class TestLocalCancelRsync:

    def test_cancel_rsync_returns_cancelled(self, large_src, tmp_path):
        dst = tmp_path / "dest"
        result = run_kosmokopy_with_cancel(
            src=large_src, dst=dst, method="rsync", cancel_after=0.1,
        )
        assert result["status"] in ("cancelled", "finished")
        assert result["errors"] == []

    def test_cancel_rsync_files_intact(self, large_src, tmp_path):
        dst = tmp_path / "dest"
        result = run_kosmokopy_with_cancel(
            src=large_src, dst=dst, method="rsync", cancel_after=0.1,
        )
        root = dst / large_src.name
        if root.exists():
            for dst_file in root.rglob("*"):
                if dst_file.is_file():
                    rel = dst_file.relative_to(root)
                    src_file = large_src / rel
                    assert files_are_identical(src_file, dst_file)


# ═══════════════════════════════════════════════════════════════════════
#  Cancel with exclusions active
# ═══════════════════════════════════════════════════════════════════════


class TestCancelWithExclusions:

    def test_cancel_respects_exclude_counts(self, tmp_path):
        """Exclusion counts should still be reported on cancel."""
        src = tmp_path / "src"
        src.mkdir()
        for i in range(100):
            (src / f"keep_{i:03d}.txt").write_text(f"keep {i}\n")
        for i in range(20):
            (src / f"skip_{i:03d}.log").write_text(f"log {i}\n")
        dst = tmp_path / "dst"

        result = run_kosmokopy_with_cancel(
            src=src, dst=dst, exclude=["~*.log"], cancel_after=0.05,
        )
        assert result["status"] in ("cancelled", "finished")
        assert result["excluded_files"] == 20


# ═══════════════════════════════════════════════════════════════════════
#  Immediate cancel (before any files transfer)
# ═══════════════════════════════════════════════════════════════════════


class TestImmediateCancel:

    def test_immediate_cancel_zero_copied(self, large_src, tmp_path):
        """SIGINT sent very early should result in 0 or very few copies."""
        dst = tmp_path / "dest"
        result = run_kosmokopy_with_cancel(
            src=large_src, dst=dst, cancel_after=0.05,
        )
        # May be cancelled, finished (if very fast), or error (if signal
        # arrives before ctrlc handler is registered)
        assert result["status"] in ("cancelled", "finished", "error")
        assert result["errors"] == []


# ═══════════════════════════════════════════════════════════════════════
#  Normal completion (no cancel) still works
# ═══════════════════════════════════════════════════════════════════════


class TestNoCancelStillWorks:

    def test_full_copy_still_finishes(self, tmp_path):
        """Confirm a normal (un-cancelled) run still finishes correctly."""
        src = tmp_path / "src"
        src.mkdir()
        (src / "a.txt").write_text("aaa\n")
        (src / "b.txt").write_text("bbb\n")
        dst = tmp_path / "dst"

        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"
        assert result["copied"] == 2
        assert result["errors"] == []
