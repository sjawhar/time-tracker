from click.testing import CliRunner

from tt.cli import cli


def test_cli_help() -> None:
    result = CliRunner().invoke(cli, ["--help"])
    assert result.exit_code == 0
    assert "Time Tracker CLI" in result.output
