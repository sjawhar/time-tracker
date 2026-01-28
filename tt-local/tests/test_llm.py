"""Tests for LLM tag suggestion functionality."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from unittest.mock import MagicMock, patch

import pytest

from tt_local.llm import (
    ApiKeyNotSetError,
    ApiError,
    TagSuggester,
    _build_stream_context,
    _format_duration_short,
    _truncate_path,
)


class TestTruncatePath:
    """Tests for _truncate_path helper."""

    def test_short_path_unchanged(self):
        """Short paths are returned as-is."""
        assert _truncate_path("/foo/bar") == "/foo/bar"

    def test_long_path_truncated(self):
        """Long paths are truncated to last N segments."""
        result = _truncate_path("/home/user/projects/myapp/src")
        assert result == "projects/myapp/src"

    def test_empty_path(self):
        """Empty path returns empty string."""
        assert _truncate_path("") == ""

    def test_custom_segments(self):
        """Can specify number of segments to keep."""
        result = _truncate_path("/a/b/c/d/e", segments=2)
        assert result == "d/e"


class TestFormatDurationShort:
    """Tests for _format_duration_short helper."""

    def test_zero_ms(self):
        assert _format_duration_short(0) == "0m"

    def test_sub_minute(self):
        assert _format_duration_short(30_000) == "<1m"

    def test_minutes_only(self):
        assert _format_duration_short(300_000) == "5m"

    def test_hours_and_minutes(self):
        assert _format_duration_short(5_400_000) == "1h 30m"


class TestBuildStreamContext:
    """Tests for _build_stream_context."""

    def test_basic_context(self):
        """Test basic context generation."""
        events = [
            {
                "type": "tmux_pane_focus",
                "cwd": "/home/user/projects/myapp",
                "timestamp": "2025-01-28T10:00:00Z",
            },
            {
                "type": "tmux_pane_focus",
                "cwd": "/home/user/projects/myapp",
                "timestamp": "2025-01-28T11:00:00Z",
            },
        ]
        context = _build_stream_context("myapp", events, ["work", "personal"])

        assert 'Stream: "myapp"' in context
        assert "projects/myapp" in context  # Truncated path
        assert "2 tmux pane focus" in context
        assert "1h 0m" in context
        assert "work, personal" in context

    def test_context_with_agent_events(self):
        """Test context with agent tool use events."""
        events = [
            {
                "type": "agent_tool_use",
                "data": json.dumps({"tool": "Edit"}),
                "timestamp": "2025-01-28T10:00:00Z",
                "session_id": "session-1",
            },
            {
                "type": "agent_tool_use",
                "data": json.dumps({"tool": "Read"}),
                "timestamp": "2025-01-28T10:01:00Z",
                "session_id": "session-1",
            },
        ]
        context = _build_stream_context("myapp", events, [])

        assert "2 agent tool use" in context
        assert "tools: Edit, Read" in context
        assert "Agent sessions: 1" in context

    def test_context_no_events(self):
        """Test context with empty events list."""
        context = _build_stream_context("empty", [], [])

        assert 'Stream: "empty"' in context
        assert "no events" in context


class TestTagSuggester:
    """Tests for TagSuggester class."""

    def test_init_requires_api_key(self):
        """Test that missing API key raises error."""
        with patch.dict("os.environ", {}, clear=True):
            with pytest.raises(ApiKeyNotSetError):
                TagSuggester()

    def test_init_with_explicit_key(self):
        """Test initialization with explicit API key."""
        suggester = TagSuggester(api_key="test-key")
        assert suggester._api_key == "test-key"

    def test_init_with_env_key(self):
        """Test initialization with environment variable."""
        with patch.dict("os.environ", {"ANTHROPIC_API_KEY": "env-key"}):
            suggester = TagSuggester()
            assert suggester._api_key == "env-key"

    def test_suggest_tags_success(self):
        """Test successful tag suggestion."""
        suggester = TagSuggester(api_key="test-key")

        # Mock the anthropic client
        mock_client = MagicMock()
        mock_response = MagicMock()
        mock_block = MagicMock()
        mock_block.type = "tool_use"
        mock_block.name = "suggest_tags"
        mock_block.input = {"tags": ["feature-work", "refactoring"]}
        mock_response.content = [mock_block]
        mock_client.messages.create.return_value = mock_response

        suggester._client = mock_client

        events = [{"type": "tmux_pane_focus", "timestamp": "2025-01-28T10:00:00Z"}]
        tags = suggester.suggest_tags("myapp", events, ["work"])

        assert tags == ["feature-work", "refactoring"]
        mock_client.messages.create.assert_called_once()

    def test_suggest_tags_api_error(self):
        """Test handling of API errors."""
        suggester = TagSuggester(api_key="test-key")

        mock_client = MagicMock()
        mock_client.messages.create.side_effect = Exception("API failed")
        suggester._client = mock_client

        events = [{"type": "tmux_pane_focus", "timestamp": "2025-01-28T10:00:00Z"}]

        with pytest.raises(ApiError, match="API call failed"):
            suggester.suggest_tags("myapp", events, [])

    def test_suggest_tags_empty_response(self):
        """Test handling of empty tool response."""
        suggester = TagSuggester(api_key="test-key")

        mock_client = MagicMock()
        mock_response = MagicMock()
        mock_response.content = []  # No tool use blocks
        mock_client.messages.create.return_value = mock_response

        suggester._client = mock_client

        events = [{"type": "tmux_pane_focus", "timestamp": "2025-01-28T10:00:00Z"}]
        tags = suggester.suggest_tags("myapp", events, [])

        assert tags == []

    def test_suggest_tags_limits_to_three(self):
        """Test that tags are limited to 3."""
        suggester = TagSuggester(api_key="test-key")

        mock_client = MagicMock()
        mock_response = MagicMock()
        mock_block = MagicMock()
        mock_block.type = "tool_use"
        mock_block.name = "suggest_tags"
        mock_block.input = {"tags": ["a", "b", "c", "d", "e"]}  # More than 3
        mock_response.content = [mock_block]
        mock_client.messages.create.return_value = mock_response

        suggester._client = mock_client

        events = [{"type": "tmux_pane_focus", "timestamp": "2025-01-28T10:00:00Z"}]
        tags = suggester.suggest_tags("myapp", events, [])

        assert len(tags) == 3
        assert tags == ["a", "b", "c"]
