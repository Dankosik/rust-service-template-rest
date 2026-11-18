# Telemetry decisions

## Unrecognized OTLP compression settings

OpenTelemetry defines OTLP compression as an enum and requires implementations
to warn and ignore unrecognized values. [OTLP exporter configuration](https://opentelemetry.io/docs/specs/otel/protocol/exporter/#configuration-options) · [OpenTelemetry enum requirements](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/#enum)

The SDK reads compression settings before the service installs its logging
subscriber. Startup retains the names of ignored variables so operators can
correct them without recording their values. A supported common setting still
applies when the signal-specific setting is unrecognized.

## Collector credentials

A typed header requires a typed HTTPS destination. Standard header variables
are accepted with a standard HTTPS destination. A general credential bound to
a configured general collector cannot follow a trace endpoint override to a
different destination. Use trace-specific credentials for that destination.
Carry credentials in headers because collector URLs can appear in proxy logs.

The HTTP exporter reads standard headers again when it builds the client.
Startup applies the SDK's signal precedence, rejects duplicate names before
their spelling is lost, and supplies the validated headers explicitly so an
empty signal variable cannot hide the general credentials.

For mutual TLS, configure the collector endpoint, client certificate and key
through the standard OTLP variables. When a typed endpoint also selects the
collector, the effective standard trace destination must match it, including
the path. Both the certificate and key must be bound to that destination.
A private CA used solely to verify a typed HTTPS collector needs no additional
endpoint setting.

When a platform projects a rotating client identity, point both client
certificate and key settings at its combined PEM bundle, or use paths pinned to
one release. The publisher can switch generations between two separate opens.
Reload this startup identity by replacing the replica.
[Kubernetes projected certificates](https://kubernetes.io/docs/concepts/storage/projected-volumes/#podcertificate-projected-volumes)
