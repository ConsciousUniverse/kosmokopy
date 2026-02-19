"""
Integrity and verification tests.

Operations are performed by invoking ``kosmokopy --cli``.
Verification is done in Python — checking file identity, SHA-256
hashes, and correct error handling.
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

        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert files_are_identical(f, tmp_dst / rel)

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

        assert sha256_of_file(dst / "big.bin") == expected

    def test_empty_file_integrity(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        (src / "empty").write_bytes(b"")

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst)
        assert result["status"] == "finished"

        assert (dst / "empty").exists()
        assert (dst / "empty").stat().st_size == 0
        assert sha256_of_file(dst / "empty") == hashlib.sha256(b"").hexdigest()


# ═══════════════════════════════════════════════════════════════════════
#  Local copy integrity — rsync
# ═══════════════════════════════════════════════════════════════════════


@requires_rsync
class TestLocalRsyncIntegrity:

    def test_rsync_files_identical(self, tmp_src, tmp_dst):
        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, method="rsync")
        assert result["status"] == "finished"
        assert result["errors"] == []

        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert files_are_identical(f, tmp_dst / rel)

    def test_rsync_large_binary(self, tmp_path):
        src = tmp_path / "src"
        src.mkdir()
        data = os.urandom(100_000)
        (src / "big.bin").write_bytes(data)
        expected = hashlib.sha256(data).hexdigest()

        dst = tmp_path / "dst"
        result = run_kosmokopy(src=src, dst=dst, method="rsync")
        assert result["status"] == "finished"

        assert sha256_of_file(dst / "big.bin") == expected


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
        assert sha256_of_file(dst / "move_me.bin") == expected

    def test_move_multiple_files(self, tmp_src, tmp_dst):
        originals = {}
        for f in tmp_src.rglob("*"):
            if f.is_file():
                originals[f.relative_to(tmp_src)] = sha256_of_file(f)

        result = run_kosmokopy(src=tmp_src, dst=tmp_dst, move=True)
        assert result["status"] == "finished"

        for rel, h in originals.items():
            assert not (tmp_src / rel).exists(), "Source not removed: {}".format(rel)
            assert sha256_of_file(tmp_dst / rel) == h

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
        assert sha256_of_file(dst / "rsync_move.bin") == expected


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
                remote_path = "{}/{}".format(rdir, rel)
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
        assert sha256_remote(host, rdir + "/big.bin") == expected


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
        assert sha256_remote(host, rdir + "/to_move.bin") == expected


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

        for f in dst.rglob("*"):
            if f.is_file():
                rel = f.relative_to(dst)
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
