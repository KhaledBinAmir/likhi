"""A second engine must never share a port with the first.

On Windows SO_REUSEADDR allows two processes to listen on the same port, which would split the
keyboard's requests between two engines holding different personal dictionaries. Autostart is per
user, so two people signed in at once is a realistic way to hit this.
"""

import socket
import subprocess
import sys
import time

import pytest

from likhi.server import already_running


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def test_already_running_is_false_for_a_dead_port():
    assert already_running(_free_port()) is False


def test_already_running_is_false_for_a_port_that_is_not_likhi():
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]
    try:
        assert already_running(port) is False  # accepts, never answers a ping
    finally:
        srv.close()


def test_second_instance_exits_instead_of_stealing_the_port():
    port = _free_port()
    first = subprocess.Popen(
        [sys.executable, "-m", "likhi.server", "--port", str(port)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        for _ in range(600):
            if already_running(port):
                break
            if first.poll() is not None:
                pytest.skip("engine could not start (models missing?)")
            time.sleep(0.1)
        else:
            pytest.skip("engine did not become ready in time")

        second = subprocess.run(
            [sys.executable, "-m", "likhi.server", "--port", str(port)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        assert "already listening" in second.stdout
        assert second.returncode == 0  # a clean no-op, not a crash
        assert already_running(port)  # the first one is untouched
    finally:
        first.terminate()
        first.wait(timeout=30)
