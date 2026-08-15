import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const guiDirectory = resolve(scriptDirectory, "..");
const repositoryDirectory = resolve(guiDirectory, "..");
const manifestPath = join(repositoryDirectory, "Cargo.toml");

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: repositoryDirectory,
    encoding: "utf8",
    stdio: options.capture ? "pipe" : "inherit",
  });

  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    const details = options.capture ? `\n${result.stderr || result.stdout}` : "";
    throw new Error(`${command} exited with status ${result.status}${details}`);
  }
  return result.stdout;
}

const rustcVersion = run("rustc", ["-vV"], { capture: true });
const hostLine = rustcVersion
  .split(/\r?\n/)
  .find((line) => line.startsWith("host: "));
if (!hostLine) {
  throw new Error("Could not determine the Rust host target triple");
}
const targetTriple = hostLine.slice("host: ".length).trim();

run("cargo", [
  "build",
  "--release",
  "--locked",
  "--manifest-path",
  manifestPath,
  "--bin",
  "serial-mcp-server",
]);

const metadata = JSON.parse(
  run(
    "cargo",
    ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", manifestPath],
    { capture: true },
  ),
);
const executableSuffix = process.platform === "win32" ? ".exe" : "";
const source = join(
  metadata.target_directory,
  "release",
  `serial-mcp-server${executableSuffix}`,
);
const destination = join(
  guiDirectory,
  "src-tauri",
  "binaries",
  `serial-mcp-server-${targetTriple}${executableSuffix}`,
);

mkdirSync(dirname(destination), { recursive: true });
copyFileSync(source, destination);
console.log(`Prepared Tauri sidecar: ${destination}`);
