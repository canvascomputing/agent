---
type: AttackPattern
description: Payload hidden in git "test fixture" files, only assembled by a malicious build-to-host.m4 in the release tarball, hijacking glibc IFUNC (CVE-2024-3094, Mar 2024).
tags: [build-artifact-only]
---
# xz-utils / liblzma backdoor (CVE-2024-3094, Mar 2024)

## Carrier
"test" fixture files in the public repo (tests/files/bad-3-corrupt_lzma2.xz,
.../good-large_compressed.lzma) that actually hold payload bytes

## Technique
a maliciously modified build-to-host.m4, run only from the *released tarball's*
./configure (invisible in a plain git checkout), unpacks those files via a tr+xz pipeline
and links the result into liblzma, hijacking glibc IFUNC resolution

## Payload/effect
pre-auth unauthenticated RCE in OpenSSH via a hardcoded ED448 key

## Detectable signal
build scripts (.m4, configure.ac, Makefile.am) piping a "test"/"fixture" file through a
decoder into the link step; a fixture whose size or entropy is off
