---
type: AttackPattern
description: An archive member named with a `../` prefix or an absolute path writes outside the destination directory, because the extracting code joins member names to a path without checking them. Affects every language with an archive library (Snyk, Jun 2018). Grep for the extraction call the tree's language uses: `extractall`, `unpack_archive`, `extractAllTo`, `unzipper.Extract`, `tar.x`, `filepath.Join`, `ZipEntry.getName`, `Archive::unpack`.
tags: [untrusted-input-expansion]
---
# Zip Slip (Snyk disclosure, Jun 2018)

## Carrier
an archive (zip, tar, jar, war, apk, nupkg, or a project-specific bundle extension) that the
program takes in from outside itself: downloaded from an index or mirror, uploaded, or supplied
as a plugin, theme, model, or update package

## Technique
a member name carrying a parent-directory prefix (`../../etc/...`), an absolute path, or a
symlink pointing out of the tree. Extraction helpers in most languages join the member name to
the destination directory and write the result, so the name inside the archive decides the write
path. Whoever produced the archive chose that name, not the code doing the extraction

## Payload/effect
arbitrary file write with the extracting process's rights. Landing on a file that something
later runs turns the write into code execution: a shell startup file, an interpreter import
hook, an `authorized_keys`, a cron or service unit, or a binary already on the PATH

## Detectable signal
an extraction call with nothing between opening the archive and writing it that inspects the
member names, and nothing before the open that checks the archive against a digest or a
signature. By language:
- Python: `.extractall(...)` on a `zipfile.ZipFile` or `tarfile`, `shutil.unpack_archive(...)`
- JavaScript: `extractAllTo(...)` (adm-zip), `unzipper.Extract(...)`, `tar.x(...)`,
  `decompress(...)`
- Go: an `archive/zip` or `archive/tar` reader loop calling `filepath.Join(dest, header.Name)`
- Java: `ZipInputStream` with `ZipEntry.getName()` passed to a `File` or `Path`
- Rust: `zip::ZipArchive::extract(...)`, `tar::Archive::unpack(...)`
- C/C++: `libarchive` `archive_read_extract`, or a `minizip` loop writing `filename_inzip`

The absence of the guard is the finding, so name what is missing: no rejection of a `..`
component, no canonicalised path compared against the destination, no symlink check. An
extraction that does carry one of those is not this pattern.
