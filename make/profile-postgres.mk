# PostgreSQL profile commands. This local whole-file pack is removed when an
# initialized service selects DATABASE=none.
POSTGRES_CHECK_TARGETS := migration-check migration-history-self-test
# Database-backed tests compile only with their feature; lint them without
# running them. This is absent when the PostgreSQL profile is absent.
INTEGRATION_LINT_FEATURES := --features integration-tests/integration

.PHONY: compose-up compose-down test-integration-db migration-check migration-history-self-test migration-validate

compose-up: ## Start the local PostgreSQL from env/docker-compose.yml on port POSTGRES_PORT (default 5432)
	docker compose -f env/docker-compose.yml up -d --wait postgres

compose-down: ## Stop the local PostgreSQL and drop its volume
	docker compose -f env/docker-compose.yml down -v --remove-orphans

test-integration-db: ## Database-backed proof against a throwaway compose PostgreSQL; ALLOW_HEAVY=1, REQUIRE_DOCKER=1 to fail without Docker
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/ci/test-integration-db.sh

migration-check: ## Static append-only history check (BASE_REF for a range) and the source rules over the embedded set
	BASE_REF="$(BASE_REF)" bash scripts/ci/migration-history-check.sh
	$(CARGO) test -p migrate $(CARGO_FLAGS)

migration-history-self-test: ## Self-test of scripts/ci/migration-history-check.sh
	bash scripts/ci/migration-history-check.sh --self-test

migration-validate: ## Rehearse RUNTIME_IMAGE: /migrate against a fresh compose PostgreSQL, replay is no_change, lifecycle check with the profile on; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/ci/migration-validate.sh "$(RUNTIME_IMAGE)" "$(RUNTIME_EXPECTED_COMMIT)"
