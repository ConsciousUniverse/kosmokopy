"""
Kosmokopy — External Integration Test Suite
=============================================

Shared fixtures, configuration, and the CLI runner helper.

Every test invokes the actual Kosmokopy Rust binary (via ``--cli``)
to perform the transfer, then verifies the results in Python.

Environment variables (all optional — remote tests are skipped if unset):

  KOSMOKOPY_TEST_REMOTE_HOST    SSH host for remote tests
                                  e.g. "myserver" or "user@myserver"
  KOSMOKOPY_TEST_REMOTE_PATH    Writable base path on that host
                                  e.g. "/tmp/kosmokopy_test"
  KOSMOKOPY_TEST_REMOTE_HOST2   Second SSH host (for remote→remote tests)
  KOSMOKOPY_TEST_REMOTE_PATH2   Writable base path on second host
  KOSMOKOPY_TEST_SOURCE_DIR     Local directory with real files to use as
                                  source material.  If unset, synthetic test
                                  files are generated automatically.
  KOSMOKOPY_BIN                 Path to the kosmokopy binary.  Defaults to
                                  target/debug/kosmokopy relative to the
                                  project root.
"""

import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

import pytest


# ── Locate the binary ──────────────────────────────────────────────────

_PROJECT_ROOT = Path(__file__).resolve().parent.parent
KOSMOKOPY_BIN = os.environ.get(
    "KOSMOKOPY_BIN",
    str(_PROJECT_ROOT / "target" / "debug" / "kosmokopy"),
)


# ── Configuration from environment ──────────────────────────────────────

REMOTE_HOST = os.environ.get("KOSMOKOPY_TEST_REMOTE_HOST")
REMOTE_PATH = os.environ.get("KOSMOKOPY_TEST_REMOTE_PATH")
REMOTE_HOST2 = os.environ.get("KOSMOKOPY_TEST_REMOTE_HOST2")
REMOTE_PATH2 = os.environ.get("KOSMOKOPY_TEST_REMOTE_PATH2")
SOURCE_DIR = os.environ.get("KOSMOKOPY_TEST_SOURCE_DIR")

# SSH control-socket args — mirrors what the app uses
SSH_CTL = [
    "-o", "ControlMaster=auto",
    "-o", "ControlPath=/tmp/kosmokopy_test_ssh_%h_%p_%r",
    "-o", "ControlPersist=60",
]


# ── Skip markers ────────────────────────────────────────────────────────

requires_remote = pytest.mark.skipif(
    not (REMOTE_HOST and REMOTE_PATH),
    reason="KOSMOKOPY_TEST_REMOTE_HOST / _PATH not set",
)
requires_remote2 = pytest.mark.skipif(
    not (REMOTE_HOST2 and REMOTE_PATH2),
    reason="KOSMOKOPY_TEST_REMOTE_HOST2 / _PATH2 not set",
)
requires_rsync = pytest.mark.skipif(
    shutil.which("rsync") is None,
    reason="rsync not installed",
)


# ── CLI runner ──────────────────────────────────────────────────────────

def run_kosmokopy(
    *,
    src=None,
    dst,
    src_files=None,
    move=False,
    conflict="skip",
    strip_spaces=False,
    mode="folders",
    method="standard",
    exclude=None,
):
    """
    Invoke ``kosmokopy --cli`` with the given options and return the
    parsed JSON result dict.

    Returns a dict with either:
      {"status": "finished", "copied": N, "skipped": [...], "excluded": N, "errors": [...]}
    or:
      {"status": "error", "message": "..."}
    """
    cmd = [KOSMOKOPY_BIN, "--cli"]

    if src is not None:
        cmd += ["--src", str(src)]
    if src_files is not None:
        cmd += ["--src-files", ",".join(str(f) for f in src_files)]

    cmd += ["--dst", str(dst)]

    if move:
        cmd.append("--move")

    cmd += ["--conflict", conflict]

    if strip_spaces:
        cmd.append("--strip-spaces")

    cmd += ["--mode", mode]
    cmd += ["--method", method]

    if exclude:
        for pat in exclude:
            cmd += ["--exclude", pat]

    result = subprocess.run(cmd, capture_output=True, text=True, timeout=120)

    # Parse the JSON line from stdout
    stdout = result.stdout.strip()
    if stdout:
        return json.loads(stdout)

    # Fallback — binary failed without producing JSON
    return {
        "status": "error",
        "message": f"exit code {result.returncode}: {result.stderr.strip()}",
    }


# ── Helpers ─────────────────────────────────────────────────────────────

def sha256_of_file(path):
    """Return hex SHA-256 digest of a local file."""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_remote(host, remote_path):
    """Return hex SHA-256 digest of a remote file via SSH."""
    r = subprocess.run(
        ["ssh"] + SSH_CTL + [host, "sha256sum " + _sq(remote_path)],
        capture_output=True, text=True,
    )
    if r.returncode == 0 and r.stdout.strip():
        return r.stdout.strip().split()[0]
    r = subprocess.run(
        ["ssh"] + SSH_CTL + [host, "shasum -a 256 " + _sq(remote_path)],
        capture_output=True, text=True,
    )
    if r.returncode == 0 and r.stdout.strip():
        return r.stdout.strip().split()[0]
    raise RuntimeError("Cannot hash remote file {} on {}".format(remote_path, host))


def remote_file_exists(host, remote_path):
    r = subprocess.run(
        ["ssh"] + SSH_CTL + [host, "test -e " + _sq(remote_path)],
        capture_output=True,
    )
    return r.returncode == 0


def remote_ls(host, remote_dir):
    """List files under a remote directory (returns full paths)."""
    r = subprocess.run(
        ["ssh"] + SSH_CTL + [host, "find " + _sq(remote_dir) + " -type f 2>/dev/null"],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        return []
    return [l.strip() for l in r.stdout.strip().splitlines() if l.strip()]


def remote_read(host, remote_path):
    """Read a remote file's contents."""
    r = subprocess.run(
        ["ssh"] + SSH_CTL + [host, "cat " + _sq(remote_path)],
        capture_output=True,
    )
    assert r.returncode == 0, "Failed to read {} on {}".format(remote_path, host)
    return r.stdout


def remote_rm_rf(host, remote_path):
    """Recursively remove a remote directory."""
    subprocess.run(
        ["ssh"] + SSH_CTL + [host, "rm -rf " + _sq(remote_path)],
        capture_output=True,
    )


def _sq(s):
    """Shell-quote with single quotes (mirrors Kosmokopy's shell_quote)."""
    return "'" + s.replace("'", "'\\''") + "'"


def files_are_identical(a, b):
    """Byte-by-byte comparison — mirrors the Rust function."""
    a, b = Path(a), Path(b)
    if a.stat().st_size != b.stat().st_size:
        return False
    with open(a, "rb") as fa, open(b, "rb") as fb:
        while True:
            chunk_a = fa.read(8192)
            chunk_b = fb.read(8192)
            if chunk_a != chunk_b:
                return False
            if not chunk_a:
                return True


# ── Fixtures ────────────────────────────────────────────────────────────

@pytest.fixture
def tmp_src(tmp_path):
    """Create a temporary source directory with a handful of test files."""
    src = tmp_path / "source"
    src.mkdir()

    (src / "hello.txt").write_text("Hello, World!\n")
    (src / "data.bin").write_bytes(os.urandom(4096))
    (src / "notes.md").write_text("# Notes\nSome notes here.\n")

    sub = src / "subdir"
    sub.mkdir()
    (sub / "nested.txt").write_text("I am nested.\n")
    (sub / "deep.dat").write_bytes(os.urandom(2048))

    deep = sub / "level2"
    deep.mkdir()
    (deep / "bottom.txt").write_text("Bottom level.\n")

    return src


@pytest.fixture
def tmp_src_with_spaces(tmp_path):
    """Source tree with spaces in filenames and directory names."""
    src = tmp_path / "source spaces"
    src.mkdir()
    (src / "my file.txt").write_text("file with spaces\n")
    (src / "another doc.pdf").write_bytes(b"%PDF-fake content")
    sub = src / "sub folder"
    sub.mkdir()
    (sub / "inner file.txt").write_text("inner\n")
    return src


@pytest.fixture
def tmp_src_with_exclusions(tmp_path):
    """Source tree designed for testing exclusion patterns."""
    src = tmp_path / "source"
    src.mkdir()

    (src / "keep.txt").write_text("keep\n")
    (src / "skip_me.log").write_text("log\n")
    (src / "data.tmp").write_text("temp\n")
    (src / "PHOTO.JPG").write_bytes(b"\xff\xd8\xff\xe0 fake jpg")
    (src / "snapshot.jpg").write_bytes(b"\xff\xd8\xff\xe0 another jpg")

    exc = src / "cache"
    exc.mkdir()
    (exc / "cached.dat").write_bytes(b"cached")

    kept = src / "important"
    kept.mkdir()
    (kept / "doc.txt").write_text("important doc\n")

    build = src / "build_output"
    build.mkdir()
    (build / "artifact.o").write_bytes(b"obj")

    return src


@pytest.fixture
def tmp_dst(tmp_path):
    """Empty destination directory."""
    dst = tmp_path / "dest"
    dst.mkdir()
    return dst


@pytest.fixture
def real_source():
    """Use user-supplied source directory if available."""
    if SOURCE_DIR and Path(SOURCE_DIR).is_dir():
        return Path(SOURCE_DIR)
    pytest.skip("KOSMOKOPY_TEST_SOURCE_DIR not set or not a directory")


@pytest.fixture
def remote_dest():
    """Provide a unique remote destination path; clean up after test."""
    if not (REMOTE_HOST and REMOTE_PATH):
        pytest.skip("Remote host not configured")
    test_dir = "{}/test_{}_{}".format(REMOTE_PATH.rstrip("/"), os.getpid(), id(object()))
    subprocess.run(
        ["ssh"] + SSH_CTL + [REMOTE_HOST, "mkdir -p " + _sq(test_dir)],
        check=True, capture_output=True,
    )
    yield REMOTE_HOST, test_dir
    remote_rm_rf(REMOTE_HOST, test_dir)


@pytest.fixture
def remote_src():
    """Create a remote source directory with test files; clean up after."""
    if not (REMOTE_HOST and REMOTE_PATH):
        pytest.skip("Remote host not configured")
    test_dir = "{}/src_{}_{}".format(REMOTE_PATH.rstrip("/"), os.getpid(), id(object()))
    subprocess.run(
        ["ssh"] + SSH_CTL + [REMOTE_HOST, "mkdir -p " + _sq(test_dir)],
        check=True, capture_output=True,
    )
    with tempfile.TemporaryDirectory() as td:
        p = Path(td)
        (p / "remote_a.txt").write_text("Remote file A\n")
        (p / "remote_b.bin").write_bytes(os.urandom(2048))
        sub = p / "rsub"
        sub.mkdir()
        (sub / "remote_c.txt").write_text("Remote nested C\n")
        subprocess.run(
            ["scp"] + SSH_CTL + ["-q", "-r"]
            + [str(p / "remote_a.txt"), str(p / "remote_b.bin"), str(p / "rsub")]
            + ["{}:{}/".format(REMOTE_HOST, test_dir)],
            check=True, capture_output=True,
        )
    yield REMOTE_HOST, test_dir
    remote_rm_rf(REMOTE_HOST, test_dir)


@pytest.fixture
def remote_dest2():
    """Remote destination on second host; clean up after."""
    if not (REMOTE_HOST2 and REMOTE_PATH2):
        pytest.skip("Second remote host not configured")
    test_dir = "{}/test2_{}_{}".format(REMOTE_PATH2.rstrip("/"), os.getpid(), id(object()))
    subprocess.run(
        ["ssh"] + SSH_CTL + [REMOTE_HOST2, "mkdir -p " + _sq(test_dir)],
        check=True, capture_output=True,
    )
    yield REMOTE_HOST2, test_dir
    remote_rm_rf(REMOTE_HOST2, test_dir)
