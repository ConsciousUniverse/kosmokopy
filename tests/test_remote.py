"""
Remote transfer tests.

All file operations are performed through ``kosmokopy --cli``.
Results are verified in Python via SSH helper functions.
"""

import os
import subprocess
from pathlib import Path

import pytest

from conftest import (
    run_kosmokopy,
    requires_remote,
    requires_remote2,
    requires_rsync,
    sha256_of_file,
    sha256_remote,
    remote_file_exists,
    remote_ls,
    remote_read,
    remote_rm_rf,
    SSH_CTL,
    _sq,
    REMOTE_HOST,
    REMOTE_PATH,
)


# ═══════════════════════════════════════════════════════════════════════
#  Local → Remote (SCP / standard)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestLocalToRemoteSCP:
    """Upload from local directory to a remote host via standard method."""

    def test_basic_upload(self, tmp_src, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))

        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1

    def test_upload_file_count(self, tmp_src, remote_dest):
        host, rdir = remote_dest
        local_count = sum(1 for f in tmp_src.rglob("*") if f.is_file())

        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))
        assert result["copied"] == local_count

    def test_upload_preserves_content(self, tmp_src, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"

        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                remote_path = "{}/{}/{}".format(rdir, tmp_src.name, rel)
                assert sha256_of_file(f) == sha256_remote(host, remote_path)

    def test_upload_nested_dirs(self, tmp_src, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(src=tmp_src, dst="{}:{}".format(host, rdir))
        assert result["status"] == "finished"

        root = "{}/{}".format(rdir, tmp_src.name)
        assert remote_file_exists(host, root + "/subdir/nested.txt")
        assert remote_file_exists(host, root + "/subdir/level2/bottom.txt")

    def test_upload_selected_files(self, tmp_src, remote_dest):
        host, rdir = remote_dest
        files = [tmp_src / "hello.txt", tmp_src / "data.bin"]
        result = run_kosmokopy(
            src_files=files, dst="{}:{}".format(host, rdir), mode="files",
        )
        assert result["status"] == "finished"
        assert result["copied"] == 2

        assert remote_file_exists(host, rdir + "/hello.txt")
        assert remote_file_exists(host, rdir + "/data.bin")


# ═══════════════════════════════════════════════════════════════════════
#  Local → Remote (rsync)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
@requires_rsync
class TestLocalToRemoteRsync:

    def test_basic_upload(self, tmp_src, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(
            src=tmp_src, dst="{}:{}".format(host, rdir), method="rsync",
        )
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1

    def test_upload_content_match(self, tmp_src, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(
            src=tmp_src, dst="{}:{}".format(host, rdir), method="rsync",
        )
        assert result["status"] == "finished"

        for f in tmp_src.rglob("*"):
            if f.is_file():
                rel = f.relative_to(tmp_src)
                assert sha256_of_file(f) == sha256_remote(
                    host, "{}/{}/{}".format(rdir, tmp_src.name, rel),
                )


# ═══════════════════════════════════════════════════════════════════════
#  Remote → Local (SCP / standard)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteToLocalSCP:
    """Download from remote source to local destination."""

    def test_basic_download(self, remote_src, tmp_path):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(src="{}:{}".format(host, rdir), dst=dst)
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1

    def test_download_preserves_content(self, remote_src, tmp_path):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(src="{}:{}".format(host, rdir), dst=dst)
        assert result["status"] == "finished"

        root_name = Path(rdir).name
        root = dst / root_name
        for f in root.rglob("*"):
            if f.is_file():
                rel = f.relative_to(root)
                remote_path = "{}/{}".format(rdir, rel)
                assert sha256_of_file(f) == sha256_remote(host, remote_path)

    def test_download_nested(self, remote_src, tmp_path):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(src="{}:{}".format(host, rdir), dst=dst)
        assert result["status"] == "finished"

        root_name = Path(rdir).name
        assert (dst / root_name / "rsub" / "remote_c.txt").exists()
        assert (dst / root_name / "rsub" / "remote_c.txt").read_text() == "Remote nested C\n"


# ═══════════════════════════════════════════════════════════════════════
#  Remote single-file download (regression test for collect_remote_files)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteSingleFileDownload:
    """Downloading a single file from a remote host (not a directory tree)."""

    def test_download_single_remote_file(self, tmp_path):
        """Create one file on the remote, download it, verify content."""
        if not (REMOTE_HOST and REMOTE_PATH):
            pytest.skip("Remote host not configured")

        import subprocess as sp
        test_dir = "{}/single_file_test_{}".format(
            REMOTE_PATH.rstrip("/"), id(object()),
        )
        sp.run(
            ["ssh"] + SSH_CTL + [REMOTE_HOST, "mkdir -p " + _sq(test_dir)],
            check=True, capture_output=True,
        )
        sp.run(
            ["ssh"] + SSH_CTL + [REMOTE_HOST,
             "echo 'single file content' > " + _sq(test_dir + "/only.txt")],
            check=True, capture_output=True,
        )

        try:
            dst = tmp_path / "dst"
            result = run_kosmokopy(
                src="{}:{}".format(REMOTE_HOST, test_dir), dst=dst,
            )
            assert result["status"] == "finished"
            assert result["copied"] == 1
            assert result["errors"] == []
            root_name = Path(test_dir).name
            assert (dst / root_name / "only.txt").exists()
            assert (dst / root_name / "only.txt").read_text().strip() == "single file content"
        finally:
            remote_rm_rf(REMOTE_HOST, test_dir)

    def test_download_single_remote_file_rsync(self, tmp_path):
        """Same as above but via rsync method."""
        if not (REMOTE_HOST and REMOTE_PATH):
            pytest.skip("Remote host not configured")
        import shutil
        if shutil.which("rsync") is None:
            pytest.skip("rsync not installed")

        import subprocess as sp
        test_dir = "{}/single_rsync_test_{}".format(
            REMOTE_PATH.rstrip("/"), id(object()),
        )
        sp.run(
            ["ssh"] + SSH_CTL + [REMOTE_HOST, "mkdir -p " + _sq(test_dir)],
            check=True, capture_output=True,
        )
        sp.run(
            ["ssh"] + SSH_CTL + [REMOTE_HOST,
             "echo 'rsync single' > " + _sq(test_dir + "/rsingle.txt")],
            check=True, capture_output=True,
        )

        try:
            dst = tmp_path / "dst"
            result = run_kosmokopy(
                src="{}:{}".format(REMOTE_HOST, test_dir), dst=dst,
                method="rsync",
            )
            assert result["status"] == "finished"
            assert result["copied"] == 1
            root_name = Path(test_dir).name
            assert (dst / root_name / "rsingle.txt").exists()
        finally:
            remote_rm_rf(REMOTE_HOST, test_dir)


# ═══════════════════════════════════════════════════════════════════════
#  Remote single-file upload
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteSingleFileUpload:
    """Upload a single file to a remote host."""

    def test_upload_single_file(self, tmp_path, remote_dest):
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        f = src / "upload_only.txt"
        f.write_text("upload only\n")
        expected = sha256_of_file(f)

        result = run_kosmokopy(
            src_files=[f], dst="{}:{}".format(host, rdir), mode="files",
        )
        assert result["status"] == "finished"
        assert result["copied"] == 1
        assert sha256_remote(host, rdir + "/upload_only.txt") == expected


# ═══════════════════════════════════════════════════════════════════════
#  Remote → Local (rsync)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
@requires_rsync
class TestRemoteToLocalRsync:

    def test_basic_download(self, remote_src, tmp_path):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(
            src="{}:{}".format(host, rdir), dst=dst, method="rsync",
        )
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1

    def test_download_content_match(self, remote_src, tmp_path):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(
            src="{}:{}".format(host, rdir), dst=dst, method="rsync",
        )
        assert result["status"] == "finished"

        root_name = Path(rdir).name
        root = dst / root_name
        for f in root.rglob("*"):
            if f.is_file():
                rel = f.relative_to(root)
                remote_path = "{}/{}".format(rdir, rel)
                assert sha256_of_file(f) == sha256_remote(host, remote_path)


# ═══════════════════════════════════════════════════════════════════════
#  Remote → Remote (SCP relay)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
@requires_remote2
class TestRemoteToRemoteSCPRelay:
    """Transfer files from one remote host to another via relay."""

    def test_basic_relay(self, remote_src, remote_dest2):
        src_host, src_dir = remote_src
        dst_host, dst_dir = remote_dest2

        result = run_kosmokopy(
            src="{}:{}".format(src_host, src_dir),
            dst="{}:{}".format(dst_host, dst_dir),
        )
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1

    def test_relay_content_match(self, remote_src, remote_dest2):
        src_host, src_dir = remote_src
        dst_host, dst_dir = remote_dest2

        result = run_kosmokopy(
            src="{}:{}".format(src_host, src_dir),
            dst="{}:{}".format(dst_host, dst_dir),
        )
        assert result["status"] == "finished"

        src_root = Path(src_dir).name
        src_files = remote_ls(src_host, src_dir)
        for full_path in src_files:
            rel = os.path.relpath(full_path, src_dir)
            src_hash = sha256_remote(src_host, full_path)
            dst_hash = sha256_remote(dst_host, "{}/{}/{}".format(dst_dir, src_root, rel))
            assert src_hash == dst_hash, "Hash mismatch for {}".format(rel)


# ═══════════════════════════════════════════════════════════════════════
#  Remote → Remote (rsync relay)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
@requires_remote2
@requires_rsync
class TestRemoteToRemoteRsyncRelay:

    def test_relay_rsync(self, remote_src, remote_dest2):
        src_host, src_dir = remote_src
        dst_host, dst_dir = remote_dest2

        result = run_kosmokopy(
            src="{}:{}".format(src_host, src_dir),
            dst="{}:{}".format(dst_host, dst_dir),
            method="rsync",
        )
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1


# ═══════════════════════════════════════════════════════════════════════
#  Real source → Remote (if KOSMOKOPY_TEST_SOURCE_DIR is set)
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRealSourceToRemote:

    def test_upload_real_source(self, real_source, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(
            src=real_source, dst="{}:{}".format(host, rdir),
        )
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["copied"] >= 1

        # Spot-check: at least one file matches
        first_local = next(real_source.rglob("*"))
        while first_local.is_dir():
            first_local = next(real_source.rglob("*"))
        rel = first_local.relative_to(real_source)
        remote_path = "{}/{}/{}".format(rdir, real_source.name, rel)
        assert sha256_of_file(first_local) == sha256_remote(host, remote_path)


# ═══════════════════════════════════════════════════════════════════════
#  Remote move
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteMove:

    def test_upload_move_removes_source(self, tmp_path, remote_dest):
        host, rdir = remote_dest
        src = tmp_path / "src"
        src.mkdir()
        f = src / "to_move.txt"
        f.write_text("move me\n")
        expected = sha256_of_file(f)

        result = run_kosmokopy(
            src=src, dst="{}:{}".format(host, rdir), move=True,
        )
        assert result["status"] == "finished"
        assert result["copied"] == 1
        assert not f.exists()
        assert sha256_remote(host, rdir + "/src/to_move.txt") == expected

    def test_download_move_removes_remote(self, remote_src, tmp_path):
        host, rdir = remote_src
        dst = tmp_path / "dst"

        result = run_kosmokopy(
            src="{}:{}".format(host, rdir), dst=dst, move=True,
        )
        assert result["status"] == "finished"
        assert result["copied"] >= 1

        remaining = remote_ls(host, rdir)
        assert remaining == []


# ═══════════════════════════════════════════════════════════════════════
#  Remote conflict handling
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteConflicts:

    def test_skip_existing_remote(self, tmp_path, remote_dest):
        host, rdir = remote_dest

        # Upload once
        src = tmp_path / "src"
        src.mkdir()
        (src / "file.txt").write_text("original\n")
        run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))

        # Modify source
        (src / "file.txt").write_text("modified\n")
        result = run_kosmokopy(
            src=src, dst="{}:{}".format(host, rdir), conflict="skip",
        )
        assert result["status"] == "finished"
        assert any("file.txt" in s for s in result["skipped"])

        # Remote should still have original
        content = remote_read(host, rdir + "/src/file.txt")
        assert content == b"original\n"

    def test_overwrite_existing_remote(self, tmp_path, remote_dest):
        host, rdir = remote_dest

        src = tmp_path / "src"
        src.mkdir()
        (src / "file.txt").write_text("original\n")
        run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))

        (src / "file.txt").write_text("modified\n")
        result = run_kosmokopy(
            src=src, dst="{}:{}".format(host, rdir), conflict="overwrite",
        )
        assert result["status"] == "finished"
        assert result["copied"] >= 1

        content = remote_read(host, rdir + "/src/file.txt")
        assert content == b"modified\n"

    def test_rename_existing_remote(self, tmp_path, remote_dest):
        host, rdir = remote_dest

        src = tmp_path / "src"
        src.mkdir()
        (src / "file.txt").write_text("original\n")
        run_kosmokopy(src=src, dst="{}:{}".format(host, rdir))

        (src / "file.txt").write_text("second version\n")
        result = run_kosmokopy(
            src=src, dst="{}:{}".format(host, rdir), conflict="rename",
        )
        assert result["status"] == "finished"
        assert result["copied"] >= 1

        # Both should exist
        assert remote_file_exists(host, rdir + "/src/file.txt")
        assert remote_file_exists(host, rdir + "/src/file_1.txt")


# ═══════════════════════════════════════════════════════════════════════
#  Remote exclusions
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestRemoteExclusions:

    def test_exclude_pattern_upload(self, tmp_src_with_exclusions, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(
            src=tmp_src_with_exclusions,
            dst="{}:{}".format(host, rdir),
            exclude=["/cache"],
        )
        assert result["status"] == "finished"
        assert not remote_file_exists(host, rdir + "/source/cache/cached.dat")
        assert remote_file_exists(host, rdir + "/source/keep.txt")

    def test_wildcard_exclude_upload(self, tmp_src_with_exclusions, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(
            src=tmp_src_with_exclusions,
            dst="{}:{}".format(host, rdir),
            exclude=["~*.log", "~*.tmp"],
        )
        assert result["status"] == "finished"
        assert not remote_file_exists(host, rdir + "/source/skip_me.log")
        assert not remote_file_exists(host, rdir + "/source/data.tmp")
        assert remote_file_exists(host, rdir + "/source/keep.txt")


# ═══════════════════════════════════════════════════════════════════════
#  Strip spaces — remote
# ═══════════════════════════════════════════════════════════════════════


@requires_remote
class TestStripSpacesRemote:

    def test_strip_spaces_upload(self, tmp_src_with_spaces, remote_dest):
        host, rdir = remote_dest
        result = run_kosmokopy(
            src=tmp_src_with_spaces,
            dst="{}:{}".format(host, rdir),
            strip_spaces=True,
        )
        assert result["status"] == "finished"
        assert result["errors"] == []

        assert remote_file_exists(host, rdir + "/sourcespaces/myfile.txt")
        assert remote_file_exists(host, rdir + "/sourcespaces/anotherdoc.pdf")
        assert remote_file_exists(host, rdir + "/sourcespaces/subfolder/innerfile.txt")
