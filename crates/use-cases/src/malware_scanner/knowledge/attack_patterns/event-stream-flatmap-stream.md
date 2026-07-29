---
type: AttackPattern
description: A dependency added by a new co-maintainer decrypted its payload only using the target app's own package name as the key, staying inert elsewhere (2018).
tags: [trust-based-distribution-compromise]
---
# event-stream (2018)

## Carrier
flatmap-stream, a dependency added by a newly-added co-maintainer of the popular
event-stream package

## Technique
payload encrypted, decrypted only using the target app's own package name or description
as the key: inert everywhere else, evading generic sandboxes

## Payload/effect
targeted the Copay Bitcoin wallet specifically

## Detectable signal
a dependency that decrypts a blob using a key derived from local package.json fields at
runtime, not a constant
