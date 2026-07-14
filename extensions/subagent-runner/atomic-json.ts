import { mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

export function writeAtomicJson(path: string, value: unknown): void {
	mkdirSync(dirname(path), { recursive: true });
	const tempPath = join(dirname(path), `.${path.split(/[\\/]/).at(-1)}.${process.pid}.${Date.now()}.tmp`);
	try {
		writeFileSync(tempPath, `${JSON.stringify(value, null, 2)}\n`, "utf-8");
		renameSync(tempPath, path);
	} catch (error) {
		try {
			rmSync(tempPath, { force: true });
		} catch {
			// Best effort cleanup.
		}
		throw error;
	}
}
