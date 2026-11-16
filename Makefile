# Capture initializer input as literal Make data only for the initialization
# goal. Other goals must not inherit empty identity variables and thereby
# conflict with the init CLI's duplicate-input refusal.
ifneq (,$(filter template-init,$(MAKECMDGOALS)))
TEMPLATE_INIT_VARS := SERVICE_NAME REPOSITORY DESCRIPTION CODEOWNER DATABASE AUTHN \
	OUTBOUND_HTTP OUTBOUND_AUTH GRPC HTTP_IDEMPOTENCY JOBS MESSAGING OUTBOX \
	WEBHOOKS INBOUND_WEBHOOKS CACHE OBJECT_STORAGE SERVICE_AUTH AGENT_HARNESS
DATABASE ?= none
AUTHN ?= none
OUTBOUND_HTTP ?= none
OUTBOUND_AUTH ?= none
GRPC ?= none
HTTP_IDEMPOTENCY ?= none
JOBS ?= none
MESSAGING ?= none
OUTBOX ?= none
WEBHOOKS ?= none
INBOUND_WEBHOOKS ?= none
CACHE ?= none
OBJECT_STORAGE ?= none
SERVICE_AUTH ?= none
AGENT_HARNESS ?= all
export $(TEMPLATE_INIT_VARS)
endif

include make/service.mk
include make/template.mk
-include make/source.mk
