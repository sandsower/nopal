import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

export type McpServerConfig = {
	command: string;
	args?: string[];
	env?: Record<string, string>;
	connectTimeoutSeconds?: number;
};

export type McpBridgeConfig = {
	servers: Record<string, McpServerConfig>;
	aliases: Record<string, string>;
};

export type LoadedMcpBridgeConfig = {
	config: McpBridgeConfig;
	configPaths: string[];
	diagnostics: string[];
};

export type McpToolDefinition = {
	name: string;
	description?: string;
	inputSchema?: unknown;
};

export type McpBridgeClient = {
	listTools: () => Promise<McpToolDefinition[]>;
	callTool: (name: string, args: unknown) => Promise<unknown>;
	close: () => Promise<void>;
};

export type McpBridgeClientFactory = (serverName: string, config: McpServerConfig) => Promise<McpBridgeClient>;

export type McpBridgeToolState = {
	name: string;
	toolName: string;
	description?: string;
};

export type McpBridgeServerState = {
	name: string;
	status: "connecting" | "connected" | "error" | "skipped";
	tools: McpBridgeToolState[];
	error?: string;
};

export type McpBridgeAliasState = {
	alias: string;
	target: string;
	status: "registered" | "missing-target";
};

export type McpBridgeState = {
	configPaths: string[];
	diagnostics: string[];
	servers: Record<string, McpBridgeServerState>;
	aliases: Record<string, McpBridgeAliasState>;
};

type McpBridgeOptions = {
	config?: McpBridgeConfig;
	configPaths?: string[];
	diagnostics?: string[];
	clientFactory?: McpBridgeClientFactory;
	piConfigPath?: string;
	claudeConfigPath?: string;
	/**
	 * When true, await every server connection before returning. Default false:
	 * connections run in the background so pi startup is not blocked (tools
	 * register as each server comes online, which pi surfaces mid-session).
	 * Tests set this true for deterministic, fully-populated state.
	 */
	awaitConnections?: boolean;
};

type ToolRegistration = {
	serverName: string;
	mcpToolName: string;
	client: McpBridgeClient;
	tool: McpToolDefinition;
};

const DEFAULT_PI_CONFIG_PATH = join(homedir(), ".pi", "agent", "mcp.json");
const DEFAULT_CLAUDE_CONFIG_PATH = join(homedir(), ".config", "Claude", "claude_desktop_config.json");
const DEFAULT_CONNECT_TIMEOUT_MS = 8000;
const EmptyParams = Type.Object({}, { additionalProperties: true });

export default async function (pi: ExtensionAPI) {
	await createMcpBridge(pi);
}

export async function createMcpBridge(pi: ExtensionAPI, options: McpBridgeOptions = {}): Promise<McpBridgeState> {
	const loaded = options.config
		? { config: options.config, configPaths: options.configPaths ?? [], diagnostics: options.diagnostics ?? [] }
		: loadMcpBridgeConfig({ piConfigPath: options.piConfigPath, claudeConfigPath: options.claudeConfigPath });
	const clientFactory = options.clientFactory ?? defaultMcpBridgeClientFactory;
	const state: McpBridgeState = { configPaths: loaded.configPaths, diagnostics: [...loaded.diagnostics], servers: {}, aliases: {} };
	const toolTargets = new Map<string, ToolRegistration>();
	const clients: McpBridgeClient[] = [];

	// Seed every configured server as "connecting" synchronously so /mcp and
	// mcp_status reflect the pending set immediately, before any await.
	for (const serverName of Object.keys(loaded.config.servers)) {
		state.servers[serverName] = { name: serverName, status: "connecting", tools: [] };
	}

	// Connect and discover tools for one server, registering its proxy tools as
	// soon as they are known. pi allows registerTool after startup and surfaces
	// new tools mid-session, so this is safe to run off the init path.
	const connectServer = async (serverName: string, serverConfig: McpServerConfig): Promise<void> => {
		let client: McpBridgeClient | undefined;
		try {
			const timeoutMs = timeoutMsForServer(serverConfig);
			client = await withTimeout(clientFactory(serverName, serverConfig), timeoutMs, `${serverName} MCP connection timed out after ${timeoutMs}ms`);
			const tools = await withTimeout(client.listTools(), timeoutMs, `${serverName} MCP tool discovery timed out after ${timeoutMs}ms`);
			clients.push(client);
			const serverState: McpBridgeServerState = { name: serverName, status: "connected", tools: [] };
			for (const tool of tools) {
				const piToolName = buildMcpToolName(serverName, tool.name);
				serverState.tools.push({ name: tool.name, toolName: piToolName, description: tool.description });
				toolTargets.set(piToolName, { serverName, mcpToolName: tool.name, client, tool });
				registerMcpProxyTool(pi, piToolName, serverName, tool, client);
			}
			state.servers[serverName] = serverState;
		} catch (error) {
			if (client) await client.close().catch(() => {});
			state.servers[serverName] = { name: serverName, status: "error", tools: [], error: errorMessage(error) };
		}
	};

	// Aliases point at a tool exposed by a server, so they can only resolve once
	// that server's connection has settled.
	const registerAliases = (): void => {
		for (const [alias, target] of Object.entries(loaded.config.aliases)) {
			const registration = toolTargets.get(target);
			if (!registration) {
				state.aliases[alias] = { alias, target, status: "missing-target" };
				continue;
			}
			state.aliases[alias] = { alias, target, status: "registered" };
			registerMcpProxyTool(pi, alias, registration.serverName, { ...registration.tool, description: `Alias for ${target}` }, registration.client);
		}
	};

	// Connect all servers in parallel, then wire aliases. Backgrounded by
	// default so pi startup does not block on network round-trips.
	const connectAll = Promise.allSettled(
		Object.entries(loaded.config.servers).map(([serverName, serverConfig]) => connectServer(serverName, serverConfig)),
	).then(registerAliases);

	registerDiagnostics(pi, state);
	pi.on("session_shutdown", async () => {
		await connectAll.catch(() => {});
		await Promise.allSettled(clients.map((client) => client.close()));
	});

	if (options.awaitConnections) await connectAll;
	else connectAll.catch(() => {});

	return state;
}

export function loadMcpBridgeConfig(input: { piConfigPath?: string; claudeConfigPath?: string } = {}): LoadedMcpBridgeConfig {
	const piConfigPath = input.piConfigPath ?? DEFAULT_PI_CONFIG_PATH;
	const claudeConfigPath = input.claudeConfigPath ?? DEFAULT_CLAUDE_CONFIG_PATH;
	const diagnostics: string[] = [];
	const configPaths: string[] = [];
	const primary = readConfigFile(piConfigPath, "pi", diagnostics, configPaths);
	const secondary = readConfigFile(claudeConfigPath, "claude", diagnostics, configPaths);
	return { config: mergeMcpBridgeConfigs(primary, secondary), configPaths, diagnostics };
}

export function mergeMcpBridgeConfigs(primary: McpBridgeConfig, secondary: McpBridgeConfig): McpBridgeConfig {
	return {
		servers: { ...secondary.servers, ...primary.servers },
		aliases: { ...secondary.aliases, ...primary.aliases },
	};
}

export function buildMcpToolName(serverName: string, toolName: string): string {
	return `mcp__${sanitizeToolSegment(serverName)}__${sanitizeToolSegment(toolName)}`;
}

/**
 * mcp-remote logs its full connection handshake and every proxied JSON-RPC
 * message to stderr, which floods a Nopal session on every MCP startup. Its
 * `--silent` flag gates that output at the source while leaving proxying
 * intact. Nopal defaults mcp-remote servers to quiet: when the spawned command
 * is mcp-remote and the config has not already chosen a verbosity flag, inject
 * `--silent`. Set NOPAL_MCP_VERBOSE (to any value other than 0/false) to keep
 * mcp-remote loud, e.g. when debugging an MCP handshake. Non-mcp-remote servers
 * are never touched, since `--silent` is mcp-remote-specific.
 */
export function quietMcpRemoteArgs(command: string, args: string[], env: NodeJS.ProcessEnv = process.env): string[] {
	const verbose = env.NOPAL_MCP_VERBOSE;
	if (verbose && verbose !== "0" && verbose !== "false") return args;
	const isMcpRemote = /(^|[/\\])mcp-remote$/.test(command) || args.includes("mcp-remote");
	if (!isMcpRemote) return args;
	if (args.includes("--silent") || args.includes("--debug")) return args;
	return [...args, "--silent"];
}

export function formatMcpStatus(state: McpBridgeState): string {
	const lines: string[] = ["MCP bridge"];
	lines.push("");
	lines.push("config:");
	if (state.configPaths.length === 0) lines.push("- no config files loaded");
	for (const path of state.configPaths) lines.push(`- ${path}`);
	if (state.diagnostics.length > 0) {
		lines.push("");
		lines.push("diagnostics:");
		for (const diagnostic of state.diagnostics) lines.push(`- ${diagnostic}`);
	}
	lines.push("");
	lines.push("servers:");
	const servers = Object.values(state.servers);
	if (servers.length === 0) lines.push("- none");
	for (const server of servers) {
		lines.push(`- ${server.name}: ${server.status}${server.error ? ` (${server.error})` : ""}`);
		for (const tool of server.tools) lines.push(`  - ${tool.toolName}`);
	}
	lines.push("");
	lines.push("aliases:");
	const aliases = Object.values(state.aliases);
	if (aliases.length === 0) lines.push("- none");
	for (const alias of aliases) lines.push(`- ${alias.alias} -> ${alias.target} ${alias.status}`);
	return lines.join("\n");
}

async function defaultMcpBridgeClientFactory(serverName: string, config: McpServerConfig): Promise<McpBridgeClient> {
	const [{ Client }, { StdioClientTransport }] = await Promise.all([
		import("@modelcontextprotocol/sdk/client/index.js"),
		import("@modelcontextprotocol/sdk/client/stdio.js"),
	]);
	const args = quietMcpRemoteArgs(config.command, config.args ?? []);
	const transport = new StdioClientTransport({ command: config.command, args, env: { ...process.env, ...config.env } as Record<string, string> });
	const client = new Client({ name: `pi-mcp-${serverName}`, version: "0.1.0" }, { capabilities: {} });
	await client.connect(transport);
	return {
		async listTools() {
			const result = await client.listTools();
			return (result.tools ?? []).map((tool: any) => ({ name: tool.name, description: tool.description, inputSchema: tool.inputSchema }));
		},
		async callTool(name, args) {
			return client.callTool({ name, arguments: args as Record<string, unknown> });
		},
		async close() {
			await client.close();
		},
	};
}

function registerMcpProxyTool(pi: ExtensionAPI, piToolName: string, serverName: string, tool: McpToolDefinition, client: McpBridgeClient): void {
	pi.registerTool({
		name: piToolName,
		label: `MCP: ${serverName}.${tool.name}`,
		description: tool.description ? `MCP tool ${serverName}.${tool.name}: ${tool.description}` : `MCP tool ${serverName}.${tool.name}`,
		parameters: schemaForTool(tool.inputSchema),
		async execute(_toolCallId, params) {
			try {
				const result = await client.callTool(tool.name, params);
				return textResult(formatMcpToolResult(result), { result });
			} catch (error) {
				return textResult(`MCP tool ${serverName}.${tool.name} failed: ${errorMessage(error)}`, { error: errorMessage(error) }, true);
			}
		},
	});
}

function registerDiagnostics(pi: ExtensionAPI, state: McpBridgeState): void {
	pi.registerTool({
		name: "mcp_status",
		label: "MCP Status",
		description: "Show configured MCP bridge servers, exposed tools, aliases, and startup diagnostics.",
		parameters: Type.Object({}),
		async execute() {
			return textResult(formatMcpStatus(state), { state });
		},
	});
	pi.registerCommand("mcp", {
		description: "Show MCP bridge status",
		handler: async (_args, ctx) => {
			ctx.ui.notify(formatMcpStatus(state), "info");
		},
	});
}

function readConfigFile(path: string, source: "pi" | "claude", diagnostics: string[], configPaths: string[]): McpBridgeConfig {
	if (!existsSync(path)) return emptyConfig();
	configPaths.push(path);
	try {
		const raw = JSON.parse(readFileSync(path, "utf-8"));
		return normalizeRawConfig(raw, source, diagnostics);
	} catch (error) {
		diagnostics.push(`${source}: failed to parse ${path}: ${errorMessage(error)}`);
		return emptyConfig();
	}
}

function normalizeRawConfig(raw: any, source: "pi" | "claude", diagnostics: string[]): McpBridgeConfig {
	const servers: Record<string, McpServerConfig> = {};
	const rawServers = isRecord(raw?.mcpServers) ? raw.mcpServers : {};
	for (const [name, value] of Object.entries(rawServers)) {
		if (!isRecord(value) || typeof value.command !== "string" || !value.command.trim()) {
			diagnostics.push(`${source}: skipped ${name} because it is not a stdio command server`);
			continue;
		}
		if (value.command.startsWith("/") && !existsSync(value.command)) {
			diagnostics.push(`${source}: skipped ${name} because command does not exist: ${value.command}`);
			continue;
		}
		servers[name] = {
			command: value.command,
			args: Array.isArray(value.args) ? value.args.map(String) : undefined,
			env: isRecord(value.env) ? stringRecord(value.env) : undefined,
			connectTimeoutSeconds: typeof value.connectTimeoutSeconds === "number" ? value.connectTimeoutSeconds : undefined,
		};
	}
	return { servers, aliases: isRecord(raw?.aliases) ? stringRecord(raw.aliases) : {} };
}

function schemaForTool(inputSchema: unknown): any {
	if (!isRecord(inputSchema) || inputSchema.type !== "object") return EmptyParams;
	return {
		...inputSchema,
		type: "object",
		properties: isRecord(inputSchema.properties) ? inputSchema.properties : {},
	};
}

function formatMcpToolResult(result: unknown): string {
	if (isRecord(result) && Array.isArray(result.content)) {
		const parts = result.content.map((part) => {
			if (isRecord(part) && typeof part.text === "string") return part.text;
			return JSON.stringify(part);
		});
		return parts.join("\n");
	}
	if (typeof result === "string") return result;
	return JSON.stringify(result, null, 2);
}

function textResult(text: string, details: Record<string, unknown> = {}, isError = false) {
	return { content: [{ type: "text" as const, text }], details, isError };
}

function timeoutMsForServer(config: McpServerConfig): number {
	const seconds = config.connectTimeoutSeconds;
	if (typeof seconds !== "number" || !Number.isFinite(seconds) || seconds <= 0) return DEFAULT_CONNECT_TIMEOUT_MS;
	return Math.max(1, Math.round(seconds * 1000));
}

function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const timeout = new Promise<never>((_resolve, reject) => {
		timer = setTimeout(() => reject(new Error(message)), timeoutMs);
	});
	return Promise.race([promise, timeout]).finally(() => {
		if (timer) clearTimeout(timer);
	});
}

function emptyConfig(): McpBridgeConfig {
	return { servers: {}, aliases: {} };
}

function sanitizeToolSegment(value: string): string {
	const sanitized = value.toLowerCase().replace(/[^a-z0-9_]+/g, "_").replace(/^_+|_+$/g, "");
	return sanitized || "unnamed";
}

function isRecord(value: unknown): value is Record<string, any> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringRecord(value: Record<string, unknown>): Record<string, string> {
	const result: Record<string, string> = {};
	for (const [key, val] of Object.entries(value)) result[key] = String(val);
	return result;
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}
