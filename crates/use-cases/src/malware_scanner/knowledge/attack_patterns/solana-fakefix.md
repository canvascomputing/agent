---
type: AttackPattern
description: About 25 typosquatted npm/PyPI packages appended stealer code after real exports, auto-running via postinstall/import to harvest crypto and cloud credentials (2025/2026).
tags: [auto-executing-install-hook]
---
# Solana FakeFix (2025/2026)

## Carrier
about 25 typosquatted npm/PyPI packages posing as "fixed" Solana SDK variants; malicious
code appended after the real exports/source-map comment, so the package still works

## Technique
runs automatically via npm postinstall or Python __init__.py import, no explicit call needed

## Payload/effect
harvests Solana keypairs, SSH keys, AWS credentials, and environment variables matching
KEY/SECRET/MNEMONIC/PRIVATE/TOKEN/PASSWORD; advanced variants add Telegram-bot
command-and-control

## Detectable signal
code appended after `//# sourceMappingURL=` or after a package's real module.exports;
environment reads matching that name list combined with an outbound POST
