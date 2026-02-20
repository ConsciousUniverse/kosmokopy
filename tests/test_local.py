"""
Local transfer tests — standard (cp) and rsync.

Operations are performed by invoking ``kosmokopy --cli``.
Verification is done in Python.
"""

import os
from pathlib import Path

import pytest

from conftest import (
    run_kosmokopy,
    requires_rsync,
    sha256_of_file,
    files_are_identical,
)


# ═══════════════════════════════════════════════════════════════════════
#  Standard local copy
# ═══════════════════════════════════════════════════════════════════════


class TestLocalCopyStandard:

    def test_copy_flat_files_only(self, tmp_src, tmp_dst):
        """FilesOnly mode: all files land flat in destination."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, mode="files")
        assert result["status"] == "finished"
        assert result["copied"] == 6
        assert result["errors"] == []

        # All files should be flat in dst (no subdirs)
        dst_files = list(tmp_dst.iterdir())
        assert all(f.is_file() for f in dst_files)
        assert len(dst_files) == 6

    def test_copy_preserve_structure(self, tmp_src, tmp_dst):
        """FoldersAndFiles mode: directory structure is preserved."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, mode="folders")
        assert result["status"] == "finished"
        assert result["copied"] == 6

        root = tmp_dst / tmp_src.name  # root folder preserved
        assert root.exists()
        assert (root / "hello.txt").exists()
        assert (root / "subdir" / "nested.txt").exists()
        assert (root / "subdir" / "level2" / "bottom.txt").exists()

        # Verify content integrity
        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                dst_f = root / rel
                assert dst_f.exists()
                assert files_are_identical(f, dst_f)

    def test_copy_verifies_integrity(self, tmp_src, tmp_dst):
        """After copy, SHA-256 hashes match."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst)
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert sha256_of_file(f) == sha256_of_file(root / rel)

    def test_copy_creates_destination_dir(self, tmp_src, tmp_path):
        """Destination directory is created if it doesn't exist."""
        dst = tmp_path / "nonexistent" / "deep" / "dest"
        assert not dst.exists()

        result = run_kosmokopy(src=tmp_src, dst=dst)
        assert result["status"] == "finished"
        assert result["copied"] == 6
        assert dst.exists()

    def test_copy_individual_files(self, tmp_src, tmp_dst):
        """Copy specific files using --src-files."""
        files = [
            tmp_src / "hello.txt",
            tmp_src / "data.bin",
        ]
        result = run_kosmokopy(src_files=files, dst=tmp_dst, mode="files")
        assert result["status"] == "finished"
        assert result["copied"] == 2
        assert (tmp_dst / "hello.txt").exists()
        assert (tmp_dst / "data.bin").exists()

    def test_root_folder_preserved(self, tmp_path):
        """Regression: the source root folder name must appear in destination.

        Previously, only the *contents* of the source directory were copied
        directly into the destination, losing the top-level folder name.
        E.g. ``/src/myFolder/sub/file.txt`` ended up as ``dst/sub/file.txt``
        instead of ``dst/myFolder/sub/file.txt``.
        """
        src = tmp_path / "MyRootFolder"
        src.mkdir()
        (src / "a.txt").write_text("aaa\n")
        sub = src / "child"
        sub.mkdir()
        (sub / "b.txt").write_text("bbb\n")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, mode="folders")
        assert result["status"] == "finished"
        assert result["copied"] == 2
        assert result["errors"] == []

        # Root folder "MyRootFolder" must exist inside dst
        root = dst / "MyRootFolder"
        assert root.is_dir(), "Root folder not preserved in destination"
        assert (root / "a.txt").exists()
        assert (root / "a.txt").read_text() == "aaa\n"
        assert (root / "child" / "b.txt").exists()
        assert (root / "child" / "b.txt").read_text() == "bbb\n"

        # Files must NOT appear directly in dst (the old buggy behavior)
        assert not (dst / "a.txt").exists(), "File landed flat — root folder lost"
        assert not (dst / "child").exists(), "Subdir landed flat — root folder lost"


# ═══════════════════════════════════════════════════════════════════════
#  Standard local move
# ═══════════════════════════════════════════════════════════════════════


class TestLocalMoveStandard:

    def test_move_removes_source(self, tmp_src, tmp_dst):
        """After move, source files no longer exist."""
        src_file = tmp_src / "hello.txt"
        original_content = src_file.read_text()

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, move=True)
        assert result["status"] == "finished"
        assert result["copied"] == 6

        root = tmp_dst / tmp_src.name
        # Source files gone
        assert not src_file.exists()
        # Dest has the content
        assert (root / "hello.txt").read_text() == original_content

    def test_move_preserves_structure(self, tmp_src, tmp_dst):
        """Move with FoldersAndFiles preserves directory layout."""
        # Record original filenames
        originals = {f.relative_to(tmp_src) for f in tmp_src.rglob("*") if f.is_file()}

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, move=True)
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        # All originals should be in dst, not in src
        for rel in originals:
            assert (root / rel).exists()
            assert not (tmp_src / rel).exists()


# ═══════════════════════════════════════════════════════════════════════
#  Rsync local transfers
# ═══════════════════════════════════════════════════════════════════════


@requires_rsync
class TestLocalCopyRsync:

    def test_rsync_copy_preserve_structure(self, tmp_src, tmp_dst):
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, method="rsync")
        assert result["status"] == "finished"
        assert result["copied"] == 6

        root = tmp_dst / tmp_src.name
        assert (root / "subdir" / "nested.txt").exists()
        assert (root / "subdir" / "level2" / "bottom.txt").exists()

    def test_rsync_checksum_verification(self, tmp_src, tmp_dst):
        """rsync transfers match SHA-256 hashes."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, method="rsync")
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert sha256_of_file(f) == sha256_of_file(root / rel)

    def test_rsync_flat_mode(self, tmp_src, tmp_dst):
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, method="rsync", mode="files")
        assert result["status"] == "finished"
        assert result["copied"] == 6
        # All files flat
        assert all(f.is_file() for f in tmp_dst.iterdir())


@requires_rsync
class TestLocalMoveRsync:

    def test_rsync_move(self, tmp_src, tmp_dst):
        originals = {f.relative_to(tmp_src): sha256_of_file(f)
                     for f in tmp_src.rglob("*") if f.is_file()}

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, method="rsync", move=True)
        assert result["status"] == "finished"
        assert result["copied"] == 6

        root = tmp_dst / tmp_src.name
        for rel, h in originals.items():
            assert not (tmp_src / rel).exists()
            assert sha256_of_file(root / rel) == h


# ═══════════════════════════════════════════════════════════════════════
#  Strip spaces from filenames
# ═══════════════════════════════════════════════════════════════════════


class TestStripSpaces:

    def test_strip_spaces_preserves_content(self, tmp_src_with_spaces, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_spaces, dst=tmp_dst, strip_spaces=True,
        )
        assert result["status"] == "finished"
        assert result["copied"] == 3

        # Spaces removed from all path components
        for f in tmp_dst.rglob("*"):
            if f.is_file():
                for part in f.relative_to(tmp_dst).parts:
                    assert " " not in part

    def test_strip_spaces_flat(self, tmp_src_with_spaces, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_spaces, dst=tmp_dst,
            strip_spaces=True, mode="files",
        )
        assert result["status"] == "finished"
        for f in tmp_dst.iterdir():
            assert " " not in f.name

    def test_no_strip_preserves_spaces(self, tmp_src_with_spaces, tmp_dst):
        """When strip_spaces is off, spaces are preserved."""
        result = run_kosmokopy(
            src=tmp_src_with_spaces, dst=tmp_dst, strip_spaces=False,
        )
        assert result["status"] == "finished"
        root = tmp_dst / tmp_src_with_spaces.name
        assert (root / "my file.txt").exists()


# ═══════════════════════════════════════════════════════════════════════
#  Single-file source
# ═══════════════════════════════════════════════════════════════════════


class TestSingleFileCopy:
    """Copy or move a single file (not a directory)."""

    def test_single_file_standard(self, tmp_path):
        """Copy a single file by passing it as --src-files."""
        src = tmp_path / "src"
        src.mkdir()
        f = src / "only.txt"
        f.write_text("only file\n")
        dst = tmp_path / "dst"

        result = run_kosmokopy(src_files=[f], dst=dst, mode="files")
        assert result["status"] == "finished"
        assert result["copied"] == 1
        assert (dst / "only.txt").exists()
        assert files_are_identical(f, dst / "only.txt")

    def test_single_file_rsync(self, tmp_path):
        """Copy a single file via rsync."""
        src = tmp_path / "src"
        src.mkdir()
        f = src / "only.bin"
        f.write_bytes(os.urandom(1024))
        dst = tmp_path / "dst"

        result = run_kosmokopy(src_files=[f], dst=dst, mode="files", method="rsync")
        assert result["status"] == "finished"
        assert result["copied"] == 1
        assert files_are_identical(f, dst / "only.bin")

    def test_single_file_move(self, tmp_path):
        """Move a single file — source should be deleted."""
        src = tmp_path / "src"
        src.mkdir()
        f = src / "moveme.txt"
        f.write_text("move me\n")
        expected_hash = sha256_of_file(f)
        dst = tmp_path / "dst"

        result = run_kosmokopy(src_files=[f], dst=dst, mode="files", move=True)
        assert result["status"] == "finished"
        assert result["copied"] == 1
        assert not f.exists()
        assert sha256_of_file(dst / "moveme.txt") == expected_hash

    def test_single_file_directory_source(self, tmp_path):
        """A directory containing only one file copies correctly."""
        src = tmp_path / "src"
        src.mkdir()
        (src / "solo.txt").write_text("solo\n")
        dst = tmp_path / "dst"

        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"
        assert result["copied"] == 1
        assert (dst / "src" / "solo.txt").exists()
