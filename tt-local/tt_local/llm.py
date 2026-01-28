"""LLM integration for tag suggestions."""

from __future__ import annotations

import json
import os
from collections import Counter
from datetime import datetime
from typing import Any


SUGGEST_TAGS_TOOL = {
    "name": "suggest_tags",
    "description": "Suggest 1-3 tags for a work stream based on its activity",
    "input_schema": {
        "type": "object",
        "properties": {
            "tags": {
                "type": "array",
                "items": {"type": "string"},
                "minItems": 1,
                "maxItems": 3,
                "description": "Lowercase hyphenated tags (e.g., 'bug-fix', 'code-review')",
            }
        },
        "required": ["tags"],
    },
}

SYSTEM_PROMPT = """You suggest tags for time tracking work streams. Tags should be:
- Lowercase with hyphens (e.g., "bug-fix", "code-review", "feature-work")
- Short and descriptive (1-3 words)
- Reuse existing tags when they fit the work being done

Call the suggest_tags tool with 1-3 tag suggestions."""


class LlmError(Exception):
    """Base exception for LLM errors."""

    pass


class ApiKeyNotSetError(LlmError):
    """Raised when ANTHROPIC_API_KEY is not set."""

    def __init__(self) -> None:
        super().__init__("ANTHROPIC_API_KEY not set. Cannot suggest tags.")


class ApiError(LlmError):
    """Raised when the API call fails."""

    pass


def _truncate_path(path: str, segments: int = 3) -> str:
    """Truncate a path to the last N segments.

    Args:
        path: Full path like "/home/user/projects/foo"
        segments: Number of trailing segments to keep

    Returns:
        Truncated path like "projects/foo"
    """
    if not path:
        return ""
    parts = path.rstrip("/").split("/")
    if len(parts) <= segments:
        return path
    return "/".join(parts[-segments:])


def _format_duration_short(ms: int) -> str:
    """Format milliseconds as 'Xh Ym' or 'Ym'."""
    if ms < 60_000:
        return "<1m" if ms > 0 else "0m"
    total_minutes = ms // 60_000
    hours = total_minutes // 60
    minutes = total_minutes % 60
    if hours > 0:
        return f"{hours}h {minutes}m"
    return f"{minutes}m"


def _build_stream_context(
    stream_name: str,
    events: list[dict[str, Any]],
    existing_tags: list[str],
) -> str:
    """Build context string for the LLM prompt.

    Args:
        stream_name: Name of the stream (usually directory basename)
        events: List of event dicts for this stream
        existing_tags: Tags currently in use in the system

    Returns:
        Formatted context string
    """
    # Extract directory from first event with cwd
    directory = ""
    for event in events:
        if event.get("cwd"):
            directory = _truncate_path(event["cwd"])
            break

    # Count event types
    type_counts: Counter[str] = Counter()
    tool_types: Counter[str] = Counter()
    session_ids: set[str] = set()

    for event in events:
        event_type = event.get("type", "unknown")
        type_counts[event_type] += 1

        # Track tool types for agent_tool_use events
        if event_type == "agent_tool_use":
            data = event.get("data")
            if isinstance(data, str):
                try:
                    data = json.loads(data)
                except json.JSONDecodeError:
                    data = {}
            if isinstance(data, dict) and data.get("tool"):
                tool_types[data["tool"]] += 1

        # Track session IDs
        if event.get("session_id"):
            session_ids.add(event["session_id"])

    # Calculate time span
    if events:
        first_ts = events[0].get("timestamp", "")
        last_ts = events[-1].get("timestamp", "")
        try:
            first_dt = datetime.fromisoformat(first_ts.replace("Z", "+00:00"))
            last_dt = datetime.fromisoformat(last_ts.replace("Z", "+00:00"))
            duration_ms = int((last_dt - first_dt).total_seconds() * 1000)
            time_range = f"{first_dt.strftime('%b %d, %H:%M')} - {last_dt.strftime('%H:%M')}"
        except (ValueError, AttributeError):
            duration_ms = 0
            time_range = "unknown"
    else:
        duration_ms = 0
        time_range = "no events"

    # Format event summary
    event_parts = []
    for event_type, count in type_counts.most_common(5):
        # Make event types more readable
        readable = event_type.replace("_", " ")
        event_parts.append(f"{count} {readable}")

    # Add tool types if present
    if tool_types:
        top_tools = [tool for tool, _ in tool_types.most_common(3)]
        event_parts.append(f"tools: {', '.join(top_tools)}")

    events_str = ", ".join(event_parts) if event_parts else "no events"

    # Format existing tags
    tags_str = ", ".join(existing_tags) if existing_tags else "(none)"

    return f"""Stream: "{stream_name}" (in {directory or 'unknown directory'})
Events: {events_str}
Duration: {_format_duration_short(duration_ms)} ({time_range})
Agent sessions: {len(session_ids)}

Existing tags: {tags_str}"""


class TagSuggester:
    """Suggests tags for streams using Claude API."""

    def __init__(self, api_key: str | None = None) -> None:
        """Initialize the tag suggester.

        Args:
            api_key: Anthropic API key. If not provided, reads from ANTHROPIC_API_KEY.

        Raises:
            ApiKeyNotSetError: If no API key is available.
        """
        self._api_key = api_key or os.environ.get("ANTHROPIC_API_KEY")
        if not self._api_key:
            raise ApiKeyNotSetError()
        self._client: Any = None

    def _get_client(self) -> Any:
        """Lazily initialize the Anthropic client."""
        if self._client is None:
            try:
                import anthropic
            except ImportError as e:
                raise LlmError(
                    "anthropic package not installed. Run: uv add anthropic"
                ) from e
            self._client = anthropic.Anthropic(api_key=self._api_key)
        return self._client

    def suggest_tags(
        self,
        stream_name: str,
        events: list[dict[str, Any]],
        existing_tags: list[str],
    ) -> list[str]:
        """Suggest tags for a stream based on its events.

        Args:
            stream_name: Stream name (usually directory basename)
            events: List of event dicts for context
            existing_tags: Top tags currently in use (for consistency)

        Returns:
            List of 1-3 suggested tags

        Raises:
            ApiError: If the API call fails
        """
        client = self._get_client()
        context = _build_stream_context(stream_name, events, existing_tags)

        try:
            response = client.messages.create(
                model="claude-3-5-haiku-latest",
                max_tokens=200,
                system=SYSTEM_PROMPT,
                tools=[SUGGEST_TAGS_TOOL],
                tool_choice={"type": "tool", "name": "suggest_tags"},
                messages=[{"role": "user", "content": context}],
            )
        except Exception as e:
            raise ApiError(f"API call failed: {e}") from e

        # Extract tags from tool use response
        for block in response.content:
            if block.type == "tool_use" and block.name == "suggest_tags":
                # Defensive check for malformed API response
                if not hasattr(block, "input") or not isinstance(block.input, dict):
                    continue
                tags = block.input.get("tags", [])
                if isinstance(tags, list):
                    # Normalize tags: lowercase, hyphenated, no special chars
                    normalized = []
                    for tag in tags[:3]:
                        tag_str = str(tag).lower().strip()
                        # Replace spaces with hyphens, remove other special chars
                        tag_str = tag_str.replace(" ", "-")
                        # Keep only alphanumeric and hyphens, truncate
                        tag_str = "".join(
                            c for c in tag_str if c.isalnum() or c == "-"
                        )[:50]
                        if tag_str:
                            normalized.append(tag_str)
                    return normalized

        # Fallback if no tool use found
        return []
