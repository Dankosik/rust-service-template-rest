# Capture initializer input as literal Make data only for the initialization
# goal. Other goals must not inherit empty identity variables and thereby
# conflict with the init CLI's duplicate-input refusal.
ifneq (,$(filter template-init,$(MAKECMDGOALS)))
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
override SERVICE_NAME := $(value SERVICE_NAME)
export SERVICE_NAME
override REPOSITORY := $(value REPOSITORY)
export REPOSITORY
override DESCRIPTION := $(value DESCRIPTION)
export DESCRIPTION
override CODEOWNER := $(value CODEOWNER)
export CODEOWNER
override DATABASE := $(value DATABASE)
export DATABASE
override AUTHN := $(value AUTHN)
export AUTHN
override OUTBOUND_HTTP := $(value OUTBOUND_HTTP)
export OUTBOUND_HTTP
override OUTBOUND_AUTH := $(value OUTBOUND_AUTH)
export OUTBOUND_AUTH
override GRPC := $(value GRPC)
export GRPC
override HTTP_IDEMPOTENCY := $(value HTTP_IDEMPOTENCY)
export HTTP_IDEMPOTENCY
override JOBS := $(value JOBS)
export JOBS
override MESSAGING := $(value MESSAGING)
export MESSAGING
override OUTBOX := $(value OUTBOX)
export OUTBOX
override WEBHOOKS := $(value WEBHOOKS)
export WEBHOOKS
override INBOUND_WEBHOOKS := $(value INBOUND_WEBHOOKS)
export INBOUND_WEBHOOKS
override CACHE := $(value CACHE)
export CACHE
override OBJECT_STORAGE := $(value OBJECT_STORAGE)
export OBJECT_STORAGE
override SERVICE_AUTH := $(value SERVICE_AUTH)
export SERVICE_AUTH
override AGENT_HARNESS := $(value AGENT_HARNESS)
export AGENT_HARNESS
endif

include make/service.mk
include make/template.mk
-include make/source.mk
