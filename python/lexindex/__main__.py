"""The ``lexindex`` command line, for an install that has no ``cargo``.

``pip install lexindex`` puts it on the environment's path as ``lexindex``, and
``python -m lexindex`` runs it where that path is not on the shell's. The program is the crate's
own binary compiled into this extension, so the subcommands, their output and their exit statuses
are the ones ``cargo install lexindex`` gives. ``lexindex --help`` lists them.
"""

import signal
import sys

from lexindex._core import _cli


def main() -> int:
    """Run the command line on ``sys.argv`` and return its exit status."""
    # The program runs in Rust and does not come back to the interpreter until it is done, so the
    # KeyboardInterrupt Python would raise on Ctrl-C never could be; the signal's default action
    # ends the process, as it ends the binary.
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    return _cli(sys.argv[1:])


if __name__ == "__main__":
    sys.exit(main())
