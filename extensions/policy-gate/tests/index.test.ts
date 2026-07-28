import assert from "node:assert/strict";
import { test } from "node:test";
import { activeToolCatalogIsExpected, EXPECTED_PI_TOOL_CATALOG } from "../guard.ts";

test("runtime acknowledgement requires the complete audited Pi tool catalog", () => {
	assert.equal(activeToolCatalogIsExpected([...EXPECTED_PI_TOOL_CATALOG]), true);
	assert.equal(activeToolCatalogIsExpected([...EXPECTED_PI_TOOL_CATALOG].reverse()), true);
	assert.equal(activeToolCatalogIsExpected(EXPECTED_PI_TOOL_CATALOG.filter((name) => name !== "edit")), false);
	assert.equal(activeToolCatalogIsExpected([...EXPECTED_PI_TOOL_CATALOG, "future_mutator"]), false);
	assert.equal(activeToolCatalogIsExpected(["read", "read", ...EXPECTED_PI_TOOL_CATALOG.slice(1)]), false);
});
