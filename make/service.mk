# Repository-owned facts and recipes. `make/template.mk` owns the portable
# method and standard target names; this file deliberately has no include,
# evaluation directive, or standard-target recipe.
SERVICE_PACKAGE ?= service
SERVICE_BIN ?= service
RUNTIME_IMAGE ?= service:ci
CONTAINER_IMAGE ?= $(RUNTIME_IMAGE)
LOCAL_CONFIG ?= env/config/local.toml
OPENAPI_FILE ?= api/openapi/service.yaml
OPENAPI_BREAKING_APPROVALS ?= api/openapi/breaking-changes-approvals.txt
