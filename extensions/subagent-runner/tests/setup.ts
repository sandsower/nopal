/**
 * Test-only loader for pure modules that internally import a sibling
 * module with a `.js` specifier (the pi idiom - see `ts-loader.mjs` for
 * why plain `node --test` needs help resolving that). Modules with no
 * internal relative imports (e.g. `subagent-child-runtime.ts`) don't need
 * this and can be imported statically as usual.
 *
 * Node resolves a module's static imports at link time, before any of its
 * top-level code (including a prior static import's side effects) runs.
 * So the resolve hook must be registered, and the target module then
 * loaded via dynamic `import()`, in that order - a plain static import
 * would still fail even after another module registers the hook.
 */
import { register } from "node:module";

let registered = false;

function ensureLoaderRegistered(): void {
	if (registered) return;
	registered = true;
	register(new URL("./ts-loader.mjs", import.meta.url), import.meta.url);
}

/** Dynamically import a sibling module (relative to this file) after registering the `.js`-to-`.ts` resolve hook. */
export async function loadSubagentRunnerModule<T>(relativePath: string): Promise<T> {
	ensureLoaderRegistered();
	return (await import(relativePath)) as T;
}
