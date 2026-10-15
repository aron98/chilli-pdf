# Security policy

## Supported versions

Only the latest tagged release is supported.

## Offline by design

chilli-pdf performs **no network requests at runtime**: no telemetry, no
update checks, no downloads. If you observe the released binary attempting
any network connection, that is a security bug worth reporting.

## Reporting a vulnerability

Please report suspected vulnerabilities privately to the repository owner
(via GitHub security advisories: *Security → Report a vulnerability*) rather
than opening a public issue. Include reproduction steps and affected
versions. You will receive an acknowledgment and a fix timeline.

## Hardening expectations

- Native libraries load exclusively from the app's versioned, checksum-
  verified extraction directory — never the working directory or `PATH`.
- Bundled assets are verified against an embedded SHA-256 manifest before
  use.
- Output is written transactionally; the input is never modified.
- Errors never include document contents.
