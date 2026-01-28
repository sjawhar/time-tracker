"""Tests for tag management functionality."""

from __future__ import annotations

import pytest
from click.testing import CliRunner

from tt_local.cli import main
from tt_local.db import EventStore


@pytest.fixture
def db_with_streams(tmp_path):
    """Create a database with some streams and events."""
    db_path = tmp_path / "test.db"
    with EventStore.open(db_path) as store:
        # Create streams
        stream1 = store.create_stream(name="project-alpha")
        stream2 = store.create_stream(name="project-beta")
        stream3 = store.create_stream(name="untagged-stream")

        # Add some tags to first two streams
        store.add_tag(stream1, "work")
        store.add_tag(stream1, "important")
        store.add_tag(stream2, "personal")

        # Return paths for use in tests
        return {
            "db_path": db_path,
            "stream1": stream1,
            "stream2": stream2,
            "stream3": stream3,
        }


class TestDatabaseTagMethods:
    """Tests for EventStore tag methods."""

    def test_add_tag_success(self, tmp_path):
        """Test adding a tag to a stream."""
        db_path = tmp_path / "test.db"
        with EventStore.open(db_path) as store:
            stream_id = store.create_stream(name="test")
            result = store.add_tag(stream_id, "my-tag")
            assert result is True
            tags = store.get_stream_tags([stream_id])
            assert tags[stream_id] == ["my-tag"]

    def test_add_tag_duplicate(self, tmp_path):
        """Test adding a duplicate tag returns False."""
        db_path = tmp_path / "test.db"
        with EventStore.open(db_path) as store:
            stream_id = store.create_stream(name="test")
            store.add_tag(stream_id, "my-tag")
            result = store.add_tag(stream_id, "my-tag")
            assert result is False

    def test_remove_tag_success(self, tmp_path):
        """Test removing an existing tag."""
        db_path = tmp_path / "test.db"
        with EventStore.open(db_path) as store:
            stream_id = store.create_stream(name="test")
            store.add_tag(stream_id, "my-tag")
            result = store.remove_tag(stream_id, "my-tag")
            assert result is True
            tags = store.get_stream_tags([stream_id])
            assert stream_id not in tags or tags[stream_id] == []

    def test_remove_tag_nonexistent(self, tmp_path):
        """Test removing a non-existent tag returns False."""
        db_path = tmp_path / "test.db"
        with EventStore.open(db_path) as store:
            stream_id = store.create_stream(name="test")
            result = store.remove_tag(stream_id, "nonexistent")
            assert result is False

    def test_get_top_tags(self, db_with_streams):
        """Test getting most-used tags."""
        with EventStore.open(db_with_streams["db_path"]) as store:
            # Add more tags to increase counts
            store.add_tag(db_with_streams["stream2"], "work")  # work: 2 streams

            top_tags = store.get_top_tags(limit=10)
            # work should be first (2 streams), others have 1 each
            assert top_tags[0] == ("work", 2)
            assert len(top_tags) == 3  # work, important, personal

    def test_get_untagged_streams(self, db_with_streams):
        """Test getting streams without tags."""
        with EventStore.open(db_with_streams["db_path"]) as store:
            untagged = store.get_untagged_streams()
            assert len(untagged) == 1
            assert untagged[0]["id"] == db_with_streams["stream3"]

    def test_get_stream_by_prefix_success(self, db_with_streams):
        """Test finding stream by unique prefix."""
        with EventStore.open(db_with_streams["db_path"]) as store:
            stream_id = db_with_streams["stream1"]
            # Use first 7 characters as prefix
            prefix = stream_id[:7]
            result = store.get_stream_by_prefix(prefix)
            assert result is not None
            assert result["id"] == stream_id

    def test_get_stream_by_prefix_not_found(self, db_with_streams):
        """Test getting non-existent stream returns None."""
        with EventStore.open(db_with_streams["db_path"]) as store:
            result = store.get_stream_by_prefix("zzzzzzz")
            assert result is None

    def test_get_stream_by_prefix_ambiguous(self, tmp_path):
        """Test ambiguous prefix raises ValueError."""
        db_path = tmp_path / "test.db"
        with EventStore.open(db_path) as store:
            # Create streams with known IDs
            store._conn.execute(
                "INSERT INTO streams (id, created_at, updated_at, name) VALUES (?, ?, ?, ?)",
                ("abc123-one", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z", "one"),
            )
            store._conn.execute(
                "INSERT INTO streams (id, created_at, updated_at, name) VALUES (?, ?, ?, ?)",
                ("abc456-two", "2025-01-01T00:00:00Z", "2025-01-01T00:00:00Z", "two"),
            )
            store._conn.commit()

            with pytest.raises(ValueError, match="Ambiguous"):
                store.get_stream_by_prefix("abc")


class TestTagAddCommand:
    """Tests for `tt tag add` CLI command."""

    def test_add_tag_to_stream(self, db_with_streams):
        """Test adding a tag via CLI."""
        runner = CliRunner()
        stream_id = db_with_streams["stream3"][:7]  # Untagged stream
        result = runner.invoke(
            main,
            ["tag", "add", stream_id, "new-tag", "--db", str(db_with_streams["db_path"])],
        )
        assert result.exit_code == 0
        assert "Tagged stream" in result.output
        assert "new-tag" in result.output

    def test_add_duplicate_tag(self, db_with_streams):
        """Test adding a tag that already exists."""
        runner = CliRunner()
        stream_id = db_with_streams["stream1"][:7]  # Has "work" tag
        result = runner.invoke(
            main,
            ["tag", "add", stream_id, "work", "--db", str(db_with_streams["db_path"])],
        )
        assert result.exit_code == 0
        assert "already has tag" in result.output

    def test_add_tag_nonexistent_stream(self, db_with_streams):
        """Test adding tag to non-existent stream."""
        runner = CliRunner()
        result = runner.invoke(
            main,
            ["tag", "add", "zzzzzzz", "some-tag", "--db", str(db_with_streams["db_path"])],
        )
        assert result.exit_code == 1
        assert "No stream found" in result.output


class TestTagRemoveCommand:
    """Tests for `tt tag remove` CLI command."""

    def test_remove_tag_from_stream(self, db_with_streams):
        """Test removing a tag via CLI."""
        runner = CliRunner()
        stream_id = db_with_streams["stream1"][:7]
        result = runner.invoke(
            main,
            ["tag", "remove", stream_id, "work", "--db", str(db_with_streams["db_path"])],
        )
        assert result.exit_code == 0
        assert "Removed tag" in result.output

    def test_remove_nonexistent_tag(self, db_with_streams):
        """Test removing a tag that doesn't exist."""
        runner = CliRunner()
        stream_id = db_with_streams["stream1"][:7]
        result = runner.invoke(
            main,
            ["tag", "remove", stream_id, "nonexistent", "--db", str(db_with_streams["db_path"])],
        )
        assert result.exit_code == 0
        assert "doesn't have tag" in result.output
