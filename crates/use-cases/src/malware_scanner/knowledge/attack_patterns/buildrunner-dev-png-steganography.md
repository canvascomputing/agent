---
type: AttackPattern
description: npm postinstall fetches images from a non-registry host and decodes a payload from PNG pixel/RGB data (Veracode, Feb 2026).
tags: [binary-media-disguise]
---
# buildrunner-dev npm package (Veracode, Feb 2026)

## Carrier
postinstall hook -> small init.js -> obfuscated batch file fetched from an external host
(not npm/GitHub, e.g. Codeberg)

## Technique
fodhelper.exe UAC bypass, then payload hidden in PNG pixel data: size in the first pixels,
3 payload bytes per pixel across R/G/B channels

## Payload/effect
decodes and runs a .NET Pulsar RAT

## Detectable signal
postinstall/init script fetching from a non-registry host; the string `fodhelper`; any
image file read byte-by-byte with per-channel bit math instead of as an asset
