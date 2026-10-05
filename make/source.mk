# Source-template checks. This include is intentionally absent from generated
# services, so their standard aggregate cannot recurse into the source matrix.

SOURCE_CHECK_TARGETS := template-owned-purity-check template-init-check template-quality-projections

.PHONY: template-owned-purity-check template-init-check template-init-projections template-quality-projections template-init-artifacts

template-owned-purity-check: ## Check source-only manifest and portability boundaries
	python3 scripts/tests/template-owned-purity.py --repo .

template-init-check: ## Check 368 canonical projections and sixty-three runtime graphs (13-26, 27, 29, 30, 48, 49, 53, 55 also run retained database suites; Docker)
	@{ test "$(ALLOW_FULL)" = 1 || test "$(CI)" = true; } || { echo "template-init-check requires ALLOW_FULL=1 (CI sets CI=true)" >&2; exit 2; }
	bash scripts/ci/template-init-check.sh --repo .

template-init-projections: ## Check the 368 canonical projections alone, without Cargo
	bash scripts/ci/template-init-check.sh --repo . --projections-only

template-quality-projections: ## Exercise readability checkers on four retained/removed, renamed profile graphs
	bash scripts/ci/template-init-check.sh --repo . --quality-projections

# Kept separate from SOURCE_CHECK_TARGETS: the image job owns this serial proof.
ARTIFACT_GRAPHS ?= 1,7,47,65
template-init-artifacts: ## Build and prove selected initialized release images once each; ALLOW_HEAVY=1
	@{ test "$(ALLOW_HEAVY)" = 1 || test "$(CI)" = true; } || { echo "template-init-artifacts requires ALLOW_HEAVY=1 (CI sets CI=true)" >&2; exit 2; }
	bash scripts/ci/template-init-check.sh --repo . --artifact-graphs "$(ARTIFACT_GRAPHS)"
