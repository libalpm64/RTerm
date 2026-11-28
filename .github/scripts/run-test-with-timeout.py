"""Cargo test runner: fail a hung native test executable on any CI OS."""
import subprocess
import sys

# Regular tests finish in seconds; opt-in benchmarks are not run in CI.
TIMEOUT_SECONDS = 120


def main():
    if len(sys.argv) < 2:
        print("Missing test executable", file=sys.stderr)
        return 2
    try:
        return subprocess.run(sys.argv[1:], timeout=TIMEOUT_SECONDS).returncode
    except subprocess.TimeoutExpired:
        print(
            f"::error::Test executable {sys.argv[1]} exceeded "
            f"{TIMEOUT_SECONDS} seconds; terminated",
            file=sys.stderr,
            flush=True,
        )
        return 124


if __name__ == "__main__":
    sys.exit(main())
