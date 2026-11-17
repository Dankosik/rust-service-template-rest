# Telemetry decisions

## Unrecognized OTLP compression settings

OpenTelemetry defines OTLP compression as an enum and requires implementations
to warn and ignore unrecognized values. [OTLP exporter configuration](https://opentelemetry.io/docs/specs/otel/protocol/exporter/#configuration-options) · [OpenTelemetry enum requirements](https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/#enum)

The template deliberately degrades trace export when the selected compression
setting is invalid. The SDK warning was lost during startup while the exporter
reported `initialized`, so operators could miss that a compression setting
had been ignored. A typo can therefore disable OTLP trace export for the
process. [Accepted change in PR #416](https://github.com/Dankosik/rust-service-template-rest/pull/416)

Reopen this deviation when startup can warn operators that the compression
setting was ignored while withholding its value.
