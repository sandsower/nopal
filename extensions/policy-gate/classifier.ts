import { realpathSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";

/**
 * Pure classification and protected-floor logic for policy-gate.
 *
 * Shell parsing, protected paths, and command-shape rules compile only into
 * Core's closed `{ action, class }` vocabulary. Unknown tools, actions, and
 * ambiguous command envelopes cannot receive authority or human approval.
 * This module never decides authority.
 *
 * No pi imports here: this file (and its tests) must run as plain node
 * modules with no extension host and no subprocess.
 */

export type Classification = {
	action: string;
	class: string;
};

export type CommandClassifications = {
	classifications: Classification[];
	complete: boolean;
	reason?: string;
};

export type ToolIntent = Classification & {
	kind: "nopal.enforcement.tool_intent/v1";
	toolName: string;
	inputDigest: string;
	targetDigest: string;
	changedFiles: string[];
	mutates: boolean;
};

export type ToolIntentResult = {
	complete: boolean;
	intent?: ToolIntent;
	reason?: string;
};

type Segment = {
	raw: string;
	argv: string[];
};

type ClassRule = {
	id: string;
	command?: string | string[];
	argsPrefixAny?: string[][];
	argsContainsAny?: string[];
	class: string;
	action: string | ((segment: Segment) => string);
};

const SECRET_KEY_RE = /\b(?:[A-Z0-9_]*(?:SECRET|TOKEN|PASSWORD|PRIVATE[_-]?KEY|API[_-]?KEY|AWS_SECRET|AWS_SESSION)[A-Z0-9_]*)\b/i;

// ---------------------------------------------------------------------------
// Protected floors: local, deterministic, and never routed through Nopal
// policy. They remain active for the entire Nopal-launched Pi process.
// ---------------------------------------------------------------------------

const protectedPathFragments = [
	".env",
	"secrets.env",
	"/.ssh/",
	"/.aws/credentials",
	"/.npmrc",
	"/.netrc",
	"/.pgpass",
	"/.my.cnf",
	"credentials.json",
	"credentials.yml",
	"id_rsa",
	"id_ed25519",
];

export function isProtectedCredentialPath(path: string): boolean {
	const normalized = path.replaceAll("\\", "/");
	return protectedPathFragments.some((fragment) => normalized.includes(fragment));
}

export function shouldBlockProtectedCredentialPath(toolName: string, path: string): boolean {
	return (toolName === "write" || toolName === "edit") && isProtectedCredentialPath(path);
}

export type EnforcementAuthority = {
	projectRoot: string;
	stateDir: string;
	configDir?: string;
	adapterDir: string;
	nopalBin: string;
	runId: string;
	adapterCapability: string;
	gateExecutorBin: string;
	gateHome: string;
	gateExecutorDigest: string;
	gateRuntimeDigest: string;
};

function isWithin(candidate: string, root: string): boolean {
	return candidate === root || candidate.startsWith(`${root}${path.sep}`);
}

function resolvedPathIsProtected(resolved: string, authority: EnforcementAuthority): boolean {
	const projectRoot = resolveThroughExistingAncestor(path.resolve(authority.projectRoot)) ?? path.resolve(authority.projectRoot);
	const stateDir = resolveThroughExistingAncestor(path.resolve(authority.stateDir)) ?? path.resolve(authority.stateDir);
	if (isWithin(resolved, path.join(projectRoot, ".nopal"))) return true;
	if (resolved === path.join(projectRoot, ".beislid", "workflow.md")) return true;
	if (resolved === path.join(projectRoot, ".pi", "settings.json")) return true;
	if (isWithin(resolved, stateDir)) return true;
	const adapterDir = resolveThroughExistingAncestor(path.resolve(authority.adapterDir)) ?? path.resolve(authority.adapterDir);
	const nopalBin = resolveThroughExistingAncestor(path.resolve(authority.nopalBin)) ?? path.resolve(authority.nopalBin);
	if (isWithin(resolved, adapterDir)) return true;
	if (resolved === nopalBin) return true;
	if (authority.configDir) {
		const configDir = resolveThroughExistingAncestor(path.resolve(authority.configDir)) ?? path.resolve(authority.configDir);
		if (resolved === path.join(configDir, "policy.jsonc")) return true;
	}
	return false;
}

export function isProtectedEnforcementPath(candidate: string, cwd: string, authority: EnforcementAuthority): boolean {
	const resolved = path.resolve(cwd, candidate.replace(/^@/, ""));
	if (resolvedPathIsProtected(resolved, authority)) return true;
	const throughExistingAncestor = resolveThroughExistingAncestor(resolved);
	return Boolean(throughExistingAncestor && resolvedPathIsProtected(throughExistingAncestor, authority));
}

function resolveThroughExistingAncestor(candidate: string): string | undefined {
	let cursor = candidate;
	const suffix: string[] = [];
	while (true) {
		try {
			return path.join(realpathSync(cursor), ...suffix.reverse());
		} catch {
			const parent = path.dirname(cursor);
			if (parent === cursor) return undefined;
			suffix.push(path.basename(cursor));
			cursor = parent;
		}
	}
}

export function commandReferencesEnforcementAuthority(command: string, cwd: string, authority: EnforcementAuthority): boolean {
	const normalized = command.replaceAll("\\", "/");
	const protectedStrings = [
		authority.projectRoot,
		authority.stateDir,
		authority.configDir,
		authority.adapterDir,
		authority.nopalBin,
		authority.runId,
		authority.adapterCapability,
		"NOPAL_ENFORCEMENT_RUN_ID",
		"NOPAL_ENFORCEMENT_STATE_DIR",
		"NOPAL_ENFORCEMENT_ROOT",
		"NOPAL_ENFORCEMENT_ADAPTER_DIR",
		"NOPAL_ENFORCEMENT_CLI",
		"NOPAL_ENFORCEMENT_CAPABILITY",
		"BEISLID_STATE_DIR",
		"NOPAL_CONFIG_DIR",
	]
		.filter((value): value is string => Boolean(value))
		.map((value) => value.replaceAll("\\", "/"));
	if (
		/(^|[\s'"`])(?:\.\/)?\.nopal(?:\/|[\s'"`]|$)/.test(normalized)
		|| /\.beislid\/workflow\.md/.test(normalized)
		|| /(?:^|\/)\.pi\/settings\.json(?:[\s'"`]|$)/.test(normalized)
		|| protectedStrings.some((value) => normalized.includes(value))
	) return true;

	for (const segment of parseShellSegments(command)) {
		for (const rawArgument of segment.argv.slice(1)) {
			const argument = rawArgument.includes("=") && rawArgument.startsWith("--")
				? rawArgument.slice(rawArgument.indexOf("=") + 1)
				: rawArgument;
			if (isProtectedEnforcementPath(argument, cwd, authority)) return true;
			const resolved = resolveThroughExistingAncestor(path.resolve(cwd, argument.replace(/^@/, "")));
			if (resolved && isProtectedEnforcementPath(resolved, cwd, authority)) return true;
		}
	}
	return false;
}

export function redactSecrets(input: string): string {
	let output = input;
	output = output.replace(
		/(\b[A-Z0-9_]*(?:SECRET|TOKEN|PASSWORD|PRIVATE[_-]?KEY|API[_-]?KEY|AWS_SECRET|AWS_SESSION)[A-Z0-9_]*\b\s*[=:]\s*)([^\s'"`]+)/gi,
		"$1[REDACTED]",
	);
	output = output.replace(/\b(sk-[A-Za-z0-9_-]{8,})\b/g, "[REDACTED]");
	output = output.replace(/\b(gh[pousr]_[A-Za-z0-9_]{20,})\b/g, "[REDACTED]");
	output = output.replace(/\b(AKIA[0-9A-Z]{16})\b/g, "[REDACTED]");
	output = output.replace(/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, "[REDACTED PRIVATE KEY]");
	return output;
}

export function redactToolContent(content: unknown): unknown {
	if (typeof content === "string") return redactSecrets(content);
	if (!Array.isArray(content)) return content;
	return content.map((entry) => {
		if (!entry || typeof entry !== "object") return entry;
		const maybeText = entry as { type?: string; text?: unknown };
		if (maybeText.type === "text" && typeof maybeText.text === "string") {
			return { ...maybeText, text: redactSecrets(maybeText.text) };
		}
		return entry;
	});
}

// ---------------------------------------------------------------------------
// Shell parsing (ported near-verbatim from safety-gate)
// ---------------------------------------------------------------------------

function stripHeredocsAndProse(command: string): string {
	let output = command.replace(/<<['"]?([A-Za-z0-9_-]+)['"]?[\s\S]*?\n\1\b/g, "<<EOF\nEOF");
	output = output.replace(/(\bgit\s+commit\b[^;&|]*\s-m\s+)(['"])(?:\\.|(?!\2)[\s\S])*\2/g, '$1"[MESSAGE]"');
	output = output.replace(/(\bgh\s+(?:pr|issue)\s+(?:comment|review)\b[^;&|]*\s--body\s+)(['"])(?:\\.|(?!\2)[\s\S])*\2/g, '$1"[BODY]"');
	return output;
}

function shellWords(input: string): string[] {
	const words: string[] = [];
	let current = "";
	let quote: "'" | '"' | null = null;
	for (let i = 0; i < input.length; i++) {
		const char = input[i];
		if (char === "\\") {
			current += input[++i] ?? "";
			continue;
		}
		if ((char === "'" || char === '"') && quote === null) {
			quote = char;
			continue;
		}
		if (char === quote) {
			quote = null;
			continue;
		}
		if (!quote && /\s/.test(char)) {
			if (current) words.push(current);
			current = "";
			continue;
		}
		current += char;
	}
	if (current) words.push(current);
	return words;
}

function splitShell(command: string, separators: string[]): string[] {
	const parts: string[] = [];
	let current = "";
	let quote: "'" | '"' | null = null;
	for (let i = 0; i < command.length; i++) {
		const char = command[i];
		const two = command.slice(i, i + 2);
		if (char === "\\") {
			current += char + (command[++i] ?? "");
			continue;
		}
		if ((char === "'" || char === '"') && quote === null) quote = char;
		else if (char === quote) quote = null;
		if (!quote && separators.includes(two)) {
			parts.push(current);
			current = "";
			i++;
			continue;
		}
		if (!quote && separators.includes(char)) {
			parts.push(current);
			current = "";
			continue;
		}
		current += char;
	}
	parts.push(current);
	return parts;
}

function stripEnvAssignments(argv: string[]): string[] {
	let index = 0;
	while (index < argv.length && /^[A-Za-z_][A-Za-z0-9_]*=.*/.test(argv[index])) index++;
	return argv.slice(index);
}

function unsupportedShellSyntax(command: string): string | undefined {
	if (/[\r\n]/.test(command)) return "multi-command newlines are not supported";
	const rawTopLevelArgv = shellWords(command);
	if (rawTopLevelArgv.some((word, index) => index === 0 && /^[A-Za-z_][A-Za-z0-9_]*=.*/.test(word))) {
		return "command-local environment assignments are not supported";
	}
	const topLevelArgv = stripEnvAssignments(rawTopLevelArgv);
	const shellNames = new Set(["bash", "sh", "dash", "zsh", "ksh", "fish"]);
	for (let index = 0; index < topLevelArgv.length; index++) {
		if (!shellNames.has(path.basename(topLevelArgv[index]))) continue;
		if (topLevelArgv.slice(index + 1).some((argument) => /^-[^-]*c/.test(argument))) {
			return "nested shell evaluation is not supported";
		}
	}
	let quote: "'" | '"' | null = null;
	for (let index = 0; index < command.length; index++) {
		const character = command[index];
		if (character === "\\") {
			index += 1;
			continue;
		}
		if ((character === "'" || character === '"') && quote === null) {
			quote = character;
			continue;
		}
		if (character === quote) {
			quote = null;
			continue;
		}
		if (quote === "'") continue;
		if (character === "$" || character === "`") return "dynamic shell evaluation is not supported";
		if (quote === null && character === "{" && command[index + 1] === "}") {
			index += 1;
			continue;
		}
		if (quote === null && "&;|<>*?[]{}()".includes(character)) {
			return `shell operator or expansion ${JSON.stringify(character)} is not supported`;
		}
	}
	if (quote !== null) return "unterminated shell quoting is not supported";
	return undefined;
}

function parseShellSegments(command: string): Segment[] {
	const rawSegments = splitShell(command, ["&&", "||", ";", "|"]);
	const segments: Segment[] = [];
	for (const raw of rawSegments) {
		const argv = stripEnvAssignments(shellWords(raw.trim()));
		if (argv.length === 0) continue;
		if (argv[0] === "command" && argv[1]) {
			argv.shift();
		}
		if ((argv[0] === "bash" || argv[0] === "sh") && argv[1] === "-c" && argv[2]) {
			segments.push(...parseShellSegments(argv[2]));
			continue;
		}
		segments.push({ raw: raw.trim(), argv });
	}
	return segments;
}

function normalizeExecutableSegment(segment: Segment): { segment?: Segment; reason?: string } {
	const [rawCommand, ...rawArgs] = segment.argv;
	if (!rawCommand) return { reason: "missing executable" };
	const command = path.basename(rawCommand);
	if (rawCommand !== command) {
		return { reason: "executable paths are not supported; use the audited command name" };
	}
	if (command === "env" && rawArgs.length > 0) {
		return { reason: "environment command wrappers are not supported" };
	}
	if (command !== "git" && rawArgs.some((argument) => path.basename(argument) === "git")) {
		return { reason: "nested executable wrappers are not supported" };
	}
	if (command !== "git") return { segment: { ...segment, argv: [command, ...rawArgs] } };

	const args = [...rawArgs];
	while (args[0]?.startsWith("-")) {
		const option = args[0];
		if (["--no-pager", "--paginate", "-P", "-p", "--literal-pathspecs", "--no-literal-pathspecs", "--glob-pathspecs", "--noglob-pathspecs", "--icase-pathspecs"].includes(option)) {
			args.shift();
			continue;
		}
		if (option === "-C" || (option.startsWith("-C") && option.length > 2)) {
			return { reason: "Git -C can retarget authorization to another repository" };
		}
		if (["--git-dir", "--work-tree", "--namespace", "--super-prefix"].includes(option)
			|| ["--git-dir=", "--work-tree=", "--namespace=", "--super-prefix="].some((prefix) => option.startsWith(prefix))) {
			return { reason: `Git option ${JSON.stringify(option)} can retarget authorization` };
		}
		return { reason: `unsupported Git global option ${JSON.stringify(option)}` };
	}
	if (!args[0]) return { reason: "Git subcommand is missing" };
	return { segment: { ...segment, argv: ["git", ...args] } };
}

function unsafeAuditedOptions(segment: Segment): string | undefined {
	const [command, subcommand] = segment.argv;
	const commandArgs = segment.argv.slice(1);
	const gitArgs = segment.argv.slice(2);
	const has = (args: string[], ...options: string[]) => args.some((arg) => options.includes(arg) || options.some((option) => arg.startsWith(`${option}=`)));
	if (command === "git" && ["diff", "log", "show"].includes(subcommand ?? "")
		&& has(gitArgs, "--ext-diff", "--textconv", "--output")) {
		return "Git read options that execute helpers or write output are not supported";
	}
	if (command === "git" && subcommand === "commit"
		&& !gitArgs.some((arg) => arg === "-m" || arg.startsWith("--message=")
			|| arg === "-F" || arg.startsWith("--file=") || arg === "--no-edit")) {
		return "Git commits require an explicit non-interactive message source";
	}
	if (command === "git" && subcommand === "push"
		&& (has(gitArgs, "--receive-pack", "--exec", "--upload-pack", "--repo")
			|| gitArgs.some((arg) => /^[A-Za-z0-9_-]+::/.test(arg)))) {
		return "Git transport helper and repository overrides are not supported";
	}
	if (command === "rg" && has(commandArgs, "--pre", "--pre-glob", "--hostname-bin")) {
		return "ripgrep helper execution is not supported";
	}
	if (["sort", "uniq", "file", "date", "ps", "pgrep", "jq"].includes(command ?? "")) {
		return `${command} is not in Nopal's closed read-command grammar`;
	}
	if (command === "find" && commandArgs.some((arg) => ["-fprint", "-fprint0", "-fprintf", "-fls"].includes(arg))) {
		return "find output-file options are not supported in a read action";
	}
	if (command === "date" && has(commandArgs, "-s", "--set")) {
		return "date mutation options are not supported in a read action";
	}
	return undefined;
}

function isNetworkTransferCommand(command: string | undefined): boolean {
	return Boolean(command && ["curl", "wget", "nc", "netcat", "scp", "rsync", "ftp", "sftp"].includes(command));
}

function externalTargetIsExact(segment: Segment): string | undefined {
	const [command, ...args] = segment.argv;
	if (!isNetworkTransferCommand(command)) return undefined;
	if (command !== "curl") {
		return `${command} has no audited exact-target adapter`;
	}
	if (args.length !== 2 || !["--disable", "-q"].includes(args[0])) {
		return "curl exact-target authorization permits only: curl --disable <literal-http-url>";
	}
	const targetText = args[1];
	if (!/^https?:\/\/[^\s]+$/i.test(targetText) || /[{}\[\]]/.test(targetText)) {
		return "curl requires one non-globbing literal HTTP or HTTPS target";
	}
	try {
		const target = new URL(targetText);
		if (!target.hostname || target.username || target.password) {
			return "curl target credentials and missing hostnames are not supported";
		}
	} catch {
		return "curl target is not a valid literal URL";
	}
	return undefined;
}

function isCredentialArgument(arg: string): boolean {
	const normalized = arg.replace(/^@+/, "");
	if (/^[a-z][a-z0-9+.-]*:\/\//i.test(normalized)) return false;
	return isProtectedCredentialPath(normalized);
}

// ---------------------------------------------------------------------------
// Credential/secret command classification (checked before the rule table)
// ---------------------------------------------------------------------------

function classifyNetworkCredentialExfil(command: string): Classification | null {
	const segments = parseShellSegments(command);
	const hasNetworkTransfer = segments.some((segment) => isNetworkTransferCommand(segment.argv[0]));
	if (!hasNetworkTransfer) return null;

	for (const segment of segments) {
		const [cmd, ...args] = segment.argv;
		if (!cmd) continue;
		if (args.some(isCredentialArgument)) return { action: "network.exfil_credential", class: "secret_bearing" };
		if (["cat", "less", "more", "head", "tail"].includes(cmd) && args.some(isCredentialArgument)) {
			return { action: "network.exfil_credential", class: "secret_bearing" };
		}
		if (["env", "printenv", "set", "declare"].includes(cmd)) {
			return { action: "network.exfil_env", class: "secret_bearing" };
		}
	}
	return null;
}

function classifyCredentialCommand(segment: Segment): Classification | null {
	const [cmd, ...args] = segment.argv;
	if (!cmd) return null;
	if (["env", "printenv", "set", "declare"].includes(cmd)) {
		return { action: "env.dump", class: "secret_bearing" };
	}
	if (cmd === "export" && args.length === 0) {
		return { action: "export.bare", class: "secret_bearing" };
	}
	if (["echo", "printf"].includes(cmd) && SECRET_KEY_RE.test(segment.raw) && /[$]/.test(segment.raw)) {
		return { action: "echo.secret_var", class: "secret_bearing" };
	}
	if (["cat", "less", "more", "head", "tail"].includes(cmd) && args.some(isCredentialArgument)) {
		return { action: "file.read_credential", class: "secret_bearing" };
	}
	if (["grep", "rg", "awk", "sed"].includes(cmd) && args.some((arg) => SECRET_KEY_RE.test(arg)) && args.some(isCredentialArgument)) {
		return { action: "file.search_credential", class: "secret_bearing" };
	}
	return null;
}

// ---------------------------------------------------------------------------
// Command-shape rule table. Every emitted class and action belongs to Core's
// closed vocabulary. Unknown or ambiguous command shapes stop before policy,
// and infrastructure failures remain non-approvable.
// ---------------------------------------------------------------------------

const HIGH_SEVERITY_RULES: ClassRule[] = [
	{
		id: "nopal-enforcement-internal",
		command: "nopal",
		argsPrefixAny: [["enforcement"], ["--json", "enforcement"]],
		class: "destructive",
		action: "nopal.enforcement_internal",
	},
	{ id: "git-reset-hard", command: "git", argsPrefixAny: [["reset", "--hard"]], class: "destructive", action: "git.reset_hard" },
	{
		id: "git-clean-force",
		command: "git",
		argsPrefixAny: [["clean", "-f"], ["clean", "-fd"], ["clean", "-df"], ["clean", "--force"]],
		class: "destructive",
		action: "git.clean_force",
	},
	{
		id: "git-push",
		command: "git",
		argsPrefixAny: [["push"]],
		class: "git_remote",
		action: (segment) =>
			segment.argv.slice(2).some((arg) =>
				arg === "-f"
				|| arg === "--force"
				|| /^-[^-]*f/.test(arg)
				|| arg.startsWith("--force-with-lease")
				|| arg === "--force-if-includes"
				|| arg === "--mirror"
				|| arg === "--delete"
				|| arg === "-d"
				|| /^-[^-]*d/.test(arg)
				|| arg === "--prune"
				|| arg.startsWith("+")
				|| arg.startsWith(":")
				|| /[$`*?\[]/.test(arg)
			)
				? "git.push_force"
				: "git.push",
	},
	{
		id: "dependency-install",
		command: ["npm", "pnpm", "yarn", "bun", "pip", "pip3", "cargo", "gem", "composer"],
		argsPrefixAny: [["install"], ["ci"], ["add"], ["update"]],
		class: "dependency_install",
		action: "dependency.install",
	},
	{ id: "git-add", command: "git", argsPrefixAny: [["add"]], class: "git_local", action: "git.add" },
	{ id: "git-commit", command: "git", argsPrefixAny: [["commit"]], class: "git_local", action: "git.commit" },
	{
		id: "rm-recursive",
		command: "rm",
		argsPrefixAny: [["-rf"], ["-fr"], ["-r"], ["-R"], ["--recursive"]],
		class: "destructive",
		action: "rm.recursive",
	},
	{ id: "sudo", command: "sudo", argsPrefixAny: [[]], class: "destructive", action: "sudo.exec" },
	{ id: "tmux-environment", command: "tmux", argsPrefixAny: [["show-environment"]], class: "secret_bearing", action: "env.dump" },
	{ id: "chmod-777", command: "chmod", argsPrefixAny: [["777"], ["-R", "777"]], class: "destructive", action: "chmod.perm_777" },
	{ id: "chown", command: "chown", argsPrefixAny: [[]], class: "destructive", action: "chown.exec" },
	{ id: "raw-disk", command: ["dd", "mkfs", "mkfs.ext4", "mkfs.xfs"], argsPrefixAny: [[]], class: "destructive", action: "disk.raw_write" },
	{ id: "shutdown", command: ["shutdown", "reboot", "poweroff"], argsPrefixAny: [[]], class: "destructive", action: "system.shutdown" },
	{ id: "find-delete", command: "find", argsContainsAny: ["-delete"], class: "destructive", action: "find.delete" },
	{ id: "find-exec", command: "find", argsContainsAny: ["-exec", "-execdir", "-ok", "-okdir"], class: "destructive", action: "find.exec" },
	{
		id: "gh-pr-mutations",
		command: "gh",
		argsPrefixAny: [["pr", "create"], ["pr", "merge"], ["pr", "review"], ["pr", "edit"], ["pr", "comment"]],
		class: "git_remote",
		action: "gh.pr_mutate",
	},
	{
		id: "gh-issue-mutations",
		command: "gh",
		argsPrefixAny: [["issue", "create"], ["issue", "edit"], ["issue", "comment"], ["issue", "close"], ["issue", "reopen"]],
		class: "git_remote",
		action: "gh.issue_mutate",
	},
	{ id: "gh-api-mutations", command: "gh", argsPrefixAny: [["api"]], class: "unknown", action: "gh.api" },
	{
		id: "wrangler-mutations",
		command: "wrangler",
		argsPrefixAny: [
			["deploy"],
			["delete"],
			["d1", "execute"],
			["kv", "key", "put"],
			["kv", "key", "delete"],
			["r2", "object", "put"],
			["r2", "object", "delete"],
		],
		class: "network_write",
		action: "wrangler.mutate",
	},
	{
		id: "terraform-mutations",
		command: ["terraform", "tofu"],
		argsPrefixAny: [["apply"], ["destroy"]],
		class: "network_write",
		action: "terraform.mutate",
	},
	{
		id: "kubectl-mutations",
		command: "kubectl",
		argsPrefixAny: [["apply"], ["delete"], ["patch"], ["scale"], ["rollout"]],
		class: "network_write",
		action: "kubectl.mutate",
	},
	{ id: "deploy-tools", command: ["fly", "vercel"], argsPrefixAny: [["deploy"], ["remove"], ["delete"]], class: "network_write", action: "deploy.mutate" },
	{
		id: "network-transfer",
		command: ["curl", "wget", "nc", "netcat", "scp", "rsync", "ftp", "sftp"],
		argsPrefixAny: [[]],
		class: "network_write",
		action: "network.transfer",
	},
	{ id: "aws-ambiguous", command: "aws", argsPrefixAny: [[]], class: "unknown", action: "aws.exec" },
];

const ROUTINE_RULES: ClassRule[] = [
	{
		id: "git-read-only",
		command: "git",
		argsPrefixAny: [["status"], ["diff"], ["log"], ["show"], ["branch", "--show-current"]],
		class: "git_local",
		action: "git.read",
	},
	{
		id: "gh-read-only",
		command: "gh",
		argsPrefixAny: [["pr", "view"], ["pr", "diff"], ["pr", "list"], ["pr", "status"], ["issue", "view"], ["issue", "list"], ["run", "view"], ["run", "list"]],
		class: "network_read",
		action: "gh.read",
	},
	{
		id: "filesystem-read-only",
		command: ["pwd", "uname", "whoami", "id"],
		argsPrefixAny: [[]],
		class: "read",
		action: "fs.read",
	},
];

function matchesRule(segment: Segment, rule: ClassRule): boolean {
	const [command, ...args] = segment.argv;
	if (!command) return false;
	const commands = Array.isArray(rule.command) ? rule.command : rule.command ? [rule.command] : [];
	if (commands.length > 0 && !commands.includes(command)) return false;
	if (rule.argsContainsAny && !rule.argsContainsAny.some((arg) => args.includes(arg))) return false;
	if (!rule.argsPrefixAny) return true;
	return rule.argsPrefixAny.some((prefix) => argsPrefixMatches(args, prefix));
}

function argsPrefixMatches(args: string[], prefix: string[]): boolean {
	if (prefix.length === 0) return true;
	if (args.length < prefix.length) return false;
	return prefix.every((part, index) => args[index] === part);
}

function firstMatchingRule(segment: Segment, rules: ClassRule[]): ClassRule | null {
	return rules.find((rule) => matchesRule(segment, rule)) ?? null;
}

function resolveAction(action: ClassRule["action"], segment: Segment): string {
	return typeof action === "function" ? action(segment) : action;
}

/**
 * Classify one supported bash envelope into Core's closed action vocabulary.
 * Never returns a decision. Unmatched or incomplete commands carry an
 * `unknown` diagnostic projection here, and Core rejects that projection as
 * an unapprovable contract error.
 */
export function classifyBashCommandSet(command: string): CommandClassifications {
	const unsupported = unsupportedShellSyntax(command);
	if (unsupported) return { classifications: [], complete: false, reason: unsupported };
	const inspectionCommand = stripHeredocsAndProse(command);
	const parsedSegments = parseShellSegments(inspectionCommand);
	if (parsedSegments.length > 1) {
		return { classifications: [], complete: false, reason: "multiple executable shell segments are not supported" };
	}
	const normalized = parsedSegments.map(normalizeExecutableSegment);
	const normalizationFailure = normalized.find((entry) => entry.reason)?.reason;
	if (normalizationFailure) {
		return { classifications: [], complete: false, reason: normalizationFailure };
	}
	const segments = normalized.flatMap((entry) => entry.segment ? [entry.segment] : []);
	if (segments.length > 1) {
		return { classifications: [], complete: false, reason: "multiple executable shell segments are not supported" };
	}
	if (segments.length === 0) {
		return { classifications: [], complete: false, reason: "no executable shell segment was found" };
	}
	const unsafeOption = segments.map(unsafeAuditedOptions).find((reason) => reason !== undefined);
	if (unsafeOption) return { classifications: [], complete: false, reason: unsafeOption };
	const targetFailure = segments.map(externalTargetIsExact).find((reason) => reason !== undefined);
	if (targetFailure) return { classifications: [], complete: false, reason: targetFailure };

	const classifications: Classification[] = [];
	const exfil = classifyNetworkCredentialExfil(inspectionCommand);
	if (exfil) classifications.push(exfil);

	for (const segment of segments) {
		const credential = classifyCredentialCommand(segment);
		if (credential) {
			classifications.push(credential);
			continue;
		}
		const highSeverity = firstMatchingRule(segment, HIGH_SEVERITY_RULES);
		if (highSeverity) {
			classifications.push({ action: resolveAction(highSeverity.action, segment), class: highSeverity.class });
			continue;
		}
		const routine = firstMatchingRule(segment, ROUTINE_RULES);
		if (routine) {
			classifications.push({ action: resolveAction(routine.action, segment), class: routine.class });
			continue;
		}
		const [cmd] = segment.argv;
		classifications.push({ action: cmd ? `${cmd}.exec` : "bash.exec", class: "unknown" });
	}

	return {
		classifications: classifications.filter((candidate, index, all) =>
			all.findIndex((entry) => entry.action === candidate.action && entry.class === candidate.class) === index
		),
		complete: true,
	};
}

function canonicalJson(value: unknown): string {
	if (value === undefined) return JSON.stringify("<undefined>");
	if (value === null || typeof value !== "object") return JSON.stringify(value);
	if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
	const entries = Object.entries(value as Record<string, unknown>)
		.sort(([left], [right]) => left.localeCompare(right));
	return `{${entries.map(([key, child]) => `${JSON.stringify(key)}:${canonicalJson(child)}`).join(",")}}`;
}

function digest(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function shellArgumentsAreConfined(command: string, cwd: string, projectRoot: string): string | undefined {
	for (const raw of parseShellSegments(command)) {
		const normalized = normalizeExecutableSegment(raw);
		if (!normalized.segment) continue;
		for (const argument of normalized.segment.argv.slice(1)) {
			if (argument.startsWith("~")) return `shell path expansion escapes exact target binding: ${argument}`;
			const checked = confinedToolPath(argument, cwd, projectRoot);
			if (checked.reason) return checked.reason;
		}
	}
	return undefined;
}

function confinedToolPath(candidate: string, cwd: string, projectRoot: string): { absolute?: string; relative?: string; reason?: string } {
	const root = resolveThroughExistingAncestor(path.resolve(projectRoot)) ?? path.resolve(projectRoot);
	const lexical = path.resolve(cwd, candidate.replace(/^@/, ""));
	const resolved = resolveThroughExistingAncestor(lexical) ?? lexical;
	if (!isWithin(resolved, root)) return { reason: `tool path escapes the Nopal worktree: ${candidate}` };
	const relative = path.relative(root, resolved).split(path.sep).join("/") || ".";
	return { absolute: resolved, relative };
}

/**
 * Compile one real Pi tool call into the closed Nopal intent vocabulary.
 * Unknown tools and incomplete input fail before policy, so a human approval
 * can never turn missing mediation support into authority.
 */
export function classifyPiToolCall(
	toolName: string,
	input: Record<string, unknown>,
	cwd: string,
	projectRoot: string,
): ToolIntentResult {
	const inputDigest = digest(canonicalJson(input));
	if (toolName === "bash") {
		const command = input.command;
		if (typeof command !== "string" || !command.trim()) {
			return { complete: false, reason: "bash input requires one non-empty command" };
		}
		const classified = classifyBashCommandSet(command);
		if (!classified.complete || classified.classifications.length !== 1) {
			return { complete: false, reason: classified.reason ?? "bash command did not produce one stable action" };
		}
		const confinementFailure = shellArgumentsAreConfined(command, cwd, projectRoot);
		if (confinementFailure) return { complete: false, reason: confinementFailure };
		const classification = classified.classifications[0];
		return {
			complete: true,
			intent: {
				kind: "nopal.enforcement.tool_intent/v1",
				toolName,
				...classification,
				inputDigest,
				targetDigest: digest(command),
				changedFiles: [],
				mutates: !["read", "network_read"].includes(classification.class),
			},
		};
	}

	if (!["read", "grep", "find", "ls", "write", "edit"].includes(toolName)) {
		return { complete: false, reason: `unsupported Pi tool ${JSON.stringify(toolName)}` };
	}
	const rawPath = input.path ?? ".";
	if (typeof rawPath !== "string" || !rawPath.trim()) {
		return { complete: false, reason: `${toolName} input requires a path string` };
	}
	const confined = confinedToolPath(rawPath, cwd, projectRoot);
	if (!confined.absolute || !confined.relative) return { complete: false, reason: confined.reason };
	const mutates = toolName === "write" || toolName === "edit";
	if (mutates && isProtectedCredentialPath(confined.absolute)) {
		return { complete: false, reason: `resolved mutation target is a protected credential path: ${rawPath}` };
	}
	const credentialRead = !mutates && isProtectedCredentialPath(confined.absolute);
	return {
		complete: true,
		intent: {
			kind: "nopal.enforcement.tool_intent/v1",
			toolName,
			action: credentialRead ? "file.read_credential" : mutates ? "fs.write" : "fs.read",
			class: credentialRead ? "secret_bearing" : mutates ? "workspace_write" : "read",
			inputDigest,
			targetDigest: digest(confined.absolute),
			changedFiles: mutates && confined.relative !== "." ? [confined.relative] : [],
			mutates,
		},
	};
}

/**
 * Compatibility projection for callers that can consume only one action.
 * Enforcement uses `classifyBashCommandSet`, which rejects compound or
 * unsupported shell syntax before returning its complete classifications.
 * This projection exists for diagnostics and must not authorize a command.
 */
export function classifyBashCommand(command: string): Classification {
	const result = classifyBashCommandSet(command);
	if (!result.complete || result.classifications.length === 0) {
		return { action: "bash.exec", class: "unknown" };
	}
	const priority = ["nopal.enforcement_internal", "git.push_force"];
	for (const action of priority) {
		const match = result.classifications.find((candidate) => candidate.action === action);
		if (match) return match;
	}
	for (const className of ["destructive", "secret_bearing", "network_write", "git_remote", "git_local", "network_read", "read", "unknown"]) {
		const match = result.classifications.find((candidate) => candidate.class === className);
		if (match) return match;
	}
	return result.classifications[0];
}
