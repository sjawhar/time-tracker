import click


@click.group(context_settings={"help_option_names": ["-h", "--help"]})
def cli() -> None:
    """Time Tracker CLI."""


def main() -> None:
    cli()


if __name__ == "__main__":
    main()
