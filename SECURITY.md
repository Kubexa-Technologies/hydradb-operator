# Security Policy

## Reporting a vulnerability

Do not open a public issue. Report privately to the maintainers with a description of the issue, affected versions, and steps to reproduce if known. Expect an acknowledgement and coordinated disclosure.

## Blast radius note

The operator holds no credentials itself. It references Kubernetes Secrets (for example object-storage keys) but never stores secret material in its own spec or status. The effective blast radius of a compromise is the cluster RBAC granted to the operator's ServiceAccount, so keep those Roles minimal.
