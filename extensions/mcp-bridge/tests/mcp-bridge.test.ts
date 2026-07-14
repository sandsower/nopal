import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { buildMcpToolName, createMcpBridge, formatMcpStatus, loadMcpBridgeConfig, mergeMcpBridgeConfigs, quietMcpRemoteArgs } from "../index.ts";
import type { McpBridgeClientFactory, McpBridgeConfig, McpBridgeState } from "../index.ts";

// ---------------------------------------------------------------------------
// quiet mcp-remote
// ---------------------------------------------------------------------------

test("quietMcpRemoteArgs: injects --silent for npx-spawned mcp-remote", () => {
	const args = quietMcpRemoteArgs("npx", ["-y", "mcp-remote", "https://mcp.linear.app/mcp"], {});
	assert.deepEqual(args, ["-y", "mcp-remote", "https://mcp.linear.app/mcp", "--silent"]);
});

test("quietMcpRemoteArgs: injects --silent for a direct mcp-remote command", () => {
	assert.deepEqual(quietMcpRemoteArgs("mcp-remote", ["https://x/mcp"], {}), ["https://x/mcp", "--silent"]);
	assert.deepEqual(quietMcpRemoteArgs("/opt/bin/mcp-remote", ["https://x/mcp"], {}), ["https://x/mcp", "--silent"]);
});

test("quietMcpRemoteArgs: leaves non-mcp-remote servers untouched", () => {
	const args = ["run", "some-other-server"];
	assert.deepEqual(quietMcpRemoteArgs("uvx", args, {}), args);
});

test("quietMcpRemoteArgs: does not duplicate an existing verbosity flag", () => {
	assert.deepEqual(quietMcpRemoteArgs("npx", ["mcp-remote", "u", "--silent"], {}), ["mcp-remote", "u", "--silent"]);
	assert.deepEqual(quietMcpRemoteArgs("npx", ["mcp-remote", "u", "--debug"], {}), ["mcp-remote", "u", "--debug"]);
});

test("quietMcpRemoteArgs: NOPAL_MCP_VERBOSE keeps mcp-remote loud, but 0/false still quiets", () => {
	assert.deepEqual(quietMcpRemoteArgs("npx", ["mcp-remote", "u"], { NOPAL_MCP_VERBOSE: "1" }), ["mcp-remote", "u"]);
	assert.deepEqual(quietMcpRemoteArgs("npx", ["mcp-remote", "u"], { NOPAL_MCP_VERBOSE: "0" }), ["mcp-remote", "u", "--silent"]);
	assert.deepEqual(quietMcpRemoteArgs("npx", ["mcp-remote", "u"], { NOPAL_MCP_VERBOSE: "false" }), ["mcp-remote", "u", "--silent"]);
});

// ---------------------------------------------------------------------------
// mcp-bridge config
// ---------------------------------------------------------------------------

test("mcp-bridge config: loads pi-native config and imports Claude desktop stdio config with pi precedence", () => {
	const dir = mkdtempSync(join(tmpdir(), "mcp-bridge-"));
	const piPath = join(dir, "pi-mcp.json");
	const claudePath = join(dir, "claude_desktop_config.json");
	writeFileSync(
		piPath,
		JSON.stringify({
			mcpServers: {
				linear: { command: "npx", args: ["-y", "mcp-remote", "https://mcp.linear.app/mcp"], env: { FROM: "pi" } },
			},
			aliases: { mcp__plugin_linear_linear__get_issue: "mcp__linear__get_issue" },
		}),
	);
	writeFileSync(
		claudePath,
		JSON.stringify({
			mcpServers: {
				linear: { command: "claude-linear", env: { FROM: "claude" } },
				dala: { command: "dala-mcp", args: ["serve"] },
				missingAbsolute: { command: "/tmp/missing-dala-mcp", args: ["serve"] },
				remoteOnly: { url: "https://example.test/mcp" },
			},
		}),
	);

	const loaded = loadMcpBridgeConfig({ piConfigPath: piPath, claudeConfigPath: claudePath });

	assert.equal(loaded.config.servers.linear?.command, "npx");
	assert.equal(loaded.config.servers.linear?.env?.FROM, "pi");
	assert.equal(loaded.config.servers.dala?.command, "dala-mcp");
	assert.equal(loaded.config.servers.remoteOnly, undefined);
	assert.equal(loaded.config.servers.missingAbsolute, undefined);
	assert.equal(loaded.config.aliases.mcp__plugin_linear_linear__get_issue, "mcp__linear__get_issue");
	assert.ok(loaded.diagnostics.some((line) => line.includes("skipped remoteOnly")));
	assert.ok(loaded.diagnostics.some((line) => line.includes("skipped missingAbsolute")));
});

test("mcp-bridge config: invalid config files produce diagnostics instead of throwing", () => {
	const dir = mkdtempSync(join(tmpdir(), "mcp-bridge-"));
	const piPath = join(dir, "pi-mcp.json");
	writeFileSync(piPath, "{");

	const loaded = loadMcpBridgeConfig({ piConfigPath: piPath, claudeConfigPath: join(dir, "missing.json") });

	assert.deepEqual(loaded.config.servers, {});
	assert.ok(loaded.diagnostics.join("\n").includes("failed to parse"));
});

test("mcp-bridge config: merge keeps primary servers over secondary servers", () => {
	const primary: McpBridgeConfig = { servers: { linear: { command: "primary" } }, aliases: { a: "b" } };
	const secondary: McpBridgeConfig = { servers: { linear: { command: "secondary" }, other: { command: "other" } }, aliases: { c: "d" } };

	const merged = mergeMcpBridgeConfigs(primary, secondary);

	assert.equal(merged.servers.linear?.command, "primary");
	assert.equal(merged.servers.other?.command, "other");
	assert.deepEqual(merged.aliases, { c: "d", a: "b" });
});

// ---------------------------------------------------------------------------
// mcp-bridge naming and diagnostics
// ---------------------------------------------------------------------------

test("mcp-bridge naming: builds Claude-compatible tool names", () => {
	assert.equal(buildMcpToolName("linear", "get_issue"), "mcp__linear__get_issue");
	assert.equal(buildMcpToolName("plugin-linear", "save comment"), "mcp__plugin_linear__save_comment");
});

test("mcp-bridge diagnostics: formats server, tool, alias, and error status", () => {
	const state: McpBridgeState = {
		configPaths: ["/tmp/mcp.json"],
		diagnostics: ["loaded /tmp/mcp.json"],
		servers: {
			linear: { name: "linear", status: "connected", tools: [{ name: "get_issue", toolName: "mcp__linear__get_issue" }] },
			bad: { name: "bad", status: "error", tools: [], error: "boom" },
		},
		aliases: {
			mcp__plugin_linear_linear__get_issue: { alias: "mcp__plugin_linear_linear__get_issue", target: "mcp__linear__get_issue", status: "registered" },
		},
	};

	const report = formatMcpStatus(state);

	assert.ok(report.includes("linear: connected"));
	assert.ok(report.includes("mcp__linear__get_issue"));
	assert.ok(report.includes("bad: error"));
	assert.ok(report.includes("boom"));
	assert.ok(report.includes("mcp__plugin_linear_linear__get_issue -> mcp__linear__get_issue registered"));
});

// ---------------------------------------------------------------------------
// mcp-bridge extension
// ---------------------------------------------------------------------------

function fakePi(registered: Record<string, any>, handlers: Record<string, Function> = {}) {
	return {
		registerTool(tool: any) {
			registered[tool.name] = tool;
		},
		registerCommand(_name: string, _command: any) {},
		on(event: string, handler: Function) {
			handlers[event] = handler;
		},
	};
}

test("mcp-bridge extension: discovers tools, registers proxy tools, and forwards calls", async () => {
	const calls: Array<{ name: string; args: unknown }> = [];
	const factory: McpBridgeClientFactory = async () => ({
		listTools: async () => [{ name: "get_issue", description: "Get an issue", inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"] } }],
		callTool: async (name, args) => {
			calls.push({ name, args });
			return { content: [{ type: "text", text: "Issue DC-4986" }] };
		},
		close: async () => {},
	});
	const registered: Record<string, any> = {};
	const pi = fakePi(registered);

	await createMcpBridge(pi as any, {
		config: {
			servers: { linear: { command: "npx", args: ["mcp-remote"] } },
			aliases: { mcp__plugin_linear_linear__get_issue: "mcp__linear__get_issue" },
		},
		clientFactory: factory,
		awaitConnections: true,
	});

	assert.deepEqual(Object.keys(registered).sort(), ["mcp__linear__get_issue", "mcp__plugin_linear_linear__get_issue", "mcp_status"]);
	assert.equal(registered.mcp__plugin_linear_linear__get_issue.parameters.properties.id.type, "string");
	const result = await registered.mcp__plugin_linear_linear__get_issue.execute("tc1", { id: "DC-4986" });

	assert.deepEqual(calls, [{ name: "get_issue", args: { id: "DC-4986" } }]);
	assert.equal(result.content[0].text, "Issue DC-4986");
});

test("mcp-bridge extension: normalizes empty tool schemas into valid object schemas", async () => {
	const factory: McpBridgeClientFactory = async () => ({
		listTools: async () => [{ name: "ping", inputSchema: { type: "object" } }],
		callTool: async () => ({ content: [] }),
		close: async () => {},
	});
	const registered: Record<string, any> = {};

	await createMcpBridge(fakePi(registered) as any, {
		config: { servers: { test: { command: "test" } }, aliases: {} },
		clientFactory: factory,
		awaitConnections: true,
	});

	assert.equal(registered.mcp__test__ping.parameters.type, "object");
	assert.deepEqual(registered.mcp__test__ping.parameters.properties, {});
});

test("mcp-bridge extension: times out slow discovery and closes the client", async () => {
	let closed = 0;
	const factory: McpBridgeClientFactory = async () => ({
		listTools: async () => new Promise(() => {}),
		callTool: async () => ({ content: [] }),
		close: async () => {
			closed += 1;
		},
	});
	const registered: Record<string, any> = {};

	const state = await createMcpBridge(fakePi(registered) as any, {
		config: { servers: { linear: { command: "npx", connectTimeoutSeconds: 0.01 } }, aliases: {} },
		clientFactory: factory,
		awaitConnections: true,
	});

	assert.equal(state.servers.linear?.status, "error");
	assert.ok(state.servers.linear?.error?.includes("timed out"));
	assert.equal(closed, 1);
});

test("mcp-bridge extension: keeps startup alive when one server fails", async () => {
	const factory: McpBridgeClientFactory = async (_serverName) => {
		throw new Error("cannot start");
	};
	const registered: Record<string, any> = {};

	const state = await createMcpBridge(fakePi(registered) as any, {
		config: { servers: { bad: { command: "missing" } }, aliases: {} },
		clientFactory: factory,
		awaitConnections: true,
	});

	assert.equal(state.servers.bad?.status, "error");
	assert.ok(formatMcpStatus(state).includes("cannot start"));
	assert.ok(registered.mcp_status !== undefined);
});

test("mcp-bridge extension: closes clients on session shutdown", async () => {
	let closed = 0;
	const handlers: Record<string, Function> = {};
	const factory: McpBridgeClientFactory = async () => ({
		listTools: async () => [],
		callTool: async () => ({ content: [] }),
		close: async () => {
			closed += 1;
		},
	});

	await createMcpBridge(fakePi({}, handlers) as any, {
		config: { servers: { linear: { command: "npx" } }, aliases: {} },
		clientFactory: factory,
		awaitConnections: true,
	});
	await handlers.session_shutdown?.({}, {});

	assert.equal(closed, 1);
});

test("mcp-bridge extension: backgrounds connections so init returns before a slow server is ready", async () => {
	let releaseConnect: () => void = () => {};
	const gate = new Promise<void>((resolve) => {
		releaseConnect = resolve;
	});
	const factory: McpBridgeClientFactory = async () => {
		await gate;
		return {
			listTools: async () => [{ name: "get_issue", inputSchema: { type: "object" } }],
			callTool: async () => ({ content: [] }),
			close: async () => {},
		};
	};
	const registered: Record<string, any> = {};

	// Default (background): returns immediately, server still connecting, tool not yet registered.
	const state = await createMcpBridge(fakePi(registered) as any, {
		config: { servers: { linear: { command: "npx", args: ["mcp-remote"] } }, aliases: {} },
		clientFactory: factory,
	});
	assert.equal(state.servers.linear?.status, "connecting");
	assert.equal(registered.mcp__linear__get_issue, undefined);
	assert.ok(registered.mcp_status !== undefined, "diagnostics register synchronously");

	// Once the connection completes, the tool registers mid-session and state flips to connected.
	releaseConnect();
	await new Promise((resolve) => setTimeout(resolve, 10));
	assert.equal(state.servers.linear?.status, "connected");
	assert.ok(registered.mcp__linear__get_issue !== undefined, "tool registered after background connect");
});
