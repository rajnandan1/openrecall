---
name: retry-limit
description: The export queue retries a failed row 3 times
metadata:
  type: decision
scope: github.com/acme/app
source: S0 2026-01-01T00:00:00.000Z
---

The export queue retries a failed row 3 times, then parks it; see src/queue.py for the limit.
