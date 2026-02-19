"""
Exclusion-pattern tests.

Operations are performed by invoking ``kosmokopy --cli`` with --exclude.
Verification is done in Python.
"""

import os
from pathlib import Path

import pytest

from conftest import run_kosmokopy, sha256_of_file


# ═══════════════════════════════════════════════════════════════════════
#  Exact directory exclusion
# ═══════════════════════════════════════════════════════════════════════


class TestDirectoryExclusion:

    def test_exclude_named_directory(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["/cache"],
        )
        assert result["status"] == "finished"
        assert result["excluded_dirs"] == 1

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "cached.dat" not in dst_names
        assert "keep.txt" in dst_names
        assert "doc.txt" in dst_names

    def test_exclude_multiple_directories(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst,
            exclude=["/cache", "/important"],
        )
        assert result["status"] == "finished"
        assert result["excluded_dirs"] == 2

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "cached.dat" not in dst_names
        assert "doc.txt" not in dst_names
        assert "keep.txt" in dst_names

    def test_nonexistent_dir_exclusion_harmless(self, tmp_src_with_exclusions, tmp_dst):
        """Excluding a non-existent dir doesn't cause errors."""
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["/nonexistent"],
        )
        assert result["status"] == "finished"
        assert result["errors"] == []
        assert result["excluded_files"] == 0
        assert result["excluded_dirs"] == 0

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "keep.txt" in dst_names
        assert "cached.dat" in dst_names


# ═══════════════════════════════════════════════════════════════════════
#  Exact file exclusion
# ═══════════════════════════════════════════════════════════════════════


class TestFileExclusion:

    def test_exclude_named_file(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["skip_me.log"],
        )
        assert result["status"] == "finished"
        assert result["excluded_files"] == 1

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "skip_me.log" not in dst_names
        assert "keep.txt" in dst_names

    def test_exclude_multiple_files(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst,
            exclude=["skip_me.log", "data.tmp"],
        )
        assert result["status"] == "finished"
        assert result["excluded_files"] == 2

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "skip_me.log" not in dst_names
        assert "data.tmp" not in dst_names


# ═══════════════════════════════════════════════════════════════════════
#  Wildcard directory exclusion  (~/pattern)
# ═══════════════════════════════════════════════════════════════════════


class TestWildcardDirExclusion:

    def test_wildcard_dir_star(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["~/build*"],
        )
        assert result["status"] == "finished"
        assert result["excluded_dirs"] == 1

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "artifact.o" not in dst_names
        assert "keep.txt" in dst_names

    def test_wildcard_dir_question(self, tmp_src_with_exclusions, tmp_dst):
        """? matches single character: 'cach?' matches 'cache'."""
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["~/cach?"],
        )
        assert result["status"] == "finished"
        assert result["excluded_dirs"] == 1

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "cached.dat" not in dst_names

    def test_wildcard_dir_no_match(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["~/zzz*"],
        )
        assert result["status"] == "finished"
        assert result["excluded_dirs"] == 0

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "cached.dat" in dst_names


# ═══════════════════════════════════════════════════════════════════════
#  Wildcard file exclusion  (~pattern)
# ═══════════════════════════════════════════════════════════════════════


class TestWildcardFileExclusion:

    def test_wildcard_file_extension(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["~*.log"],
        )
        assert result["status"] == "finished"
        assert result["excluded_files"] == 1

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "skip_me.log" not in dst_names

    def test_wildcard_file_case_insensitive(self, tmp_src_with_exclusions, tmp_dst):
        """*.jpg should match both PHOTO.JPG and snapshot.jpg."""
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["~*.jpg"],
        )
        assert result["status"] == "finished"
        assert result["excluded_files"] == 2

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "PHOTO.JPG" not in dst_names
        assert "snapshot.jpg" not in dst_names

    def test_wildcard_file_star_prefix(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst, exclude=["~data.*"],
        )
        assert result["status"] == "finished"
        assert result["excluded_files"] == 1

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "data.tmp" not in dst_names
        assert "keep.txt" in dst_names


# ═══════════════════════════════════════════════════════════════════════
#  Combined exclusions
# ═══════════════════════════════════════════════════════════════════════


class TestCombinedExclusions:

    def test_dir_and_file_exclusion(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst,
            exclude=["/cache", "skip_me.log"],
        )
        assert result["status"] == "finished"
        assert result["excluded_dirs"] == 1
        assert result["excluded_files"] == 1

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "cached.dat" not in dst_names
        assert "skip_me.log" not in dst_names
        assert "keep.txt" in dst_names

    def test_all_four_exclusion_types(self, tmp_src_with_exclusions, tmp_dst):
        result = run_kosmokopy(
            src=tmp_src_with_exclusions, dst=tmp_dst,
            exclude=["/cache", "skip_me.log", "~/build*", "~*.tmp"],
        )
        assert result["status"] == "finished"
        assert result["excluded_dirs"] == 2   # /cache + ~/build*
        assert result["excluded_files"] == 2  # skip_me.log + ~*.tmp

        dst_names = {f.name for f in tmp_dst.rglob("*") if f.is_file()}
        assert "cached.dat" not in dst_names    # /cache dir excluded
        assert "skip_me.log" not in dst_names   # exact file excluded
        assert "artifact.o" not in dst_names    # build* dir excluded
        assert "data.tmp" not in dst_names      # *.tmp file excluded
        assert "keep.txt" in dst_names
        assert "doc.txt" in dst_names
