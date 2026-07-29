# Security policy

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability.
Use GitHub's private vulnerability reporting flow from the repository Security tab.
Include the affected version, operating system, reproduction steps, expected impact, and any proposed mitigation.
Avoid including real credentials, private repository contents, or personal data in the report.

The maintainers will acknowledge a complete report as soon as practical, validate the issue, coordinate a fix, and disclose it after users have a reasonable upgrade path.

## Supported versions

Security fixes target the latest published Nopal release and the default branch.
Older releases may receive a fix when the affected code is still supported and a safe backport is practical.

## Scope

Nopal launches an exact Pi distribution, mediates protected Pi tool calls, executes confined verification gates, and records local enforcement evidence.
Reports involving authorization bypass, command execution, credential exposure, unsafe file handling, runtime substitution, dependency compromise, or ledger integrity are especially valuable.
Vulnerabilities in bundled Pi, Node, or other third-party software may also be reported here when they affect Nopal users.
A plain Pi session started outside `nopal` is outside the Nopal enforcement boundary.
