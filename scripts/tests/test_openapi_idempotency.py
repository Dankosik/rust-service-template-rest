"""Unit tests for scripts.checks.openapi_idempotency."""

from __future__ import annotations

import unittest
from typing import Any

from scripts.checks import openapi_idempotency


class OpenApiIdempotencyTests(unittest.TestCase):
    @staticmethod
    def _doc(paths: dict[str, Any]) -> dict[str, Any]:
        return {"openapi": "3.1.0", "paths": paths}

    @staticmethod
    def _idempotent_op() -> dict[str, Any]:
        return {
            "post": {
                "parameters": [{"name": "Idempotency-Key", "in": "header", "required": True}],
                "responses": {"200": {"headers": {"Idempotent-Replayed": {}}}},
            }
        }

    def test_detects_contract_removal_and_precedence_shadow(self) -> None:
        base = self._doc({"/v1/orders/{id}": self._idempotent_op()})
        removed = self._doc({"/v1/orders/{id}": {"post": {"responses": {"200": {}}}}})
        self.assertTrue(openapi_idempotency.check_compatibility(base, removed))
        shadowed = self._doc({"/v1/orders/{id}": self._idempotent_op(), "/v1/orders/special": {"post": {"responses": {"200": {}}}}})
        self.assertTrue(openapi_idempotency.check_compatibility(base, shadowed))
        self.assertEqual(openapi_idempotency.check_compatibility(base, base), [])


if __name__ == "__main__":
    unittest.main()
