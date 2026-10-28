{{/* SPDX-License-Identifier: Apache-2.0 */}}
{{/* hydradb-operator helpers. */}}
{{/* fullname: release-scoped resource name, DNS-1123 safe. */}}
{{- define "hydradb-operator.fullname" -}}
{{- printf "%s-%s" .Release.Name "hydradb-operator" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/* name: chart short name. */}}
{{- define "hydradb-operator.name" -}}
{{- .Chart.Name -}}
{{- end -}}

{{/* labels: standard app.kubernetes.io labels. */}}
{{- define "hydradb-operator.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{ include "hydradb-operator.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{/* selectorLabels: stable pod/selector identity. Never chart/version (selectors immutable). */}}
{{- define "hydradb-operator.selectorLabels" -}}
app.kubernetes.io/name: {{ include "hydradb-operator.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/* serviceAccountName: operator service account name. */}}
{{- define "hydradb-operator.serviceAccountName" -}}
{{- if .Values.serviceAccount.name -}}
{{ .Values.serviceAccount.name }}
{{- else -}}
{{ include "hydradb-operator.fullname" . }}
{{- end -}}
{{- end -}}

{{/* isCluster: true when installScope is cluster. */}}
{{- define "hydradb-operator.isCluster" -}}
{{- eq .Values.installScope "cluster" -}}
{{- end -}}

{{/* rbacKind: ClusterRole in cluster scope, Role in namespaced scope. */}}
{{- define "hydradb-operator.rbacKind" -}}
{{- if eq .Values.installScope "cluster" -}}ClusterRole{{- else -}}Role{{- end -}}
{{- end -}}

{{/* bindingKind: ClusterRoleBinding in cluster scope, RoleBinding in namespaced scope. */}}
{{- define "hydradb-operator.bindingKind" -}}
{{- if eq .Values.installScope "cluster" -}}ClusterRoleBinding{{- else -}}RoleBinding{{- end -}}
{{- end -}}

{{/* image: repository:tag, digest-pinned when .Values.image.digest is set. */}}
{{- define "hydradb-operator.image" -}}
{{- $base := printf "%s:%s" .Values.image.repository .Values.image.tag -}}
{{- if .Values.image.digest -}}
{{- printf "%s@sha256:%s" $base .Values.image.digest -}}
{{- else -}}
{{- $base -}}
{{- end -}}
{{- end -}}
