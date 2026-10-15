# SPDX-License-Identifier: Apache-2.0
# Monitoring

The chart renders a Prometheus `ServiceMonitor`
(`monitoring.coreos.com/v1`) only when
`monitoring.serviceMonitor.enabled: true`. The default is `false`, so a
plain install creates no monitoring objects.

The monitor scrapes the operator `healthz` Service (`:8080/metrics` every
30s). RBAC for `servicemonitors` is always present so enabling the monitor
needs no permission change.

```sh
helm upgrade hydradb-operator oci://harbor.kubexa.tech/oss/hydradb-operator \
  --set monitoring.serviceMonitor.enabled=true
```

CI renders the enabled set (`ci/servicemonitor-enabled-values.yaml`) and
`tests/servicemonitor_test.yaml` asserts absence by default and presence
when enabled.
