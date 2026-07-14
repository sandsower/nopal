/**
 * Pure classification and protected-floor logic for policy-gate.
 *
 * Ported from the legacy safety-gate extension
 * (`~/.pi/agent/git/github.com/sandsower/pi-extensions/extensions/safety-gate.ts`):
 * the shell-segment parsing, credential-path list, and command-shape rule
 * matching are the same proven mechanism. The difference is what a match
 * produces: safety-gate resolved a decision (allow/confirm/block) locally;
 * this module only classifies a bash command into an open-vocab
 * `{ action, class }` token pair. The actual allow/deny/ask decision is made
 * by `nopal policy decide` (see `nopal-cli.ts`), which this module never
 * calls or imports.
 *
 * No pi imports here: this file (and its tests) must run as plain node
 * modules with no extension host and no subprocess.
 */

export type Classification = {
	action: string;
	class: string;
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
// Protected floors: local, deterministic, never routed through nopal policy
// decide. These stay active even when the policy-gate roundtrip is toggled
// off (see index.ts's `/policy-gate off`).
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

function isNetworkTransferCommand(command: string | undefined): boolean {
	return Boolean(command && ["curl", "wget", "nc", "netcat", "scp", "rsync", "ftp", "sftp"].includes(command));
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
// Command-shape rule table (ported from safety-gate's builtInPolicy, with
// decisions replaced by open-vocab class tokens). `secret_bearing` and
// `read`/`workspace_write`/`network_read`/`git_local`/`git_remote`/
// `destructive` are nopal-core's known vocabulary
// (crates/nopal-core/src/policy.rs KNOWN_CLASSES); `network_write` is an
// additional open-vocab token this classifier mints for remote/infra
// mutations that don't fit any known class - unknown classes fail closed in
// nopal-core (treated as protected/unsafe), which is the conservative
// behavior we want for these until a project's `.nopal/policy.jsonc`
// explicitly configures a rule for them.
// ---------------------------------------------------------------------------

const HIGH_SEVERITY_RULES: ClassRule[] = [
	{ id: "git-reset-hard", command: "git", argsPrefixAny: [["reset", "--hard"]], class: "destructive", action: "git.reset_hard" },
	{
		id: "git-clean-force",
		command: "git",
		argsPrefixAny: [["clean", "-f"], ["clean", "-fd"], ["clean", "-df"], ["clean", "--force"]],
		class: "destructive",
		action: "git.clean_force",
	},
	{ id: "git-push", command: "git", argsPrefixAny: [["push"]], class: "git_remote", action: "git.push" },
	{ id: "git-commit", command: "git", argsPrefixAny: [["commit"]], class: "git_local", action: "git.commit" },
	{
		id: "rm-recursive",
		command: "rm",
		argsPrefixAny: [["-rf"], ["-fr"], ["-r"], ["-R"], ["--recursive"]],
		class: "destructive",
		action: "rm.recursive",
	},
	{ id: "sudo", command: "sudo", argsPrefixAny: [[]], class: "destructive", action: "sudo.exec" },
	{ id: "chmod-777", command: "chmod", argsPrefixAny: [["777"], ["-R", "777"]], class: "destructive", action: "chmod.perm_777" },
	{ id: "chown", command: "chown", argsPrefixAny: [[]], class: "destructive", action: "chown.exec" },
	{ id: "raw-disk", command: ["dd", "mkfs", "mkfs.ext4", "mkfs.xfs"], argsPrefixAny: [[]], class: "destructive", action: "disk.raw_write" },
	{ id: "shutdown", command: ["shutdown", "reboot", "poweroff"], argsPrefixAny: [[]], class: "destructive", action: "system.shutdown" },
	{ id: "find-delete", command: "find", argsContainsAny: ["-delete"], class: "destructive", action: "find.delete" },
	{ id: "find-exec", command: "find", argsContainsAny: ["-exec", "-execdir"], class: "destructive", action: "find.exec" },
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
		command: ["ls", "pwd", "rg", "grep", "head", "tail", "wc", "sort", "uniq", "date", "jq", "file", "stat", "du", "df", "uname", "whoami", "id", "ps", "pgrep", "find"],
		argsPrefixAny: [[]],
		class: "read",
		action: "fs.read",
	},
	{
		id: "tmux-read-only",
		command: "tmux",
		argsPrefixAny: [["list-sessions"], ["ls"], ["list-windows"], ["list-panes"], ["display-message"], ["show-options"], ["show-environment"]],
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
 * Classify a bash command into an open-vocab `{ action, class }` token pair.
 * Never returns a decision; the caller routes the result through
 * `nopal policy decide` (nopal-cli.ts) to get an allow/deny/ask verdict.
 *
 * Unmatched/unclassifiable commands get `class: "unknown"` rather than an
 * implicit allow - this is the key behavior change from safety-gate, whose
 * default for an unmatched command was `allow`. Under policy-gate, the
 * default is "let the core decide", and the core treats unknown classes as
 * protected/unsafe.
 */
export function classifyBashCommand(command: string): Classification {
	const inspectionCommand = stripHeredocsAndProse(command);

	const exfil = classifyNetworkCredentialExfil(inspectionCommand);
	if (exfil) return exfil;

	const segments = parseShellSegments(inspectionCommand);
	if (segments.length === 0) return { action: "bash.exec", class: "unknown" };

	for (const segment of segments) {
		const credential = classifyCredentialCommand(segment);
		if (credential) return credential;
	}

	for (const segment of segments) {
		const rule = firstMatchingRule(segment, HIGH_SEVERITY_RULES);
		if (rule) return { action: resolveAction(rule.action, segment), class: rule.class };
	}

	for (const segment of segments) {
		const rule = firstMatchingRule(segment, ROUTINE_RULES);
		if (rule) return { action: resolveAction(rule.action, segment), class: rule.class };
	}

	const [cmd] = segments[0].argv;
	return { action: cmd ? `${cmd}.exec` : "bash.exec", class: "unknown" };
}
