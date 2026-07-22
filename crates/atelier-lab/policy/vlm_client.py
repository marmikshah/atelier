#!/usr/bin/env python3
"""Thin command-policy client for the persistent local VLM server."""

import json
import os
import sys
import urllib.error
import urllib.request


def main():
    request_body = sys.stdin.buffer.read()
    request = json.loads(request_body)
    if request.get("format_version") != 1:
        raise ValueError("unsupported policy request format")
    url = os.environ.get("ATELIER_VLM_URL", "http://127.0.0.1:8766/generate")
    call = urllib.request.Request(
        url,
        data=json.dumps(request, separators=(",", ":")).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(call, timeout=600) as response:
            body = response.read(1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        detail = error.read(4096).decode(errors="replace")
        raise ValueError(f"VLM server rejected request ({error.code}): {detail}") from error
    if len(body) > 1024 * 1024:
        raise ValueError("VLM server response exceeds 1 MiB")
    result = json.loads(body)
    if result.get("format_version") != 1 or "action" not in result:
        raise ValueError("VLM server returned an invalid PolicyResponse")
    sys.stdout.write(json.dumps(result, separators=(",", ":")) + "\n")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
