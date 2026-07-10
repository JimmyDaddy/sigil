# Security Policy

Sigil is an early alpha coding agent that can read files, modify a workspace,
run commands, and connect to configured providers and MCP servers. Treat its
permission, sandbox, secret-handling, and session boundaries as
security-sensitive.

## Supported Versions

Security fixes are provided for the latest published release and the current
`main` branch. Older alpha releases may not receive backports.

| Version | Supported |
| --- | --- |
| Latest release | Yes |
| Current `main` | Yes |
| Older releases | No |

## Reporting a Vulnerability

Do not open a public issue for a suspected vulnerability. Email
`heyjimmygo@gmail.com` with the subject `[Sigil Security]` and include:

- the affected version or commit;
- the operating system and execution backend;
- the relevant provider, MCP, plugin, or tool configuration with secrets
  removed;
- reproduction steps and observed impact;
- any suggested mitigation, if known.

Please do not include live credentials, private repository contents, or other
users' data. We aim to acknowledge a complete report within seven days, then
coordinate validation, remediation, and disclosure through the private email
thread.

## Security Scope

Reports are especially useful when they demonstrate workspace escape, approval
or sandbox bypass, secret exposure, untrusted extension execution, session-log
tampering, unsafe recovery, or a remotely triggerable denial of service. A
model producing incorrect or undesirable text without crossing a documented
security boundary is generally a product-quality issue rather than a security
vulnerability.
