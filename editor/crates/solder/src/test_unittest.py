"""The unittest runner's own objects give stable ids, locations and failures."""
import contextlib
import inspect
import json
import os
import sys
import unittest

wire = sys.stdout


def send(value):
    wire.write("solder-test:" + json.dumps(value) + "\n")
    wire.flush()


def leaves(suite):
    for test in suite:
        if isinstance(test, unittest.TestSuite):
            yield from leaves(test)
        else:
            yield test


class Result(unittest.TextTestResult):
    def addSuccess(self, test):
        super().addSuccess(test)
        send({"id": test.id(), "state": "passed"})

    def addFailure(self, test, err):
        super().addFailure(test, err)
        send({"id": test.id(), "state": "failed", "detail": self._exc_info_to_string(err, test)})

    def addError(self, test, err):
        super().addError(test, err)
        send({"id": test.id(), "state": "failed", "detail": self._exc_info_to_string(err, test)})

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        send({"id": test.id(), "state": "skipped", "detail": reason})

    def addExpectedFailure(self, test, err):
        super().addExpectedFailure(test, err)
        send({"id": test.id(), "state": "skipped", "detail": "Expected failure"})

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        send({"id": test.id(), "state": "failed", "detail": "Unexpected success"})


with contextlib.redirect_stdout(sys.stderr):
    loader = unittest.TestLoader()
    suite = loader.discover(".")
    if sys.argv[1] == "list":
        for test in leaves(suite):
            method = getattr(test, getattr(test, "_testMethodName", ""), None)
            try:
                path = inspect.getsourcefile(method)
                line = inspect.getsourcelines(method)[1]
            except (TypeError, OSError):
                path, line = None, 1
            send({"id": test.id(), "path": os.path.abspath(path) if path else None, "line": line})
        if loader.errors:
            raise RuntimeError("\n".join(loader.errors))
    else:
        selected = unittest.TestSuite(test for test in leaves(suite) if test.id() in sys.argv[2:])
        if selected.countTestCases() != len(set(sys.argv[2:])):
            raise RuntimeError("The selected tests changed. Discover tests again.")
        result = unittest.TextTestRunner(stream=sys.stderr, resultclass=Result).run(selected)
        sys.exit(0 if result.wasSuccessful() else 1)
