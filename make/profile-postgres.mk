# PostgreSQL profile commands. This local whole-file pack is removed when an
# initialized service selects DATABASE=none.
POSTGRES_CHECK_TARGETS := migration-check migration-history-self-test
# Database-backed tests compile only with their feature; lint them without
# running them. This is absent when the PostgreSQL profile is absent.
INTEGRATION_LINT_FEATURES := --features integration-tests/integration

# sqlx-cli publishes no release binaries, so the pinned version is built
# once into the Git common directory, locally and in CI alike (CI caches it).
# `rustls` keeps OpenSSL out of the build; only the PostgreSQL driver is in.
SQLX_CLI_BIN := $(TOOLS_ROOT)/sqlx-cli-$(SQLX_CLI_VERSION)/bin

.PHONY: compose-up compose-down test-integration-db sqlx-prepare sqlx-check migration-check migration-history-self-test migration-validate

$(SQLX_CLI_BIN)/cargo-sqlx:
	$(CARGO) install --locked --root "$(TOOLS_ROOT)/sqlx-cli-$(SQLX_CLI_VERSION)" --version "$(SQLX_CLI_VERSION)" --no-default-features --features postgres,rustls sqlx-cli

compose-up: ## Start the local PostgreSQL from env/docker-compose.yml on port POSTGRES_PORT (default 5432)
	docker compose -f env/docker-compose.yml up -d --wait postgres

compose-down: ## Stop the local PostgreSQL and drop its volume
	docker compose -f env/docker-compose.yml down -v --remove-orphans

test-integration-db: ## Database-backed proof against a throwaway compose PostgreSQL; ALLOW_HEAVY=1, REQUIRE_DOCKER=1 to fail without Docker
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/ci/test-integration-db.sh

sqlx-prepare: $(SQLX_CLI_BIN)/cargo-sqlx ## Regenerate .sqlx/ (query metadata for sqlx::query!) against a throwaway compose PostgreSQL with the migrations applied
	$(VALIDATION_LOCK) env PATH="$(SQLX_CLI_BIN):$$PATH" bash scripts/ci/sqlx-prepare.sh

sqlx-check: $(SQLX_CLI_BIN)/cargo-sqlx ## Fail when .sqlx/ differs from what the statements and the migrations produce now; ALLOW_HEAVY=1, REQUIRE_DOCKER=1 to fail without Docker
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) env PATH="$(SQLX_CLI_BIN):$$PATH" bash scripts/ci/sqlx-prepare.sh --check

migration-check: ## Static append-only history check (BASE_REF for a range), Squawk over the added files (Node.js), and the source rules over the embedded set
	BASE_REF="$(BASE_REF)" bash scripts/ci/migration-history-check.sh
	$(BUILD_CARGO) test -p migrate $(CARGO_FLAGS)

migration-history-self-test: ## Self-test of scripts/ci/migration-history-check.sh
	bash scripts/ci/migration-history-check.sh --self-test

migration-validate: ## Rehearse RUNTIME_IMAGE: /migrate against a fresh compose PostgreSQL, replay is no_change, lifecycle check with the profile on; ALLOW_HEAVY=1
	$(HEAVY_GUARD)
	$(VALIDATION_LOCK) bash scripts/ci/migration-validate.sh "$(RUNTIME_IMAGE)" "$(RUNTIME_EXPECTED_COMMIT)"
