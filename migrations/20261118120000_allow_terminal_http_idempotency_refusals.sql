ALTER TABLE http_idempotency_records
    DROP CONSTRAINT http_idempotency_records_status_check,
    ADD CONSTRAINT http_idempotency_records_status_check
        CHECK (status BETWEEN 200 AND 299 OR status IN (409, 422)) NOT VALID;
