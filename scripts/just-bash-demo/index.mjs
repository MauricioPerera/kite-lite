// Registers kite-lite as a custom command inside vercel-labs/just-bash's
// sandboxed shell (https://github.com/vercel-labs/just-bash), giving that
// simulated bash real web-browsing/rendering — kite-lite runs as a genuine
// subprocess, and its stdout composes with just-bash's own builtins (jq,
// grep, sed...) through normal pipes.
//
// Caveat: file-path arguments (page.json / *.html for webmcp-lint,
// a11y-lint, social-lint) resolve against the REAL host filesystem, not
// just-bash's virtual one — kite-lite has no idea it's being called from
// inside a sandbox. Only fine for URL-based invocations if you actually
// need the filesystem isolation just-bash otherwise provides.
//
// Usage:
//   npm install
//   node index.mjs                                  # runs the default demo pipeline
//   node index.mjs "kite-lite fetch <url> | jq '.title'"
//
// Env override: KITE_LITE_BIN (path to the kite-lite binary).

import { Bash, defineCommand } from "just-bash";
import { execFile } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "..", "..");
const defaultBinary = path.join(
  repoRoot,
  "target",
  "release",
  process.platform === "win32" ? "kite-lite.exe" : "kite-lite",
);
const KITE_LITE_BIN = process.env.KITE_LITE_BIN || defaultBinary;

const kiteLite = defineCommand("kite-lite", async (args) => {
  try {
    const { stdout, stderr } = await execFileAsync(KITE_LITE_BIN, args, {
      timeout: 30000,
      maxBuffer: 20 * 1024 * 1024,
    });
    return { stdout, stderr, exitCode: 0 };
  } catch (error) {
    return {
      stdout: error.stdout ?? "",
      stderr: error.stderr ?? String(error.message ?? error),
      exitCode: typeof error.code === "number" ? error.code : 1,
    };
  }
});

const bash = new Bash({ customCommands: [kiteLite] });

const script =
  process.argv.slice(2).join(" ") ||
  "kite-lite fetch https://example.com | jq '.title, .links'";

console.log(`$ ${script}`);
const result = await bash.exec(script);
process.stdout.write(result.stdout);
if (result.stderr) process.stderr.write(result.stderr);
console.log(`(exit code: ${result.exitCode})`);
