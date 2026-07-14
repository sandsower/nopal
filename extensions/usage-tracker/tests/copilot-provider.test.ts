import assert from "node:assert/strict";
import { test } from "node:test";
import { loadUsageTrackerModule } from "./setup.ts";

const providers = await loadUsageTrackerModule<typeof import("../usage-tracker-providers.ts")>("../usage-tracker-providers.ts");

// ---------------------------------------------------------------------------
// Copilot provider support
// ---------------------------------------------------------------------------

test("Copilot provider: maps pi github-copilot auth to the copilot quota provider", () => {
	assert.equal(providers.AUTH_KEY_TO_PROVIDER["github-copilot"], "copilot");
});

test("Copilot provider: selects Copilot quota provider for GitHub-hosted Claude and GPT models", () => {
	assert.deepEqual(providers.providerKeysForModel("github-copilot", "claude-sonnet-4.6"), ["copilot"]);
	assert.deepEqual(providers.providerKeysForModel("github-copilot", "gpt-5.4"), ["copilot"]);
});

test("Copilot provider: uses the GitHub OAuth token, not the short-lived Copilot proxy token, for Copilot quota probes", () => {
	assert.equal(
		providers.tokenForProviderProbe("copilot", {
			access: "tid=proxy-token",
			refresh: "ghu_github-token",
		}),
		"ghu_github-token",
	);
});

test("Copilot provider: parses Copilot internal user quota with nested premium interaction snapshot", () => {
	const result = providers.parseCopilotInternalUserQuota({
		quota_snapshots: {
			premium_interactions: {
				entitlement: 300,
				remaining: 225,
			},
		},
		quota_reset_date: "2026-05-01",
		copilot_plan: "pro",
	});

	assert.equal(result.windows.length, 1);
	assert.equal(result.windows[0]?.label, "Premium requests (75/300 used)");
	assert.equal(result.windows[0]?.percentLeft, 75);
	assert.equal(result.windows[0]?.windowMinutes, null);
	assert.equal(result.plan, "pro");
});

test("Copilot provider: probes GitHub Copilot internal user endpoint with bearer auth", async () => {
	const calls: Array<{ url: string; init: RequestInit | undefined }> = [];
	const fetchImpl = async (url: string | URL | Request, init?: RequestInit) => {
		calls.push({ url: String(url), init });
		return new Response(
			JSON.stringify({
				monthly_premium_requests: { total: 300, used: 45 },
				copilot_plan: "pro",
			}),
			{ status: 200, headers: { "content-type": "application/json" } },
		);
	};

	const result = await providers.probeCopilotDirect("ghu_test_token", fetchImpl);

	assert.equal(calls[0]?.url, "https://api.github.com/copilot_internal/user");
	assert.equal((calls[0]?.init?.headers as Record<string, string>)?.authorization, "Bearer ghu_test_token");
	assert.equal(result.provider, "copilot");
	assert.equal(result.windows[0]?.percentLeft, 85);
});
