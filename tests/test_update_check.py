"""
Tests for update_check.py's `run_self_update()` -- specifically the
diverged-history recovery path (_reclone / _DIVERGED_HISTORY_MARKER).

There was previously no test coverage for this module at all. This file
focuses on run_self_update() (the /update REPL command's implementation)
using *real* local git repos rather than mocked subprocess calls, because
the bug this covers is entirely about how a real `git pull --ff-only`
behaves against real diverged history -- a mocked subprocess would only
ever test that our code calls git with the right arguments, not that the
recovery actually works against git's real output and exit codes.

Found via a real fresh-machine Docker test: after a maintainer force-
pushed a history rewrite (git filter-repo, to strip an accidentally
committed private file) to the public repo, an already-installed user's
`/update` started failing with a raw "fatal: Not possible to
fast-forward, aborting." git error and no way to recover short of a
manual reinstall. _reclone() discards the local checkout and re-clones
fresh from origin instead, which is safe because config.json and .env
both live under the separate ~/.oh-my-shell/ directory, never inside the
repo checkout `run_self_update` operates on (see config.py's CONFIG_DIR
and wizard.py's ENV_PATH).
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from ohmyshell import update_check as update_check_module


def _run_git(args: list[str], cwd: Path) -> subprocess.CompletedProcess:
    result = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, f"git {args} failed: {result.stderr}"
    return result


def _init_repo_with_commit(path: Path, message: str) -> None:
    path.mkdir(parents=True, exist_ok=True)
    _run_git(["init", "-q", "-b", "main"], path)
    _run_git(["config", "user.email", "test@example.com"], path)
    _run_git(["config", "user.name", "Test"], path)
    (path / "README.md").write_text(f"{message}\n")
    _run_git(["add", "README.md"], path)
    _run_git(["commit", "-q", "-m", message], path)


@pytest.fixture
def diverged_repos(tmp_path):
    """
    Builds a real "origin" repo and a real "clone" checkout of it (tracking
    origin/main, exactly like install.sh's own first-time setup leaves a
    fresh install), then force-pushes a rewritten, unrelated history onto
    origin -- reproducing exactly what `git filter-repo` + `git push
    --force` does to a maintainer's public repo. Returns (origin_dir,
    clone_dir); `git -C clone_dir pull --ff-only` is confirmed to fail
    with the real "Not possible to fast-forward" error at this point.
    """
    origin_dir = tmp_path / "origin"
    _init_repo_with_commit(origin_dir, "original history")

    clone_dir = tmp_path / "install"
    _run_git(["clone", "-q", str(origin_dir), str(clone_dir)], tmp_path)

    # Rewrite origin's history to something unrelated and force-push it --
    # this is what a real `git filter-repo --invert-paths` + force-push
    # does to every branch/tag, from a downstream clone's point of view.
    _run_git(["checkout", "-q", "--orphan", "rewritten"], origin_dir)
    (origin_dir / "README.md").write_text("rewritten history\n")
    _run_git(["add", "README.md"], origin_dir)
    _run_git(["commit", "-q", "-m", "rewritten history"], origin_dir)
    _run_git(["branch", "-q", "-M", "rewritten", "main"], origin_dir)

    return origin_dir, clone_dir


class TestReclone:
    def test_recovers_from_diverged_history(self, diverged_repos):
        origin_dir, clone_dir = diverged_repos

        # Sanity check: confirms the fixture actually reproduces the real
        # bug before asserting our fix handles it.
        pull = subprocess.run(
            ["git", "-C", str(clone_dir), "pull", "--ff-only"],
            capture_output=True,
            text=True,
        )
        assert pull.returncode != 0
        assert update_check_module._DIVERGED_HISTORY_MARKER in (pull.stderr + pull.stdout)

        ok, error = update_check_module._reclone(clone_dir)

        assert ok is True
        assert error == ""
        # The checkout now matches origin's rewritten history, not the
        # stale local history it had before.
        assert (clone_dir / "README.md").read_text() == "rewritten history\n"
        log = _run_git(["log", "--oneline"], clone_dir)
        assert "rewritten history" in log.stdout

    def test_does_not_touch_sibling_oh_my_shell_config_dir(self, diverged_repos, tmp_path):
        # config.json / .env live under a completely separate ~/.oh-my-shell/
        # directory, never inside the repo checkout -- confirms _reclone()'s
        # rm -rf of the checkout can't reach them even if someone changes
        # the fixture's directory layout later.
        origin_dir, clone_dir = diverged_repos
        sibling_config_dir = tmp_path / ".oh-my-shell"
        sibling_config_dir.mkdir()
        (sibling_config_dir / "config.json").write_text('{"kept": true}')
        (sibling_config_dir / ".env").write_text("GOOGLE_AI_STUDIO_API_KEY=keep-me\n")

        ok, _error = update_check_module._reclone(clone_dir)

        assert ok is True
        assert (sibling_config_dir / "config.json").read_text() == '{"kept": true}'
        assert (sibling_config_dir / ".env").read_text() == "GOOGLE_AI_STUDIO_API_KEY=keep-me\n"


class TestRunSelfUpdate:
    def test_diverged_history_triggers_automatic_recovery_not_an_error(self, diverged_repos, monkeypatch):
        origin_dir, clone_dir = diverged_repos
        # pip install would need a real pyproject.toml in the fresh clone;
        # this test is about the git-recovery path, not packaging, so only
        # the final `pip install -e .` subprocess.run call is faked out --
        # every git subprocess.run call (including inside _reclone, called
        # by the real run_self_update) still runs for real.
        real_run = subprocess.run

        def _fake_run(cmd, **kwargs):
            if cmd[0] == update_check_module.sys.executable:
                return MagicMock(returncode=0, stdout="", stderr="")
            return real_run(cmd, **kwargs)

        monkeypatch.setattr(update_check_module.subprocess, "run", _fake_run)

        succeeded, message = update_check_module.run_self_update(clone_dir)

        assert succeeded is True
        assert "recovered" in message.lower()
        assert (clone_dir / "README.md").read_text() == "rewritten history\n"

    def test_non_diverged_pull_failure_still_reported_as_a_normal_error(self, tmp_path, monkeypatch):
        # A checkout with no configured remote at all fails `git pull` for
        # an unrelated reason ("no tracking information" / "no remote
        # repository") -- must NOT match _DIVERGED_HISTORY_MARKER and must
        # NOT trigger a destructive re-clone attempt.
        repo_dir = tmp_path / "install"
        _init_repo_with_commit(repo_dir, "solo repo, no remote")

        reclone_mock = MagicMock()
        monkeypatch.setattr(update_check_module, "_reclone", reclone_mock)

        succeeded, message = update_check_module.run_self_update(repo_dir)

        assert succeeded is False
        assert "git pull failed" in message
        reclone_mock.assert_not_called()

    def test_missing_git_dir_reports_reinstall_message_unchanged(self, tmp_path):
        not_a_repo = tmp_path / "not-a-repo"
        not_a_repo.mkdir()

        succeeded, message = update_check_module.run_self_update(not_a_repo)

        assert succeeded is False
        assert "Re-run the installer" in message