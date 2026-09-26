# Capture initializer input as literal Make data only for the initialization
# goal. Other goals must not inherit empty identity variables and thereby
# conflict with the init CLI's duplicate-input refusal.
ifneq (,$(filter template-init,$(MAKECMDGOALS)))
DATABASE ?= none
AUTHN ?= none
OUTBOUND_AUTH ?= none
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
override OUTBOUND_AUTH := $(value OUTBOUND_AUTH)
export OUTBOUND_AUTH
override AGENT_HARNESS := $(value AGENT_HARNESS)
export AGENT_HARNESS
endif

include make/service.mk
include make/template.mk
-include make/source.mk
