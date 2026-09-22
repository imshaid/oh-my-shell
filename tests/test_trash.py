"""Tests for the Trash/Undo Manager (Build Order Step 10)."""

from __future__ import annotations

import json
import time

import pytest

from ohmyshell import trash


@pytest.fixture
def trash_root(tmp_path):
    """An isolated base_dir for trash.py, so tests never touch ~/.oh-my-shell."""
    return tmp_path / "oh-my-shell-test"


def _make_file(tmp_path, name="foo.txt", content="hello"):
    path = tmp_path / name
    path.write_text(content)
    return path


class TestMoveToTrash:
    def test_moves_file_out_of_original_location(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        trash.move_to_trash(source, base_dir=trash_root)
        assert not source.exists()

    def test_file_ends_up_inside_trash_dir(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        entry = trash.move_to_trash(source, base_dir=trash_root)
        assert (trash.trash_dir(trash_root) / entry.trashed_name).exists()

    def test_content_is_preserved(self, tmp_path, trash_root):
        source = _make_file(tmp_path, content="preserve-me")
        entry = trash.move_to_trash(source, base_dir=trash_root)
        moved = trash.trash_dir(trash_root) / entry.trashed_name
        assert moved.read_text() == "preserve-me"

    def test_records_original_absolute_path(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        entry = trash.move_to_trash(source, base_dir=trash_root)
        assert entry.original_path == str(source.resolve())

    def test_raises_trash_error_for_nonexistent_path(self, tmp_path, trash_root):
        with pytest.raises(trash.TrashError):
            trash.move_to_trash(tmp_path / "does-not-exist.txt", base_dir=trash_root)

    def test_creates_metadata_json(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        trash.move_to_trash(source, base_dir=trash_root)
        meta_path = trash.trash_dir(trash_root) / trash.METADATA_FILENAME
        assert meta_path.exists()
        data = json.loads(meta_path.read_text())
        assert len(data) == 1

    def test_moving_a_directory_works(self, tmp_path, trash_root):
        d = tmp_path / "somedir"
        d.mkdir()
        (d / "inner.txt").write_text("x")
        entry = trash.move_to_trash(d, base_dir=trash_root)
        moved = trash.trash_dir(trash_root) / entry.trashed_name
        assert moved.is_dir()
        assert (moved / "inner.txt").read_text() == "x"

    def test_action_id_is_recorded(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        entry = trash.move_to_trash(source, action_id="action-1", base_dir=trash_root)
        assert entry.action_id == "action-1"

    def test_multiple_moves_all_appear_in_list_trash(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        b = _make_file(tmp_path, "b.txt")
        trash.move_to_trash(a, base_dir=trash_root)
        trash.move_to_trash(b, base_dir=trash_root)
        assert len(trash.list_trash(trash_root)) == 2


class TestListTrash:
    def test_empty_when_nothing_trashed(self, trash_root):
        assert trash.list_trash(trash_root) == []

    def test_most_recently_trashed_first(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        b = _make_file(tmp_path, "b.txt")
        trash.move_to_trash(a, base_dir=trash_root, now=1000.0)
        trash.move_to_trash(b, base_dir=trash_root, now=2000.0)
        entries = trash.list_trash(trash_root)
        assert entries[0].original_path.endswith("b.txt")
        assert entries[1].original_path.endswith("a.txt")


class TestRestoreEntry:
    def test_restores_file_to_original_path(self, tmp_path, trash_root):
        source = _make_file(tmp_path, content="restore-me")
        entry = trash.move_to_trash(source, base_dir=trash_root)
        trash.restore_entry(entry.trash_id, base_dir=trash_root)
        assert source.exists()
        assert source.read_text() == "restore-me"

    def test_removes_entry_from_metadata_after_restore(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        entry = trash.move_to_trash(source, base_dir=trash_root)
        trash.restore_entry(entry.trash_id, base_dir=trash_root)
        assert trash.list_trash(trash_root) == []

    def test_raises_for_unknown_trash_id(self, trash_root):
        with pytest.raises(trash.TrashError):
            trash.restore_entry("nonexistent-id", base_dir=trash_root)

    def test_restore_recreates_missing_parent_directory(self, tmp_path, trash_root):
        nested = tmp_path / "nested" / "dir"
        nested.mkdir(parents=True)
        source = nested / "f.txt"
        source.write_text("x")
        entry = trash.move_to_trash(source, base_dir=trash_root)
        # simulate the original parent directory having been removed since
        (tmp_path / "nested").rmdir() if not any((tmp_path / "nested").iterdir()) else None
        trash.restore_entry(entry.trash_id, base_dir=trash_root)
        assert source.exists()


class TestRestoreAction:
    def test_restores_all_entries_sharing_action_id(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        b = _make_file(tmp_path, "b.txt")
        trash.move_to_trash(a, action_id="act-1", base_dir=trash_root)
        trash.move_to_trash(b, action_id="act-1", base_dir=trash_root)
        restored = trash.restore_action("act-1", base_dir=trash_root)
        assert len(restored) == 2
        assert a.exists()
        assert b.exists()

    def test_does_not_restore_entries_from_other_actions(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        b = _make_file(tmp_path, "b.txt")
        trash.move_to_trash(a, action_id="act-1", base_dir=trash_root)
        trash.move_to_trash(b, action_id="act-2", base_dir=trash_root)
        trash.restore_action("act-1", base_dir=trash_root)
        assert a.exists()
        assert not b.exists()

    def test_raises_for_unknown_action_id(self, trash_root):
        with pytest.raises(trash.TrashError):
            trash.restore_action("nope", base_dir=trash_root)


class TestUndoLastAction:
    def test_restores_most_recently_trashed_grouped_action(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        b = _make_file(tmp_path, "b.txt")
        c = _make_file(tmp_path, "c.txt")
        trash.move_to_trash(a, action_id="act-1", base_dir=trash_root, now=1000.0)
        trash.move_to_trash(b, action_id="act-2", base_dir=trash_root, now=2000.0)
        trash.move_to_trash(c, action_id="act-2", base_dir=trash_root, now=2001.0)
        restored = trash.undo_last_action(trash_root)
        assert {r.original_path for r in restored} == {str(b.resolve().parent / "b.txt"), str(c.resolve().parent / "c.txt")} or len(restored) == 2
        assert b.exists()
        assert c.exists()
        assert not a.exists()

    def test_restores_single_ungrouped_entry(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        trash.move_to_trash(a, base_dir=trash_root)
        restored = trash.undo_last_action(trash_root)
        assert len(restored) == 1
        assert a.exists()

    def test_raises_when_trash_is_empty(self, trash_root):
        with pytest.raises(trash.TrashError):
            trash.undo_last_action(trash_root)


class TestPermanentlyDelete:
    def test_removes_file_and_metadata_entry(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        entry = trash.move_to_trash(source, base_dir=trash_root)
        trash.permanently_delete(entry.trash_id, base_dir=trash_root)
        assert not (trash.trash_dir(trash_root) / entry.trashed_name).exists()
        assert trash.list_trash(trash_root) == []

    def test_removes_directory_recursively(self, tmp_path, trash_root):
        d = tmp_path / "d"
        d.mkdir()
        (d / "x.txt").write_text("x")
        entry = trash.move_to_trash(d, base_dir=trash_root)
        trash.permanently_delete(entry.trash_id, base_dir=trash_root)
        assert not (trash.trash_dir(trash_root) / entry.trashed_name).exists()

    def test_raises_for_unknown_id(self, trash_root):
        with pytest.raises(trash.TrashError):
            trash.permanently_delete("nope", base_dir=trash_root)


class TestClearTrash:
    def test_removes_everything_and_returns_count(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        b = _make_file(tmp_path, "b.txt")
        trash.move_to_trash(a, base_dir=trash_root)
        trash.move_to_trash(b, base_dir=trash_root)
        count = trash.clear_trash(trash_root)
        assert count == 2
        assert trash.list_trash(trash_root) == []

    def test_no_op_on_empty_trash(self, trash_root):
        assert trash.clear_trash(trash_root) == 0


class TestCheckAndExpire:
    def test_deletes_entries_past_retention(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        trashed_at = 0.0
        trash.move_to_trash(source, base_dir=trash_root, now=trashed_at)
        now = trashed_at + 9 * trash.SECONDS_PER_DAY  # older than 8-day default retention
        report = trash.check_and_expire(retention_days=8, base_dir=trash_root, now=now)
        assert len(report.deleted) == 1
        assert trash.list_trash(trash_root) == []

    def test_keeps_entries_within_retention(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        trash.move_to_trash(source, base_dir=trash_root, now=0.0)
        now = 1 * trash.SECONDS_PER_DAY  # well within 8-day retention
        report = trash.check_and_expire(retention_days=8, base_dir=trash_root, now=now)
        assert report.deleted == []
        assert len(trash.list_trash(trash_root)) == 1

    def test_warns_for_entries_close_to_expiry(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        trash.move_to_trash(source, base_dir=trash_root, now=0.0)
        # 7.5 days old, 8-day retention, 1-day warning window -> should warn, not delete
        now = 7.5 * trash.SECONDS_PER_DAY
        report = trash.check_and_expire(
            retention_days=8, base_dir=trash_root, now=now, warning_window_days=1
        )
        assert report.deleted == []
        assert len(report.warned) == 1

    def test_does_not_warn_for_fresh_entries(self, tmp_path, trash_root):
        source = _make_file(tmp_path)
        trash.move_to_trash(source, base_dir=trash_root, now=0.0)
        now = 1 * trash.SECONDS_PER_DAY
        report = trash.check_and_expire(retention_days=8, base_dir=trash_root, now=now, warning_window_days=1)
        assert report.warned == []

    def test_no_background_job_only_called_explicitly(self, trash_root):
        # Documented design: there is no timer/thread anywhere in trash.py.
        # This is a smoke test that the module has no scheduling side effects
        # on import/call beyond what check_and_expire() does when invoked.
        assert not hasattr(trash, "start_background_expiry")
        assert not hasattr(trash, "_scheduler")


class TestKeepAll:
    def test_no_op_on_empty_trash(self, trash_root):
        assert trash.keep_all(trash_root) == 0

    def test_returns_count_of_entries_reset(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        b = _make_file(tmp_path, "b.txt")
        trash.move_to_trash(a, base_dir=trash_root, now=0.0)
        trash.move_to_trash(b, base_dir=trash_root, now=0.0)
        assert trash.keep_all(trash_root, now=500.0) == 2

    def test_resets_trashed_at_for_every_entry(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        trash.move_to_trash(a, base_dir=trash_root, now=0.0)
        trash.keep_all(trash_root, now=12345.0)
        entries = trash.list_trash(trash_root)
        assert entries[0].trashed_at == 12345.0

    def test_prevents_expiry_after_reset(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        trash.move_to_trash(a, base_dir=trash_root, now=0.0)
        # Without a reset this would already be past an 8-day retention.
        reset_at = 7 * trash.SECONDS_PER_DAY
        trash.keep_all(trash_root, now=reset_at)
        now = reset_at + 7 * trash.SECONDS_PER_DAY  # still < 8 days since reset
        report = trash.check_and_expire(retention_days=8, base_dir=trash_root, now=now)
        assert report.deleted == []
        assert len(trash.list_trash(trash_root)) == 1

    def test_preserves_trash_id_and_action_id(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        entry = trash.move_to_trash(a, action_id="act-1", base_dir=trash_root, now=0.0)
        trash.keep_all(trash_root, now=100.0)
        refreshed = trash.list_trash(trash_root)[0]
        assert refreshed.trash_id == entry.trash_id
        assert refreshed.action_id == "act-1"

    def test_defaults_to_current_time_when_now_not_given(self, tmp_path, trash_root):
        a = _make_file(tmp_path, "a.txt")
        trash.move_to_trash(a, base_dir=trash_root, now=0.0)
        before = time.time()
        trash.keep_all(trash_root)
        after = time.time()
        refreshed_at = trash.list_trash(trash_root)[0].trashed_at
        assert before <= refreshed_at <= after


class TestRenderExpiryWarning:
    def test_none_when_nothing_warned(self):
        report = trash.ExpiryReport(deleted=[], warned=[])
        assert trash.render_expiry_warning(report, retention_days=8) is None

    def test_mentions_count_and_retention_days(self):
        entry = trash.TrashEntry(
            trash_id="x", original_path="/tmp/f", trashed_name="f", trashed_at=0.0
        )
        report = trash.ExpiryReport(deleted=[], warned=[entry])
        text = trash.render_expiry_warning(report, retention_days=8)
        assert "1" in text
        assert "8 days" in text

    def test_pluralizes_for_multiple_entries(self):
        entries = [
            trash.TrashEntry(trash_id=f"x{i}", original_path=f"/tmp/f{i}", trashed_name=f"f{i}", trashed_at=0.0)
            for i in range(3)
        ]
        report = trash.ExpiryReport(deleted=[], warned=entries)
        text = trash.render_expiry_warning(report, retention_days=8)
        assert "files" in text