"""Shared fixtures for the end-to-end and contract tests.

`dorman` starts the real binaries against a throwaway Postgres database:
it migrates, loads the example batch, publishes DAY, and runs `dorman-api`
(built with the `test-clock` feature) on a free port. `dorman.set_clock()`
moves the server's clock.

Needs DATABASE_URL (a role that can create databases). Set DORMAN_BIN_DIR to
use prebuilt binaries; otherwise this runs `cargo build` first.
"""

from __future__ import annotations

import os
import socket
import subprocess
import time
import urllib.request
import uuid
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

import psycopg
import pytest

ROOT = Path(__file__).resolve().parents[1]
FIXTURE_BATCH = ROOT / "fixtures" / "batch.example.json"
DAY = "2026-10-27"


def _bin_dir() -> Path:
    configured = os.environ.get("DORMAN_BIN_DIR")
    if configured:
        return Path(configured)
    subprocess.run(
        [
            "cargo",
            "build",
            "-q",
            "-p",
            "dorman-ingest",
            "-p",
            "dorman-api",
            "--features",
            "dorman-api/test-clock",
        ],
        cwd=ROOT,
        check=True,
    )
    return ROOT / "target" / "debug"


@pytest.fixture(scope="session")
def bin_dir() -> Path:
    return _bin_dir()


@pytest.fixture(scope="session")
def base_db_url() -> str:
    url = os.environ.get("DATABASE_URL")
    if not url:
        pytest.skip("DATABASE_URL not set")
    return url


def fresh_database(base_url: str) -> tuple[str, str]:
    name = f"e2e_{uuid.uuid4().hex[:12]}"
    with psycopg.connect(base_url, autocommit=True) as admin:
        admin.execute(f'CREATE DATABASE "{name}"')
    # A URL, not a libpq keyword string: the Rust side only takes URLs.
    return name, urlunsplit(urlsplit(base_url)._replace(path=f"/{name}"))


def drop_database(base_url: str, name: str) -> None:
    with psycopg.connect(base_url, autocommit=True) as admin:
        admin.execute(f'DROP DATABASE IF EXISTS "{name}" WITH (FORCE)')


def ingest(bin_dir: Path, db_url: str, *args: str) -> str:
    result = subprocess.run(
        [str(bin_dir / "dorman-ingest"), *args],
        env={**os.environ, "DATABASE_URL": db_url},
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"dorman-ingest {' '.join(args)} failed:\n{result.stderr}")
    return result.stdout


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@dataclass
class Dorman:
    base_url: str
    db_url: str
    clock_file: Path

    def set_clock(self, iso: str) -> None:
        self.clock_file.write_text(iso + "\n", encoding="utf-8")

    def query(self, sql: str, *params: object) -> list[tuple]:
        with psycopg.connect(self.db_url) as conn:
            return conn.execute(sql, params).fetchall()


@pytest.fixture(scope="session")
def dorman(bin_dir: Path, base_db_url: str, tmp_path_factory) -> Iterator[Dorman]:
    name, db_url = fresh_database(base_db_url)
    tmp = tmp_path_factory.mktemp("dorman")
    clock_file = tmp / "clock"
    clock_file.write_text(f"{DAY}T12:00:00Z\n")
    server = None
    log = (tmp / "api.log").open("w")
    try:
        ingest(bin_dir, db_url, "migrate")
        ingest(bin_dir, db_url, "load", str(FIXTURE_BATCH))
        ingest(bin_dir, db_url, "daily", "--day", DAY)

        port = _free_port()
        server = subprocess.Popen(
            [str(bin_dir / "dorman-api")],
            env={
                **os.environ,
                "DATABASE_URL": db_url,
                "DORMAN_BIND": f"127.0.0.1:{port}",
                "DORMAN_COOKIE_SECRET": "e2e-secret-e2e-secret-e2e-secret!",
                "DORMAN_SECURE_COOKIES": "false",
                "DORMAN_TEST_CLOCK_FILE": str(clock_file),
            },
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        base_url = f"http://127.0.0.1:{port}"
        deadline = time.monotonic() + 20
        while True:
            try:
                urllib.request.urlopen(f"{base_url}/healthz", timeout=1)
                break
            except OSError:
                if server.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(f"dorman-api didn't start; see {tmp / 'api.log'}") from None
                time.sleep(0.1)
        yield Dorman(base_url, db_url, clock_file)
    finally:
        if server is not None:
            server.terminate()
            server.wait(timeout=10)
        log.close()
        drop_database(base_db_url, name)


@pytest.fixture(scope="session")
def base_url(dorman: Dorman) -> str:
    """Overrides pytest-playwright's base_url so `page.goto("/")` hits the server."""
    return dorman.base_url


@pytest.fixture(autouse=True)
def _reset_clock(request) -> Iterator[None]:
    yield
    if "dorman" in request.fixturenames:
        request.getfixturevalue("dorman").set_clock(f"{DAY}T12:00:00Z")
