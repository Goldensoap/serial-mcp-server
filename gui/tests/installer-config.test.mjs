import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const guiDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..");

test("the MSI adds the CLI and MCP sidecar directory to the system PATH", async () => {
  const config = JSON.parse(
    await readFile(resolve(guiDirectory, "src-tauri", "tauri.conf.json"), "utf8"),
  );
  const wix = await readFile(
    resolve(guiDirectory, "src-tauri", "wix", "path.wxs"),
    "utf8",
  );

  assert.deepEqual(config.bundle.windows.wix.fragmentPaths, ["wix/path.wxs"]);
  assert.deepEqual(config.bundle.windows.wix.componentRefs, [
    "AddInstallDirToPath",
  ]);
  assert.match(wix, /Name="PATH"/);
  assert.match(wix, /Value="\[INSTALLDIR\]"/);
  assert.match(wix, /Part="last"/);
  assert.match(wix, /Permanent="no"/);
  assert.match(wix, /System="yes"/);
});
