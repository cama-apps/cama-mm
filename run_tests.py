"""
Simple test runner script.
Run with: python run_tests.py

Uses pytest discovery to find all test files:
- Root level test files: test_*.py
- Tests directory: tests/test_*.py
"""

import subprocess
import sys

if __name__ == "__main__":
    # Run pytest with verbose output, discovering all test files
    # pytest will automatically find:
    # - test_*.py files in the root directory
    # - test_*.py files in the tests/ directory
    result = subprocess.run(
        [
            sys.executable, "-m", "pytest", 
            "-v",              # Verbose output
            "--tb=short",      # Short traceback format
            ".",               # Search from current directory (finds root tests)
            "tests/",          # Also search tests directory
        ],
        cwd="."
    )
    sys.exit(result.returncode)
