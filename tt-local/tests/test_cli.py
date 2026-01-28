"""Tests for the CLI entry point."""

from click.testing import CliRunner

from tt_local.cli import main


def test_main_help():
    """Test that --help works and shows the group description."""
    runner = CliRunner()
    result = runner.invoke(main, ["--help"])
    assert result.exit_code == 0
    assert "Time Tracker local CLI" in result.output


def test_main_no_args():
    """Test that running with no args shows usage error (click group default)."""
    runner = CliRunner()
    result = runner.invoke(main, [])
    # Click groups exit with code 2 when no subcommand is provided
    assert result.exit_code == 2
    assert "Usage:" in result.output


def test_import_cli():
    """Test that the CLI module can be imported."""
    from tt_local import cli
    assert hasattr(cli, "main")
