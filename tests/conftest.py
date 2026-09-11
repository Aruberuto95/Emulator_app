"""Make the implementation under test visible in every pytest run."""
import os
from emulator_harness import resolve_binary


def pytest_report_header(config):
    backend = os.environ.get("EMULATOR_TEST_BACKEND", "native")
    try:
        binary = resolve_binary()
    except RuntimeError as error:
        binary = str(error)
    return f"Emulator backend: {backend}; target: {binary}"
