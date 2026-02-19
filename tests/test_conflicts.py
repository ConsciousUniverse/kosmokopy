"""
Conflict-mode tests — Skip, Overwrite, Rename.

Operations are performed by invoking ``kosmokopy --cli``.
Verification is done in Python.
"""

import os
from pathlib import Path

import pytest

from conftest import (
    run_kosmokopy,
    requires_remote,
    sha256_of_file,
    sha256_remote,
    remote_file_exists,
    remote_ls,
    remote_read,
    SSH_CTL,
    _sq,
)


# ═══════════════════════════════════════════════════════════════════════
#  Local conflict: Skip
# ═══════════════════════════════════════════════════════════════════════


class TestConflictSkipLocal:

    def test_skip_preserves_existing(self, tmp_src, tmp_dst):
        """Existing different file at dest is not overwritten."""
        (tmp_dst / "hello.txt").write_text("DIFFERENT CONTENT\n")
        original_hash = sha256_of_file(tmp_dst / "hello.txt")

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="skip")
        assert result["status"] == "finished"
        # hello.txt was skipped; others copied
        assert len(result["skipped"]) >= 1
        assert sha256_of_file(tmp_dst / "hello.txt") == original_hash

    def test_skip_identical_deletes_source_on_move(self, tmp_src, tmp_dst):
        """Move mode + identical file: source deleted, dest untouched."""
        import shutil
        src_file = tmp_src / "hello.txt"
        shutil.copy2(str(src_file), str(tmp_dst / "hello.txt"))

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="skip", move=True)
        assert result["status"] == "finished"
        # Source deleted (identical at dest triggers delete-source)
        assert not src_file.exists()


# ═══════════════════════════════════════════════════════════════════════
#  Local conflict: Overwrite
# ═══════════════════════════════════════════════════════════════════════


class TestConflictOverwriteLocal:

    def test_overwrite_replaces_content(self, tmp_src, tmp_dst):
        (tmp_dst / "hello.txt").write_text("OLD CONTENT\n")

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="overwrite")
        assert result["status"] == "finished"
        assert result["errors"] == []

        # Content now matches source
        assert (tmp_dst / "hello.txt").read_text() == (tmp_src / "hello.txt").read_text()

    def test_overwrite_binary(self, tmp_src, tmp_dst):
        (tmp_dst / "data.bin").write_bytes(os.urandom(4096))

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="overwrite")
        assert result["status"] == "finished"

        assert sha256_of_file(tmp_src / "data.bin") == sha256_of_file(tmp_dst / "data.bin")


# ═══════════════════════════════════════════════════════════════════════
#  Local conflict: Rename
# ═══════════════════════════════════════════════════════════════════════


class TestConflictRenameLocal:

    def test_rename_creates_numbered_copy(self, tmp_src, tmp_dst):
        (tmp_dst / "hello.txt").write_text("EXISTING\n")

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="rename")
        assert result["status"] == "finished"
        assert result["errors"] == []

        # Original untouched
        assert (tmp_dst / "hello.txt").read_text() == "EXISTING\n"
        # Renamed copy created
        assert (tmp_dst / "hello (1).txt").exists()
        assert (tmp_dst / "hello (1).txt").read_text() == (tmp_src / "hello.txt").read_text()

    def test_rename_increments(self, tmp_src, tmp_dst):
        """Multiple pre-existing files increment the counter."""
        (tmp_dst / "hello.txt").write_text("original\n")
        (tmp_dst / "hello (1).txt").write_text("first\n")
        (tmp_dst / "hello (2).txt").write_text("second\n")

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="rename")
        assert result["status"] == "finished"

        assert (tmp_dst / "hello (3).txt").exists()
        assert (tmp_dst / "hello (3).txt").read_text() == (tmp_src / "hello.txt").read_text()

    def test_rename_preserves_extension(self, tmp_src, tmp_dst):
        (tmp_dst / "data.bin").write_bytes(b"different")

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="rename")
        assert result["status"] == "finished"

        assert (tmp_dst / "data (1).bin").exists()
        assert sha256_of_file(tmp_src / "data.bin") == sha256_of_file(tmp_dst / "data (1).bin")

    def test_rename_no_extension(self, tmp_path):
        """Rename works for files with no extension."""
        src = tmp_path / "src"
        src.mkdir()
        (src / "Makefile").write_text("new\n")

        dst = tmp_path / "dst"
        dst.mkdir()
        (dst / "Makefile").write_text("old\n")

        result = run_kosmokopy(src=src, dst=dst, conflict="rename")
        assert result["status"] == "finished"

        assert (dst / "Makefile").read_text() == "old\n"
        assert (dst / "Makefile (1)").exists()
        assert (dst / "Makefile (1)").read_text() == "new\n"

    def test_rename_move_mode(self, tmp_src, tmp_dst):
        """Rename + move: source deleted, renamed copy at dest."""
        (tmp_dst / "hello.txt").write_text("EXISTING\n")
        src_hello = tmp_src / "hello.txt"
        original_content = src_hello.read_text()

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, conflict="rename", move=True)
        assert result["status"] == "finished"

        assert not src_hello.exists()
        assert (tmp_dst / "hello.txt").read_text() == "EXISTING\n"
        assert (tmp_dst / "hello (1).txt").read_text() == original_content


# ═══════════════════════════════════════════════════════════════════════
#  Remote conflict modes (SCP)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestConflictSkipRemote:

    def test_skip_existing_remote_file(self, tmp_src, remote_dest):
        host, rdir = remote_dest

        # First upload
        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"
        assert result["copied"] == 6

        # Second upload with skip — all should be skipped
        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir), conflict="skip")
        assert result["status"] == "finished"
        assert len(result["skipped"]) == 6
        assert result["copied"] == 0


@requires_remote
class TestConflictOverwriteRemote:

    def test_overwrite_replaces_remote(self, tmp_path, remote_dest):
        host, rdir = remote_dest

        # Upload initial content
        src1 = tmp_path / "src1"
        src1.mkdir()
        (src1 / "file.txt").write_text("OLD\n")
        run_kosmokopy(src=src1, dst="{}:{}".format(host, rdir))

        # Upload different content with overwrite
        src2 = tmp_path / "src2"
        src2.mkdir()
        f = src2 / "file.txt"
        f.write_text("NEW\n")
        result = run_kosmokopy(
            src=src2, dst="{}:{}".format(host, rdir), conflict="overwrite",
        )
        assert result["status"] == "finished"
        assert result["copied"] == 1

        assert sha256_remote(host, rdir + "/file.txt") == sha256_of_file(f)


@requires_remote
class TestConflictRenameRemote:

    def test_rename_remote_file(self, tmp_src, remote_dest):
        host, rdir = remote_dest

        # First upload
        run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))

        # Second upload with rename
        result = run_kosmokopy(
            src=tmp_src, dst="{}:{}".format(host, rdir), conflict="rename",
        )
        assert result["status"] == "finished"
        assert result["copied"] == 6
        assert result["errors"] == []

        # Both original and renamed files should exist
        files = remote_ls(host, rdir)
        names = {Path(f).name for f in files}
        assert "hello.txt" in names
        assert "hello (1).txt" in names
