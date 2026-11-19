# Source-template commands. Generated services omit this file.

SOURCE_CHECK_TARGETS := template-init-check template-quality-projections

.PHONY: template-init template-owned-purity-check template-init-check template-init-source-checks template-init-projections template-quality-projections template-init-artifacts

template-init: ## Initialize the service identity and selected profiles once
	@python3 scripts/template/init.py --repo .

template-owned-purity-check: ## Check source-only manifest and portability boundaries
	python3 scripts/tests/template-owned-purity.py --repo .

template-init-check: ## Check canonical profile projections and 68 runtime graphs (13-26, 27, 29, 30, 48, 49, 53, 55 also run retained database suites; Docker)
	@{ test "$(ALLOW_FULL)" = 1 || test "$(CI)" = true; } || { echo "template-init-check requires ALLOW_FULL=1 (CI sets CI=true)" >&2; exit 2; }
	bash scripts/template/check_init.sh --repo .

template-init-source-checks: ## Run source purity, initializer safety, and template sync checks
	bash scripts/template/check_init.sh --repo . --source-checks

template-init-projections: ## Check canonical profile projections alone, without Cargo
	bash scripts/template/check_init.sh --repo . --projections-only

template-quality-projections: ## Exercise readability checkers on four retained/removed, renamed profile graphs
	bash scripts/template/check_init.sh --repo . --quality-projections

# Kept separate from SOURCE_CHECK_TARGETS: the image job owns this serial proof.
template-init-artifacts: ## Build and prove selected initialized release images once each; ALLOW_HEAVY=1
	@{ test "$(ALLOW_HEAVY)" = 1 || test "$(CI)" = true; } || { echo "template-init-artifacts requires ALLOW_HEAVY=1 (CI sets CI=true)" >&2; exit 2; }
ifeq ($(origin ARTIFACT_GRAPHS),undefined)
	bash scripts/template/check_init.sh --repo . --default-artifact-graphs
else
	bash scripts/template/check_init.sh --repo . --artifact-graphs "$(ARTIFACT_GRAPHS)"
endif
