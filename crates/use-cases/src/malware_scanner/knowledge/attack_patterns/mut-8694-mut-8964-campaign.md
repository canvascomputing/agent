---
type: AttackPattern
description: 42 PyPI + 18 npm typosquats; npm loaders obfuscated while PyPI setup.py hooks were left plain, invoking PowerShell to fetch infostealers (Datadog, Oct 2024).
tags: [auto-executing-install-hook]
---
# MUT-8694 / MUT-8964 (Datadog, Oct 2024)

## Carrier
42 typosquatted PyPI packages and 18 npm packages in one campaign

## Technique
the npm loaders were run through obfuscator.io (control-flow flattening, dead code, string
encoding); the PyPI setup.py install hooks were left completely unobfuscated, deliberately
asymmetric effort within the same campaign

## Payload/effect
setup.py invokes PowerShell directly to fetch a second-stage infostealer (Blank Grabber,
Skuld Stealer), disables Windows Defender

## Detectable signal
setup.py/cfg.py calling subprocess or os.system with powershell, -enc, iwr, or iex; an npm
bundle unusually flattened or string-encoded for its stated purpose
