"""Tests for the Audit Log (Build Order Step 12)."""

from __future__ import annotations

import json

import pytest

from ohmyshell import audit_log


@pytest.fixture
def log_root(tmp_path):
    return tmp_path / "oh-my-shell-test"


class TestAuditEntryValidation:
    def test_valid_status_constructs_fine(self):
        entry = audit_log.AuditEntry(
            timestamp=0.0, action="clean_temp_files", source="natural_language",
            params={}, risk="medium", status="done",
        )
        assert entry.status == "done"

    def test_invalid_status_raises(self):
        with pytest.raises(ValueError):
            audit_log.AuditEntry(
                timestamp=0.0, action="x", source="natural_language",
                params={}, risk="low", status="bogus",
            )

    @pytest.mark.parametrize("status", audit_log.VALID_STATUSES)
    def test_all_documented_statuses_are_valid(self, status):
        entry = audit_log.AuditEntry(
            timestamp=0.0, action="x", source="raw_shell", params={}, risk=None, status=status,
        )
        assert entry.status == status


class TestRecordAndRead:
    def test_record_then_read_round_trips(self, log_root):
        entry = audit_log.record_action(
            action="clean_temp_files", source="natural_language",
            params={"days": 7}, risk="medium", status="done", base_dir=log_root,
        )
        entries = audit_log.read_entries(log_root)
        assert len(entries) == 1
        assert entries[0] == entry

    def test_multiple_entries_appended_in_order(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root, now=1.0)
        audit_log.record_action(action="b", source="raw_shell", status="failed", base_dir=log_root, now=2.0)
        entries = audit_log.read_entries(log_root)
        assert [e.action for e in entries] == ["a", "b"]

    def test_empty_log_returns_empty_list(self, log_root):
        assert audit_log.read_entries(log_root) == []

    def test_creates_parent_directory(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root)
        assert audit_log.audit_log_path(log_root).exists()

    def test_file_is_valid_jsonl(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root)
        audit_log.record_action(action="b", source="raw_shell", status="done", base_dir=log_root)
        lines = audit_log.audit_log_path(log_root).read_text().strip().splitlines()
        assert len(lines) == 2
        for line in lines:
            json.loads(line)  # must not raise

    def test_default_params_is_empty_dict(self, log_root):
        entry = audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root)
        assert entry.params == {}

    def test_records_used_sudo_and_error_fields(self, log_root):
        entry = audit_log.record_action(
            action="clean_temp_files", source="natural_language", status="failed",
            used_sudo=True, error="Permission denied", base_dir=log_root,
        )
        read_back = audit_log.read_entries(log_root)[0]
        assert read_back.used_sudo is True
        assert read_back.error == "Permission denied"

    def test_raises_audit_log_error_on_corrupted_line(self, log_root):
        path = audit_log.audit_log_path(log_root)
        path.parent.mkdir(parents=True)
        path.write_text("{not valid json\n")
        with pytest.raises(audit_log.AuditLogError):
            audit_log.read_entries(log_root)

    def test_skips_blank_lines(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root)
        path = audit_log.audit_log_path(log_root)
        with path.open("a") as f:
            f.write("\n")
        entries = audit_log.read_entries(log_root)
        assert len(entries) == 1


class TestIterEntries:
    def test_yields_same_entries_as_read(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root)
        assert list(audit_log.iter_entries(log_root)) == audit_log.read_entries(log_root)


class TestMostRecent:
    def test_none_when_empty(self, log_root):
        assert audit_log.most_recent(log_root) is None

    def test_returns_last_recorded_entry(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root, now=1.0)
        audit_log.record_action(action="b", source="raw_shell", status="done", base_dir=log_root, now=2.0)
        assert audit_log.most_recent(log_root).action == "b"


class TestEntriesSince:
    def test_filters_by_timestamp(self, log_root):
        audit_log.record_action(action="old", source="raw_shell", status="done", base_dir=log_root, now=100.0)
        audit_log.record_action(action="new", source="raw_shell", status="done", base_dir=log_root, now=200.0)
        recent = audit_log.entries_since(150.0, base_dir=log_root)
        assert [e.action for e in recent] == ["new"]

    def test_boundary_is_inclusive(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root, now=100.0)
        assert len(audit_log.entries_since(100.0, base_dir=log_root)) == 1


class TestSummarizeSession:
    def test_counts_requests_and_completed(self, log_root):
        audit_log.record_action(action="a", source="raw_shell", status="done", base_dir=log_root, now=10.0)
        audit_log.record_action(action="b", source="raw_shell", status="failed", base_dir=log_root, now=11.0)
        audit_log.record_action(action="c", source="raw_shell", status="done", base_dir=log_root, now=12.0)
        summary = audit_log.summarize_session(5.0, base_dir=log_root)
        assert summary.requests_processed == 3
        assert summary.completed == 2

    def test_ignores_entries_before_session_start(self, log_root):
        audit_log.record_action(action="before", source="raw_shell", status="done", base_dir=log_root, now=1.0)
        audit_log.record_action(action="after", source="raw_shell", status="done", base_dir=log_root, now=100.0)
        summary = audit_log.summarize_session(50.0, base_dir=log_root)
        assert summary.requests_processed == 1

    def test_tokens_used_passed_through_when_given(self, log_root):
        summary = audit_log.summarize_session(0.0, base_dir=log_root, tokens_used=1240)
        assert summary.tokens_used == 1240

    def test_tokens_used_none_by_default(self, log_root):
        summary = audit_log.summarize_session(0.0, base_dir=log_root)
        assert summary.tokens_used is None


class TestRenderSessionSummary:
    def test_matches_blueprint_shape_without_tokens(self):
        summary = audit_log.SessionSummary(requests_processed=8, completed=6)
        text = audit_log.render_session_summary(summary)
        assert "8 requests processed" in text
        assert "6 completed" in text

    def test_includes_tokens_when_present(self):
        summary = audit_log.SessionSummary(requests_processed=8, completed=6, tokens_used=1240)
        text = audit_log.render_session_summary(summary)
        assert "1,240 tokens used" in text

    def test_omits_tokens_when_absent(self):
        summary = audit_log.SessionSummary(requests_processed=1, completed=1, tokens_used=None)
        text = audit_log.render_session_summary(summary)
        assert "tokens used" not in text