import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import {
  mkdtemp,
  mkdir,
  readFile,
  rm,
  symlink,
  unlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { fileURLToPath } from "node:url";

const guiDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const execFileAsync = promisify(execFile);

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
    "CleanupRuntimeDataScript",
  ]);
  assert.match(wix, /Name="PATH"/);
  assert.match(wix, /Value="\[INSTALLDIR\]"/);
  assert.match(wix, /Part="last"/);
  assert.match(wix, /Permanent="no"/);
  assert.match(wix, /System="yes"/);
});

test("the MSI removes runtime data for every local profile on uninstall", async () => {
  const config = JSON.parse(
    await readFile(resolve(guiDirectory, "src-tauri", "tauri.conf.json"), "utf8"),
  );
  const wix = await readFile(
    resolve(guiDirectory, "src-tauri", "wix", "path.wxs"),
    "utf8",
  );
  const cleanupScript = await readFile(
    resolve(
      guiDirectory,
      "src-tauri",
      "wix",
      "cleanup-runtime-data.ps1",
    ),
    "utf8",
  );

  assert.deepEqual(config.bundle.windows.wix.componentRefs, [
    "AddInstallDirToPath",
    "CleanupRuntimeDataScript",
  ]);
  assert.match(wix, /Id="CleanupRuntimeData"/);
  assert.match(wix, /Id="CleanupRuntimeDataScript"/);
  assert.match(wix, /Name="\.uninstall"/);
  assert.match(wix, /<PermissionEx/);
  assert.match(wix, /Sddl="D:PAI/);
  assert.match(wix, /cleanup-runtime-data\.ps1/);
  assert.match(wix, /Execute="deferred"/);
  assert.match(wix, /Impersonate="no"/);
  assert.match(wix, /Return="check"/);
  assert.match(wix, /Before="RemoveFiles"/);
  assert.match(wix, /REMOVE="ALL" AND NOT UPGRADINGPRODUCTCODE/);

  assert.match(cleanupScript, /ProfileList/);
  assert.match(cleanupScript, /serial-mcp-server/);
  assert.match(cleanupScript, /dev\.serial-mcp\.console/);
  assert.match(cleanupScript, /Remove-ContainedTree/);
  assert.match(cleanupScript, /outside the runtime data allowlist/);
  assert.match(cleanupScript, /FileAttributes]::ReparsePoint/);
  assert.match(cleanupScript, /Do not add paths supplied by/);
  assert.match(cleanupScript, /Unable to remove runtime data directory/);
});

test(
  "the uninstall cleanup recursively removes both runtime data trees",
  { skip: process.platform !== "win32" },
  async () => {
    const testRoot = await mkdtemp(resolve(tmpdir(), "serial-mcp-uninstall-"));
    const installDirectory = resolve(testRoot, "install");
    const profileRoot = resolve(testRoot, "profile");
    const unrelatedDirectory = resolve(
      profileRoot,
      "AppData",
      "Local",
      "unrelated-application",
    );
    const unrelatedFile = resolve(unrelatedDirectory, "must-survive.txt");
    const eventDirectory = resolve(
      profileRoot,
      "AppData",
      "Local",
      "serial-mcp-server",
      "nested",
    );
    const webViewDirectory = resolve(
      profileRoot,
      "AppData",
      "Local",
      "dev.serial-mcp.console",
      "EBWebView",
      "Default",
    );

    try {
      await mkdir(installDirectory, { recursive: true });
      await mkdir(eventDirectory, { recursive: true });
      await mkdir(webViewDirectory, { recursive: true });
      await mkdir(unrelatedDirectory, { recursive: true });
      await writeFile(resolve(eventDirectory, "events.jsonl"), "{}\n");
      await writeFile(resolve(webViewDirectory, "Cache_Data"), "cache");
      await writeFile(unrelatedFile, "untouched");

      await execFileAsync("powershell.exe", [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        resolve(
          guiDirectory,
          "src-tauri",
          "wix",
          "cleanup-runtime-data.ps1",
        ),
        "-InstallDirectory",
        installDirectory,
        "-ProfileRoots",
        profileRoot,
      ]);

      await assert.rejects(
        readFile(
          resolve(profileRoot, "AppData", "Local", "serial-mcp-server"),
        ),
        { code: "ENOENT" },
      );
      assert.equal(await readFile(unrelatedFile, "utf8"), "untouched");
      await assert.rejects(
        readFile(
          resolve(profileRoot, "AppData", "Local", "dev.serial-mcp.console"),
        ),
        { code: "ENOENT" },
      );
    } finally {
      await rm(testRoot, { recursive: true, force: true });
    }
  },
);

test(
  "the uninstall cleanup refuses reparse points without touching their targets",
  { skip: process.platform !== "win32" },
  async () => {
    const testRoot = await mkdtemp(resolve(tmpdir(), "serial-mcp-reparse-"));
    const installDirectory = resolve(testRoot, "install");
    const profileRoot = resolve(testRoot, "profile");
    const runtimeDirectory = resolve(
      profileRoot,
      "AppData",
      "Local",
      "serial-mcp-server",
    );
    const externalDirectory = resolve(testRoot, "must-not-be-touched");
    const externalFile = resolve(externalDirectory, "sentinel.txt");
    const junctionPath = resolve(runtimeDirectory, "external-junction");

    try {
      await mkdir(installDirectory, { recursive: true });
      await mkdir(runtimeDirectory, { recursive: true });
      await mkdir(externalDirectory, { recursive: true });
      await writeFile(externalFile, "untouched");
      await symlink(externalDirectory, junctionPath, "junction");

      await assert.rejects(
        execFileAsync("powershell.exe", [
          "-NoLogo",
          "-NoProfile",
          "-NonInteractive",
          "-ExecutionPolicy",
          "Bypass",
          "-File",
          resolve(
            guiDirectory,
            "src-tauri",
            "wix",
            "cleanup-runtime-data.ps1",
          ),
          "-InstallDirectory",
          installDirectory,
          "-ProfileRoots",
          profileRoot,
        ]),
      );

      assert.equal(await readFile(externalFile, "utf8"), "untouched");
    } finally {
      await unlink(junctionPath).catch(() => {});
      await rm(testRoot, { recursive: true, force: true });
    }
  },
);
