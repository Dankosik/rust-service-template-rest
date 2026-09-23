# Source-template checks. This include is intentionally absent from generated
# services, so their standard aggregate cannot recurse into the source matrix.

SOURCE_CHECK_TARGETS := template-owned-purity-check template-init-check

.PHONY: template-owned-purity-check template-init-check

template-owned-purity-check: ## Check source-only manifest and portability boundaries
	python3 scripts/tests/template-owned-purity.py --repo .

template-init-check: ## Check 96 canonical projections and build/test twelve runtime representatives
	@{ test "$(ALLOW_FULL)" = 1 || test "$(CI)" = true; } || { echo "template-init-check requires ALLOW_FULL=1 (CI sets CI=true)" >&2; exit 2; }
	bash scripts/ci/template-init-check.sh --repo .
