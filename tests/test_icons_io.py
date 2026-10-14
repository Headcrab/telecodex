"""Shutdown regressions using real child processes, with no external services."""
import select
import signal
import subprocess
import sys
import unittest

from icons_io import stop_bridge


class BridgeShutdownTests(unittest.TestCase):
    """Verify graceful shutdown, forced reaping and fixture-token checks."""

    @staticmethod
    def reap(process):
        """Ensure a failed assertion cannot leave the fixture process running."""
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)

    def child(self, *, ignore_sigint, error=''):
        """Start a ready child that either handles or ignores SIGINT."""
        code = f'''
import signal
import sys
import time
signal.signal(signal.SIGINT, {'signal.SIG_IGN' if ignore_sigint else 'lambda *_: sys.exit(0)'})
print({error!r}, file=sys.stderr, flush=True)
print('ready', flush=True)
while True:
    time.sleep(1)
'''
        process = subprocess.Popen([sys.executable, '-c', code], stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.reap, process)
        ready, _, _ = select.select([process.stdout], [], [], 5)
        self.assertTrue(ready, 'fixture did not become ready')
        self.assertEqual(process.stdout.readline().strip(), 'ready')
        return process

    def test_graceful_shutdown(self):
        """A child that handles SIGINT exits normally and is reaped."""
        process = self.child(ignore_sigint=False)
        stop_bridge(process, timeout=1)
        self.assertEqual(process.returncode, 0)

    def test_timeout_kills_and_reaps_child(self):
        """A child ignoring SIGINT is killed, reaped and still fails the test."""
        process = self.child(ignore_sigint=True)
        with self.assertRaises(subprocess.TimeoutExpired):
            stop_bridge(process, timeout=.1)
        self.assertEqual(process.returncode, -signal.SIGKILL)

    def test_forced_shutdown_checks_stderr(self):
        """Token leakage is detected even when shutdown needs the kill fallback."""
        process = self.child(ignore_sigint=True, error='synthetic-icon-token')
        with self.assertRaises(AssertionError):
            stop_bridge(process, timeout=.1)
        self.assertEqual(process.returncode, -signal.SIGKILL)


if __name__ == '__main__':
    unittest.main()
