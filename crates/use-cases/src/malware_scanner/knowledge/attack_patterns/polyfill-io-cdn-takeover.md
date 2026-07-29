---
type: AttackPattern
description: A widely embedded CDN domain was sold to a new owner who modified the served polyfill.js to inject malware/redirects for mobile user agents (Jun 2024).
tags: [trust-based-distribution-compromise]
---
# polyfill.io (Jun 2024)

## Carrier
`<script src="cdn.polyfill.io/...">` embedded in over 100,000 sites; the CDN domain itself
sold to a new owner

## Technique
served polyfill.js modified to inject malware or redirects, conditioned on mobile
user-agent: targeted, not blanket

## Payload/effect
malicious redirects for specific mobile visitors

## Detectable signal
a script src pointing at a third-party CDN domain outside the project's own origin or
allowlist, serving dynamically generated JS per request
