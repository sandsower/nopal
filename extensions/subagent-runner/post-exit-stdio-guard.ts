import type { ChildProcess } from "node:child_process";

export type PostExitStdioGuardOptions = {
	idleMs: number;
	hardMs: number;
};

type ChildWithPipedStdio = Pick<ChildProcess, "stdout" | "stderr" | "on">;

export function attachPostExitStdioGuard(
	child: ChildWithPipedStdio,
	options: PostExitStdioGuardOptions = { idleMs: 1000, hardMs: 5000 },
): () => void {
	const { idleMs, hardMs } = options;
	let exited = false;
	let stdoutEnded = false;
	let stderrEnded = false;
	let idleTimer: ReturnType<typeof setTimeout> | undefined;
	let hardTimer: ReturnType<typeof setTimeout> | undefined;

	const destroyUnendedStdio = () => {
		if (!stdoutEnded) {
			try { child.stdout?.destroy(); } catch {}
		}
		if (!stderrEnded) {
			try { child.stderr?.destroy(); } catch {}
		}
	};

	const clearTimers = () => {
		if (idleTimer) {
			clearTimeout(idleTimer);
			idleTimer = undefined;
		}
		if (hardTimer) {
			clearTimeout(hardTimer);
			hardTimer = undefined;
		}
	};

	const armIdleTimer = () => {
		if (!exited) return;
		if (idleTimer) clearTimeout(idleTimer);
		idleTimer = setTimeout(destroyUnendedStdio, idleMs);
		idleTimer.unref?.();
	};

	child.stdout?.on("data", armIdleTimer);
	child.stderr?.on("data", armIdleTimer);
	child.stdout?.on("end", () => {
		stdoutEnded = true;
		if (stdoutEnded && stderrEnded) clearTimers();
	});
	child.stderr?.on("end", () => {
		stderrEnded = true;
		if (stdoutEnded && stderrEnded) clearTimers();
	});
	child.on("exit", () => {
		exited = true;
		armIdleTimer();
		if (hardTimer) return;
		hardTimer = setTimeout(destroyUnendedStdio, hardMs);
		hardTimer.unref?.();
	});
	child.on("close", clearTimers);
	child.on("error", clearTimers);

	return clearTimers;
}
