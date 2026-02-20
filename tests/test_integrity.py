"""
Integrity and verification tests.

Operations are performed by invoking ``kosmokopy --cli``.
Verification is done in Python — checking file identity, SHA-256
hashes, and correct error handling.

Includes *negative* (corruption) tests that deliberately tamper with a
copied file and confirm that our verification helpers detect the change.
"""

import hashlib
import os
import subprocess
from pathlib import Path

import pytest

from conftest import (
    run_kosmokopy,
    requires_remote,
    requires_rsync,
    sha256_of_file,
    sha256_remote,
    remote_file_exists,
    remote_rm_rf,
    files_are_identical,
    SSH_CTL,
    _sq,
)


# ═══════════════════════════════════════════════════════════════════════
#  Local copy integrity — standard
# ═══════════════════════════════════════════════════════════════════════


class TestLocalCopyIntegrity:

    def test_all_files_identical_after_copy(self, tmp_src, tmp_dst):
        """Every copied file must be byte-identical to the source."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst)
        assert result["status"] == "finished"
        assert result["errors"] == []

        root = tmp_dst / tmp_src.name
        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert files_are_identical(f, root / rel)

    def test_binary_file_integrity(self, tmp_path):
        """Large binary file copied through the app stays intact."""
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(100_000)
        (src / "big.bin").write_bytes(data)
        expected = hashlib.sha256(data).hexdigest()

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        assert sha256_of_file(dst / "src" / "big.bin") == expected

    def test_empty_file_integrity(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        (src / "empty").write_bytes(b"")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        assert (dst / "src" / "empty").exists()
        assert (dst / "src" / "empty").stat().st_size == 0
        assert sha256_of_file(dst / "src" / "empty") == hashlib.sha256(b"").hexdigest()


# ═══════════════════════════════════════════════════════════════════════
#  Local copy integrity — rsync
# ═══════════════════════════════════════════════════════════════════════


@requires_rsync
class TestLocalRsyncIntegrity:

    def test_rsync_files_identical(self, tmp_src, tmp_dst):
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, method="rsync")
        assert result["status"] == "finished"
        assert result["errors"] == []

        root = tmp_dst / tmp_src.name
        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert files_are_identical(f, root / rel)

    def test_rsync_large_binary(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(100_000)
        (src / "big.bin").write_bytes(data)
        expected = hashlib.sha256(data).hexdigest()

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, method="rsync")
        assert result["status"] == "finished"

        assert sha256_of_file(dst / "src" / "big.bin") == expected


# ═══════════════════════════════════════════════════════════════════════
#  Move-mode integrity — source removed only when dest verified
# ═══════════════════════════════════════════════════════════════════════


class TestMoveIntegrity:

    def test_move_deletes_source_after_copy(self, tmp_path):
        """After a successful move, source files are gone and dest is intact."""
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(8192)
        (src / "move_me.bin").write_bytes(data)
        expected = hashlib.sha256(data).hexdigest()

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, move=True)
        assert result["status"] == "finished"
        assert result["copied"] == 1

        assert not (src / "move_me.bin").exists()
        assert sha256_of_file(dst / "src" / "move_me.bin") == expected

    def test_move_multiple_files(self, tmp_src, tmp_dst):
        originals = {}
        for f in tmp_src.rglob("*"):
            if f.is_file():
                originals[f.relative_to(tmp_src)] = sha256_of_file(f)

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, move=True)
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        for rel, h in originals.items():
            assert not (tmp_src / rel).exists(), "Source not removed: {}".format(rel)
            assert sha256_of_file(root / rel) == h

    @requires_rsync
    def test_rsync_move(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(8192)
        (src / "rsync_move.bin").write_bytes(data)
        expected = hashlib.sha256(data).hexdigest()

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, move=True, method="rsync")
        assert result["status"] == "finished"

        assert not (src / "rsync_move.bin").exists()
        assert sha256_of_file(dst / "src" / "rsync_move.bin") == expected


# ═══════════════════════════════════════════════════════════════════════
#  Remote upload + SHA-256 verification
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteUploadIntegrity:

    def test_upload_hash_match(self, tmp_src, remote_dest):
        """After upload, remote SHA-256 matches local."""
        host, rdir = remote_dest
        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"
        assert result["errors"] == []

        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                remote_path = "{}/{}/{}".format(rdir, tmp_src.name, rel)
                assert sha256_of_file(f) == sha256_remote(host, remote_path)

    def test_upload_large_binary(self, tmp_path, remote_dest):
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(50_000)
        (src / "big.bin").write_bytes(data)
        expected = hashlib.sha256(data).hexdigest()

        result = run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"
        assert sha256_remote(host, rdir + "/src/big.bin") == expected


# ═══════════════════════════════════════════════════════════════════════
#  Remote move — source not deleted until hash verified
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteMoveIntegrity:

    def test_move_upload_deletes_source(self, tmp_path, remote_dest):
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        f = src / "to_move.bin"
        f.write_bytes(os.urandom(4096))
        expected = sha256_of_file(f)

        result = run_kosmokopy(
            src=src, dst="{}:{}".format(host, rdir), move=True,
        )
        assert result["status"] == "finished"
        assert result["copied"] == 1

        assert not f.exists()
        assert sha256_remote(host, rdir + "/src/to_move.bin") == expected


# ═══════════════════════════════════════════════════════════════════════
#  Remote download + integrity
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteDownloadIntegrity:

    def test_download_hash_match(self, tmp_path, remote_src):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(src="{}:{}".format(host, rdir), dst=dst)
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1

        root_name = Path(rdir).name
        root = dst / root_name
        for f in root.rglob("*"):
            if f.is_file():
                rel = f.relative_to(root)
                remote_path = "{}/{}".format(rdir, rel)
                assert sha256_of_file(f) == sha256_remote(host, remote_path)

    def test_download_move_deletes_remote(self, tmp_path, remote_src):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(
            src="{}:{}".format(host, rdir), dst=dst, move=True,
        )
        assert result["status"] == "finished"
        assert result["copied"] >= 1

        # Remote files should be gone
        remaining = subprocess.run(
            ["ssh"] + SSH_CTL + [host, "find {} -type f".format(_sq(rdir))],
            capture_output=True, text=True,
        )
        assert remaining.stdout.strip() == ""


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — corruption detection (local, standard copy)
# ═══════════════════════════════════════════════════════════════════════


class TestCorruptionDetectionLocal:
    """
    Copy files with kosmokopy, then deliberately corrupt the destination
    and verify that our integrity helpers (files_are_identical, sha256_of_file)
    catch the corruption.
    """

    def test_single_byte_flip(self, tmp_path):
        """Flipping one byte in the copied file is detected."""
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(8192)
        (src / "file.bin").write_bytes(data)
        original_hash = sha256_of_file(src / "file.bin")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        copied = dst / "src" / "file.bin"
        # Sanity: file is intact before corruption
        assert sha256_of_file(copied) == original_hash
        assert files_are_identical(src / "file.bin", copied)

        # Corrupt: flip one byte
        corrupted = bytearray(copied.read_bytes())
        corrupted[len(corrupted) // 2] ^= 0xFF
        copied.write_bytes(bytes(corrupted))

        assert sha256_of_file(copied) != original_hash
        assert not files_are_identical(src / "file.bin", copied)

    def test_appended_byte(self, tmp_path):
        """Appending a single byte is detected."""
        src = tmp_path / "src"
        src.mkdir()
        data = b"Hello, World!"
        (src / "msg.txt").write_bytes(data)
        original_hash = sha256_of_file(src / "msg.txt")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        copied = dst / "src" / "msg.txt"
        assert sha256_of_file(copied) == original_hash

        # Corrupt: append one byte
        with open(copied, "ab") as f:
            f.write(b"\x00")

        assert sha256_of_file(copied) != original_hash
        assert not files_are_identical(src / "msg.txt", copied)

    def test_truncated_file(self, tmp_path):
        """Truncating a copied file is detected."""
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(10_000)
        (src / "big.bin").write_bytes(data)
        original_hash = sha256_of_file(src / "big.bin")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        copied = dst / "src" / "big.bin"
        assert sha256_of_file(copied) == original_hash

        # Corrupt: truncate to half
        copied.write_bytes(data[:5000])

        assert sha256_of_file(copied) != original_hash
        assert not files_are_identical(src / "big.bin", copied)

    def test_replaced_with_different_content(self, tmp_path):
        """Replacing file contents entirely is detected."""
        src = tmp_path / "src"
        src.mkdir()
        (src / "doc.txt").write_text("Original document content\n")
        original_hash = sha256_of_file(src / "doc.txt")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        copied = dst / "src" / "doc.txt"
        assert sha256_of_file(copied) == original_hash

        # Corrupt: replace with different content of same length
        copied.write_text("Replaced document content\n")

        assert sha256_of_file(copied) != original_hash
        assert not files_are_identical(src / "doc.txt", copied)

    def test_deleted_file(self, tmp_src, tmp_dst):
        """Deleting a copied file is detected during scan."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst)
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        # Verify all files exist first
        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert (root / rel).exists()

        # Delete one file
        (root / "hello.txt").unlink()
        assert not (root / "hello.txt").exists()

        # Remaining files still match
        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                if rel != Path("hello.txt"):
                    assert files_are_identical(f, root / rel)

    def test_empty_file_replaced_with_content(self, tmp_path):
        """Writing data to a previously empty file is detected."""
        src = tmp_path / "src"
        src.mkdir()
        (src / "empty").write_bytes(b"")
        empty_hash = sha256_of_file(src / "empty")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        copied = dst / "src" / "empty"
        assert sha256_of_file(copied) == empty_hash

        # Corrupt: put data in the empty file
        copied.write_bytes(b"no longer empty")

        assert sha256_of_file(copied) != empty_hash
        assert not files_are_identical(src / "empty", copied)

    def test_nonempty_file_replaced_with_empty(self, tmp_path):
        """Truncating a file to zero bytes is detected."""
        src = tmp_path / "src"
        src.mkdir()
        (src / "data.bin").write_bytes(os.urandom(4096))
        original_hash = sha256_of_file(src / "data.bin")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        copied = dst / "src" / "data.bin"
        assert sha256_of_file(copied) == original_hash

        # Corrupt: truncate to zero
        copied.write_bytes(b"")

        assert sha256_of_file(copied) != original_hash
        assert not files_are_identical(src / "data.bin", copied)

    def test_corruption_in_nested_file(self, tmp_src, tmp_dst):
        """Corruption in a nested subdirectory file is detected."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst)
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        nested = root / "subdir" / "nested.txt"
        assert nested.exists()
        original_hash = sha256_of_file(tmp_src / "subdir" / "nested.txt")
        assert sha256_of_file(nested) == original_hash

        # Corrupt the nested file
        nested.write_text("CORRUPTED\n")

        assert sha256_of_file(nested) != original_hash
        assert not files_are_identical(
            tmp_src / "subdir" / "nested.txt", nested,
        )

    def test_corruption_in_deeply_nested_file(self, tmp_src, tmp_dst):
        """Corruption two levels deep is detected."""
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst)
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        deep = root / "subdir" / "level2" / "bottom.txt"
        src_deep = tmp_src / "subdir" / "level2" / "bottom.txt"
        assert files_are_identical(src_deep, deep)

        deep.write_text("TAMPERED\n")

        assert not files_are_identical(src_deep, deep)
        assert sha256_of_file(src_deep) != sha256_of_file(deep)


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — corruption detection (local, rsync)
# ═══════════════════════════════════════════════════════════════════════


@requires_rsync
class TestCorruptionDetectionRsync:
    """Verify corruption is detected after rsync-mode copies."""

    def test_rsync_single_byte_flip(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(50_000)
        (src / "big.bin").write_bytes(data)
        original_hash = sha256_of_file(src / "big.bin")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, method="rsync")
        assert result["status"] == "finished"

        copied = dst / "src" / "big.bin"
        assert sha256_of_file(copied) == original_hash

        corrupted = bytearray(copied.read_bytes())
        corrupted[0] ^= 0x01
        copied.write_bytes(bytes(corrupted))

        assert sha256_of_file(copied) != original_hash
        assert not files_are_identical(src / "big.bin", copied)

    def test_rsync_truncated_file(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(20_000)
        (src / "file.bin").write_bytes(data)
        original_hash = sha256_of_file(src / "file.bin")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, method="rsync")
        assert result["status"] == "finished"

        copied = dst / "src" / "file.bin"
        assert sha256_of_file(copied) == original_hash

        copied.write_bytes(data[:100])

        assert sha256_of_file(copied) != original_hash
        assert not files_are_identical(src / "file.bin", copied)

    def test_rsync_file_replaced(self, tmp_src, tmp_dst):
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, method="rsync")
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        original_hash = sha256_of_file(tmp_src / "hello.txt")
        assert sha256_of_file(root / "hello.txt") == original_hash

        (root / "hello.txt").write_text("COMPLETELY DIFFERENT\n")

        assert sha256_of_file(root / "hello.txt") != original_hash
        assert not files_are_identical(
            tmp_src / "hello.txt", root / "hello.txt",
        )


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — corruption detection (local, move)
# ═══════════════════════════════════════════════════════════════════════


class TestCorruptionDetectionMove:
    """
    Move files via kosmokopy, record hashes before the move, then
    corrupt the destination and verify the hash mismatch is caught.
    """

    def test_move_then_corrupt(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(8192)
        (src / "important.bin").write_bytes(data)
        original_hash = hashlib.sha256(data).hexdigest()

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, move=True)
        assert result["status"] == "finished"
        assert not (src / "important.bin").exists()

        copied = dst / "src" / "important.bin"
        assert sha256_of_file(copied) == original_hash

        # Corrupt the moved file
        corrupted = bytearray(copied.read_bytes())
        corrupted[-1] ^= 0xFF
        copied.write_bytes(bytes(corrupted))

        assert sha256_of_file(copied) != original_hash

    def test_move_multiple_then_corrupt_one(self, tmp_src, tmp_dst):
        """Corrupting one file among many is pinpointed."""
        originals = {}
        for f in tmp_src.rglob("*"):
            if f.is_file():
                originals[f.relative_to(tmp_src)] = sha256_of_file(f)

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, move=True)
        assert result["status"] == "finished"

        root = tmp_dst / tmp_src.name
        # Verify all intact
        for rel, h in originals.items():
            assert sha256_of_file(root / rel) == h

        # Corrupt just one
        target = root / "hello.txt"
        target.write_text("CORRUPTED\n")

        corrupted_count = 0
        intact_count = 0
        for rel, h in originals.items():
            if sha256_of_file(root / rel) != h:
                corrupted_count += 1
            else:
                intact_count += 1

        assert corrupted_count == 1, "Exactly one file should be corrupted"
        assert intact_count == len(originals) - 1


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — corruption detection (flat / files-only mode)
# ═══════════════════════════════════════════════════════════════════════


class TestCorruptionDetectionFlat:
    """Corruption detection works when files are copied flat (no subdirs)."""

    def test_flat_copy_then_corrupt(self, tmp_src, tmp_dst):
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, mode="files")
        assert result["status"] == "finished"
        assert result["copied"] == 6

        # Collect hashes of all flat destination files
        dst_hashes = {
            f.name: sha256_of_file(f)
            for f in tmp_dst.iterdir() if f.is_file()
        }

        # Corrupt one
        target = tmp_dst / "hello.txt"
        target.write_text("FLAT CORRUPTION\n")

        assert sha256_of_file(target) != dst_hashes["hello.txt"]

        # All others still match
        for f in tmp_dst.iterdir():
            if f.is_file() and f.name != "hello.txt":
                assert sha256_of_file(f) == dst_hashes[f.name]


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — corruption detection (strip-spaces mode)
# ═══════════════════════════════════════════════════════════════════════


class TestCorruptionDetectionStripSpaces:
    """Corruption detection works when strip-spaces renames files."""

    def test_strip_spaces_then_corrupt(self, tmp_src_with_spaces, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_spaces, dst=tmp_dst, strip_spaces=True,
        )
        assert result["status"] == "finished"

        # Collect hashes of all destination files
        dst_hashes = {}
        for f in tmp_dst.rglob("*"):
            if f.is_file():
                dst_hashes[f.relative_to(tmp_dst)] = sha256_of_file(f)

        assert len(dst_hashes) >= 1

        # Corrupt the first file found
        target_rel = next(iter(dst_hashes))
        target = tmp_dst / target_rel
        target.write_text("SPACE CORRUPTION\n")

        assert sha256_of_file(target) != dst_hashes[target_rel]


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — hash helper self-test
# ═══════════════════════════════════════════════════════════════════════


class TestHashHelperSelfTest:
    """
    Ensure our own test helpers (sha256_of_file, files_are_identical)
    behave correctly — these are the foundation of all integrity tests.
    """

    def test_identical_files_match(self, tmp_path):
        data = os.urandom(4096)
        (tmp_path / "a").write_bytes(data)
        (tmp_path / "b").write_bytes(data)

        assert sha256_of_file(tmp_path / "a") == sha256_of_file(tmp_path / "b")
        assert files_are_identical(tmp_path / "a", tmp_path / "b")

    def test_different_files_mismatch(self, tmp_path):
        (tmp_path / "a").write_bytes(os.urandom(4096))
        (tmp_path / "b").write_bytes(os.urandom(4096))

        assert sha256_of_file(tmp_path / "a") != sha256_of_file(tmp_path / "b")
        assert not files_are_identical(tmp_path / "a", tmp_path / "b")

    def test_same_size_different_content(self, tmp_path):
        """Two files with identical size but different content are caught."""
        (tmp_path / "a").write_bytes(b"\x00" * 1024)
        (tmp_path / "b").write_bytes(b"\xFF" * 1024)

        assert (tmp_path / "a").stat().st_size == (tmp_path / "b").stat().st_size
        assert sha256_of_file(tmp_path / "a") != sha256_of_file(tmp_path / "b")
        assert not files_are_identical(tmp_path / "a", tmp_path / "b")

    def test_one_byte_difference(self, tmp_path):
        """Files differing by exactly one byte are detected."""
        data = b"\x00" * 4096
        (tmp_path / "a").write_bytes(data)
        corrupted = bytearray(data)
        corrupted[2048] = 0x01
        (tmp_path / "b").write_bytes(bytes(corrupted))

        assert not files_are_identical(tmp_path / "a", tmp_path / "b")
        assert sha256_of_file(tmp_path / "a") != sha256_of_file(tmp_path / "b")

    def test_empty_vs_nonempty(self, tmp_path):
        (tmp_path / "a").write_bytes(b"")
        (tmp_path / "b").write_bytes(b"\x00")

        assert not files_are_identical(tmp_path / "a", tmp_path / "b")
        assert sha256_of_file(tmp_path / "a") != sha256_of_file(tmp_path / "b")

    def test_both_empty(self, tmp_path):
        (tmp_path / "a").write_bytes(b"")
        (tmp_path / "b").write_bytes(b"")

        assert files_are_identical(tmp_path / "a", tmp_path / "b")
        assert sha256_of_file(tmp_path / "a") == sha256_of_file(tmp_path / "b")


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — corruption detection (remote upload)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestCorruptionDetectionRemoteUpload:
    """
    Upload files with kosmokopy, then corrupt the remote copy and
    verify that our remote hash check catches the corruption.
    """

    def test_upload_then_corrupt_remote(self, tmp_path, remote_dest):
        """Corrupting a remote file after upload is detected by sha256_remote."""
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(8192)
        (src / "test.bin").write_bytes(data)
        original_hash = sha256_of_file(src / "test.bin")

        result = run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"

        # Verify intact
        assert sha256_remote(host, rdir + "/src/test.bin") == original_hash

        # Corrupt remotely: append a byte
        subprocess.run(
            ["ssh"] + SSH_CTL + [host,
             "printf '\\x00' >> " + _sq(rdir + "/src/test.bin")],
            check=True, capture_output=True,
        )

        assert sha256_remote(host, rdir + "/src/test.bin") != original_hash

    def test_upload_then_truncate_remote(self, tmp_path, remote_dest):
        """Truncating a remote file after upload is detected."""
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(10_000)
        (src / "big.bin").write_bytes(data)
        original_hash = sha256_of_file(src / "big.bin")

        result = run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"
        assert sha256_remote(host, rdir + "/src/big.bin") == original_hash

        # Truncate remotely
        subprocess.run(
            ["ssh"] + SSH_CTL + [host,
             "truncate -s 100 " + _sq(rdir + "/src/big.bin")],
            check=True, capture_output=True,
        )

        assert sha256_remote(host, rdir + "/src/big.bin") != original_hash

    def test_upload_then_replace_remote(self, tmp_path, remote_dest):
        """Replacing remote file content entirely is detected."""
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        (src / "doc.txt").write_text("Original content\n")
        original_hash = sha256_of_file(src / "doc.txt")

        result = run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"
        assert sha256_remote(host, rdir + "/src/doc.txt") == original_hash

        # Replace remotely
        subprocess.run(
            ["ssh"] + SSH_CTL + [host,
             "echo 'CORRUPTED' > " + _sq(rdir + "/src/doc.txt")],
            check=True, capture_output=True,
        )

        assert sha256_remote(host, rdir + "/src/doc.txt") != original_hash

    def test_upload_then_delete_remote(self, tmp_path, remote_dest):
        """Deleting a remote file after upload means it no longer exists."""
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        (src / "remove_me.txt").write_text("will be deleted\n")

        result = run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"
        assert remote_file_exists(host, rdir + "/src/remove_me.txt")

        # Delete remotely
        subprocess.run(
            ["ssh"] + SSH_CTL + [host,
             "rm " + _sq(rdir + "/src/remove_me.txt")],
            check=True, capture_output=True,
        )

        assert not remote_file_exists(host, rdir + "/src/remove_me.txt")

    def test_upload_multiple_corrupt_one_remote(self, tmp_src, remote_dest):
        """Corrupting one of several uploaded files is pinpointed."""
        host, rdir = remote_dest

        originals = {}
        for f in tmp_src.rglob("*"):
            if f.is_file():
                originals[f.relative_to(tmp_src)] = sha256_of_file(f)

        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"

        # Verify all intact
        for rel, h in originals.items():
            remote_path = "{}/{}/{}".format(rdir, tmp_src.name, rel)
            assert sha256_remote(host, remote_path) == h

        # Corrupt just hello.txt
        subprocess.run(
            ["ssh"] + SSH_CTL + [host,
             "echo 'CORRUPT' > " + _sq(rdir + "/" + tmp_src.name + "/hello.txt")],
            check=True, capture_output=True,
        )

        corrupted_count = 0
        for rel, h in originals.items():
            remote_path = "{}/{}/{}".format(rdir, tmp_src.name, rel)
            if sha256_remote(host, remote_path) != h:
                corrupted_count += 1

        assert corrupted_count == 1


# ═══════════════════════════════════════════════════════════════════════
#  NEGATIVE tests — corruption detection (remote download)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestCorruptionDetectionRemoteDownload:
    """
    Download files with kosmokopy, then corrupt the local copy and
    verify that comparing against the still-intact remote catches it.
    """

    def test_download_then_corrupt_local(self, tmp_path, remote_src):
        """Corrupting a downloaded file is caught when re-checked against remote."""
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(src="{}:{}".format(host, rdir), dst=dst)
        assert result["status"] == "finished"

        root_name = Path(rdir).name
        # Pick a file, record its remote hash
        target = dst / root_name / "remote_a.txt"
        assert target.exists()
        remote_hash = sha256_remote(host, rdir + "/remote_a.txt")
        assert sha256_of_file(target) == remote_hash

        # Corrupt locally
        target.write_text("LOCALLY CORRUPTED\n")

        assert sha256_of_file(target) != remote_hash

    def test_download_then_truncate_local(self, tmp_path, remote_src):
        """Truncating a downloaded file is detected."""
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(src="{}:{}".format(host, rdir), dst=dst)
        assert result["status"] == "finished"

        root_name = Path(rdir).name
        target = dst / root_name / "remote_b.bin"
        assert target.exists()
        remote_hash = sha256_remote(host, rdir + "/remote_b.bin")
        assert sha256_of_file(target) == remote_hash

        # Truncate to zero
        target.write_bytes(b"")

        assert sha256_of_file(target) != remote_hash

    def test_download_then_delete_local(self, tmp_path, remote_src):
        """Deleting a downloaded file means it's gone locally."""
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(src="{}:{}".format(host, rdir), dst=dst)
        assert result["status"] == "finished"

        root_name = Path(rdir).name
        target = dst / root_name / "remote_a.txt"
        assert target.exists()
        target.unlink()
        assert not target.exists()

        # Remote is still intact
        assert remote_file_exists(host, rdir + "/remote_a.txt")

