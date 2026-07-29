---
type: AttackPattern
description: Compromised maintainer accounts of popular npm packages pushed malicious versions at wide blast radius; exact execution mechanism unconfirmed (Dec 2025).
tags: [trust-based-distribution-compromise]
---
# Shai-Hulud 2.0 (Dec 2025)

## Carrier
compromised maintainer accounts of popular npm packages (Zapier, PostHog, Postman among
them)

## Technique
malicious versions pushed downstream at wide blast radius (500+ packages, 25,000+
repositories per secondary reporting); exact execution mechanism unconfirmed, treat as an
open question rather than a fixed pattern

## Payload/effect
unconfirmed; flag for extra scrutiny rather than pattern-match a mechanism

## Detectable signal
a lockfile bump to a widely-used package with no matching upstream changelog or release
notes
