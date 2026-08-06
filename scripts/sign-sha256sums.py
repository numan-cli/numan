#!/usr/bin/env python3
"""Sign SHA256SUMS with the Numan release Ed25519 key.

Reads the 32-byte seed from env NUMAN_RELEASE_SIGNING_KEY (standard base64).
Writes a single-line base64 Ed25519 signature over the exact file bytes to
the output path (default: <input>.sig).

Requires: pip install pynacl
"""

from __future__ import annotations

import base64
import os
import sys


def main() -> int:
    if len(sys.argv) < 2 or len(sys.argv) > 3:
        print(
            f"usage: {sys.argv[0]} SHA256SUMS [SHA256SUMS.sig]",
            file=sys.stderr,
        )
        return 2

    sums_path = sys.argv[1]
    sig_path = sys.argv[2] if len(sys.argv) == 3 else f"{sums_path}.sig"

    seed_b64 = os.environ.get("NUMAN_RELEASE_SIGNING_KEY", "").strip()
    if not seed_b64:
        print(
            "NUMAN_RELEASE_SIGNING_KEY is unset; cannot sign SHA256SUMS",
            file=sys.stderr,
        )
        return 1

    try:
        from nacl.signing import SigningKey
    except ImportError:
        print("pynacl is required: pip install pynacl", file=sys.stderr)
        return 1

    seed = base64.b64decode(seed_b64)
    if len(seed) != 32:
        print(
            f"NUMAN_RELEASE_SIGNING_KEY must decode to 32 bytes, got {len(seed)}",
            file=sys.stderr,
        )
        return 1

    with open(sums_path, "rb") as f:
        data = f.read()

    signing_key = SigningKey(seed)
    signature_b64 = base64.b64encode(signing_key.sign(data).signature).decode("ascii")
    with open(sig_path, "w", encoding="ascii") as f:
        f.write(signature_b64)
        f.write("\n")

    print(f"Signed {sums_path} -> {sig_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
