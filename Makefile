# template:begin source-template:make-initializer-inputs
# Capture initializer variables literally before the other Makefiles are read,
# and export them only for initialization.
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
$(foreach var,$(TEMPLATE_INIT_VARS),$(eval override $(var) := $$(value $(var))))
export $(TEMPLATE_INIT_VARS)
endif
# template:end source-template:make-initializer-inputs

include make/service.mk
include make/template.mk
-include make/source.mk
