# SPDX-License-Identifier: Apache-2.0
# Install scope

`installScope` selects the operator watch scope and the RBAC kind.

| `installScope` | Watches | RBAC |
|---|---|---|
| `namespaced` (default) | `OPERATOR_NAMESPACE` only | `Role` + `RoleBinding` |
| `cluster` | all namespaces | `ClusterRole` + `ClusterRoleBinding` |

The Deployment always carries `OPERATOR_SCOPE` with the same value, so the
binary and the RBAC cannot drift apart. `namespaced` is the safer default:
least privilege, no cluster-wide permissions.

Switch scopes without reinstalling the CRDs:

```sh
helm upgrade hydradb-operator oci://harbor.kubexa.tech/oss/hydradb-operator \
  --set installScope=cluster
```

CI renders both scopes (`ci/default-values.yaml`,
`ci/cluster-scope-values.yaml`) and `tests/rbac_test.yaml` asserts the kind
per scope.
