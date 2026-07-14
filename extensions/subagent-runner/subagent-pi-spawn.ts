import { existsSync, readFileSync, realpathSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { SubagentInvocation } from "./index.js";

const require = createRequire(import.meta.url);
export const SUBAGENT_CHILD_ENV = "PI_SUBAGENT_CHILD";

export type PiSpawnDeps = {
	platform?: NodeJS.Platform;
	execPath?: string;
	argv1?: string;
	existsSync?: (path: string) => boolean;
	readFileSync?: (path: string, encoding: "utf-8") => string;
	resolvePackageJson?: () => string;
	piPackageRoot?: string;
};

export type ChildPiInvocation = SubagentInvocation & {
	env: Record<string, string | undefined>;
};

export function childRuntimeExtensionPath(): string {
	return fileURLToPath(new URL("./subagent-child-runtime.ts", import.meta.url));
}

function normalizePath(path: string): string {
	return isAbsolute(path) ? path : resolve(path);
}

function isRunnableNodeScript(path: string, exists: (path: string) => boolean): boolean {
	return exists(path) && /\.(?:mjs|cjs|js)$/i.test(path);
}

export function resolvePiPackageRoot(): string | undefined {
	try {
		const entry = process.argv[1];
		if (!entry) return undefined;
		let dir = dirname(realpathSync(entry));
		while (dir !== dirname(dir)) {
			try {
				const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf-8")) as { name?: string };
				if (pkg.name === "@earendil-works/pi-coding-agent") return dir;
			} catch {
				// Keep walking parents.
			}
			dir = dirname(dir);
		}
	} catch {
		// Best effort only.
	}
	return undefined;
}

export function resolveWindowsPiCliScript(deps: PiSpawnDeps = {}): string | undefined {
	const exists = deps.existsSync ?? existsSync;
	const read = deps.readFileSync ?? ((path, encoding) => readFileSync(path, encoding));
	const argv1 = deps.argv1 ?? process.argv[1];

	if (argv1) {
		const argvPath = normalizePath(argv1);
		if (isRunnableNodeScript(argvPath, exists)) return argvPath;
	}

	try {
		const resolvePackageJson = deps.resolvePackageJson ?? (() => {
			const root = deps.piPackageRoot ?? resolvePiPackageRoot();
			if (root) return join(root, "package.json");
			return require.resolve("@earendil-works/pi-coding-agent/package.json");
		});
		const packageJsonPath = resolvePackageJson();
		const packageJson = JSON.parse(read(packageJsonPath, "utf-8")) as { bin?: string | Record<string, string> };
		const binField = packageJson.bin;
		const binPath = typeof binField === "string" ? binField : binField?.pi ?? Object.values(binField ?? {})[0];
		if (!binPath) return undefined;
		const candidate = normalizePath(resolve(dirname(packageJsonPath), binPath));
		if (isRunnableNodeScript(candidate, exists)) return candidate;
	} catch {
		return undefined;
	}

	return undefined;
}

export function getPiSpawnCommand(args: string[], deps: PiSpawnDeps = {}): SubagentInvocation {
	const platform = deps.platform ?? process.platform;
	if (platform === "win32") {
		const piCliPath = resolveWindowsPiCliScript(deps);
		if (piCliPath) return { command: deps.execPath ?? process.execPath, args: [piCliPath, ...args] };
	}

	const currentScript = deps.argv1 ?? process.argv[1];
	const isBunVirtualScript = currentScript?.startsWith("/$bunfs/root/");
	const exists = deps.existsSync ?? existsSync;
	if (currentScript && !isBunVirtualScript && exists(currentScript)) {
		return { command: deps.execPath ?? process.execPath, args: [currentScript, ...args] };
	}

	const execName = (deps.execPath ?? process.execPath).split(/[\\/]/).at(-1)?.toLowerCase() ?? "";
	const isGenericRuntime = /^(node|bun)(\.exe)?$/.test(execName);
	if (!isGenericRuntime) return { command: deps.execPath ?? process.execPath, args };

	return { command: "pi", args };
}

export function buildPiSubprocessInvocationFromPromptFile(promptPath: string, deps: PiSpawnDeps = {}): ChildPiInvocation {
	const args = ["--mode", "json", "-p", "--no-session", "--extension", childRuntimeExtensionPath(), `@${promptPath}`];
	const invocation = getPiSpawnCommand(args, deps);
	return {
		...invocation,
		env: { [SUBAGENT_CHILD_ENV]: "1" },
	};
}
