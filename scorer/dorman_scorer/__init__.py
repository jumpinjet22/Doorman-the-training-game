"""Nightly scoring for Who's At The Door? .

The pure parts (trust, Dawid-Skene, behavior flags, consensus) take plain
data and are tested on their own; `db` is the thin layer that reads and
writes Postgres, and `__main__` wires them together.
"""
