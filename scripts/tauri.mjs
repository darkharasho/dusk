// Run the Tauri CLI with a pkg-config that can actually see the system GTK.
//
// Why this exists rather than `tauri` straight from package.json: a
// Homebrew-on-Linux earlier on PATH than /usr/bin shadows the system
// pkg-config, and Homebrew's tree has no X11 or javascriptcoregtk .pc files.
// The build then fails claiming cairo "was not found" while cairo is plainly
// installed, which sends you looking for a missing package that isn't missing.
// Developers should not have to know that; `npm run dev` should just work.
//
// The check is a probe, not an assumption: it only overrides pkg-config when
// the one on PATH cannot resolve what Tauri needs AND /usr/bin/pkg-config can.
// So a healthy machine is untouched, as is anyone who set PKG_CONFIG
// deliberately, and nothing here runs off Linux.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";

// Enough of Tauri's dependency graph to catch a shadowed pkg-config: gdk-3.0
// is what drags in the X11 .pc files Homebrew lacks, javascriptcoregtk-4.1 is
// the package a Homebrew tree simply does not carry.
const NEEDED = ["gdk-3.0", "javascriptcoregtk-4.1"];
const SYSTEM = "/usr/bin/pkg-config";

const resolves = (command) =>
  spawnSync(command, ["--exists", ...NEEDED], { stdio: "ignore" }).status === 0;

function repairPkgConfig() {
  if (process.platform !== "linux" || process.env.PKG_CONFIG) return;
  if (resolves(process.env.PKG_CONFIG ?? "pkg-config")) return;
  if (!resolves(SYSTEM)) return;

  console.log(`dusk: pkg-config on PATH cannot see the system GTK; using ${SYSTEM}`);
  process.env.PKG_CONFIG = SYSTEM;
}

repairPkgConfig();

// A build makes signed update archives (createUpdaterArtifacts), and Tauri
// fails one without the signing key rather than ship an update nobody can
// verify. The release workflow passes the key; a local build uses the
// maintainer's copy if it is where `tauri signer generate` put it, and
// otherwise skips the update archives, which only a release needs.
function updaterSigning(args) {
  if (args[0] !== "build" || process.env.TAURI_SIGNING_PRIVATE_KEY) return args;
  const key = join(homedir(), ".tauri", "dusk.key");
  if (existsSync(key)) {
    process.env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(key, "utf8");
    return args;
  }
  console.log("dusk: no updater signing key; building without update archives");
  return [...args, "--config", JSON.stringify({ bundle: { createUpdaterArtifacts: false } })];
}

const args = updaterSigning(process.argv.slice(2));

const cli = createRequire(import.meta.url).resolve("@tauri-apps/cli/tauri.js");
const started = Date.now();
const tauri = spawn(process.execPath, [cli, ...args], {
  stdio: "inherit",
});

// The AppImage bundler patches every ELF file under usr/lib, which breaks
// the client and host Dusk carries: both are AppImages, and the patch lands
// on top of their payload. scripts/repair-appimage.sh puts the staged
// originals back; this runs it on each AppImage this build wrote and signs
// the result again, since the signature Tauri made is for the broken one.
function repairAppImages() {
  if (process.platform !== "linux" || args[0] !== "build") return 0;
  const version = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8")).version;
  for (const image of builtAppImages()) {
    const repair = spawnSync("scripts/repair-appimage.sh", [image], {
      stdio: ["ignore", "pipe", "inherit"],
      encoding: "utf8",
    });
    if (repair.status !== 0) {
      console.error(`dusk: could not repair ${image}`);
      return 1;
    }
    if (!repair.stdout.includes("repaired") || !existsSync(`${image}.sig`)) continue;
    const signing = ["signer", "sign", "--app-version", version, image];
    // Without this the signer asks on the terminal, which a build never does.
    if (process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD === undefined) {
      signing.push("--password", "");
    }
    const sign = spawnSync(process.execPath, [cli, ...signing], { stdio: "inherit" });
    if (sign.status !== 0) {
      console.error(`dusk: could not sign the repaired ${image}`);
      return 1;
    }
  }
  return 0;
}

// Wherever a target triple or profile put them, but only this build's.
function builtAppImages() {
  const found = [];
  const walk = (dir, depth) => {
    if (depth > 4 || !existsSync(dir)) return;
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path, depth + 1);
      else if (
        entry.name.endsWith(".AppImage") &&
        dir.endsWith(join("bundle", "appimage")) &&
        statSync(path).mtimeMs >= started
      ) {
        found.push(path);
      }
    }
  };
  walk(join("src-tauri", "target"), 0);
  return found;
}

tauri.on("exit", (code, signal) => {
  // Re-raise a signal as a signal so Ctrl-C out of `tauri dev` looks like one.
  if (signal) process.kill(process.pid, signal);
  process.exit(code === 0 ? repairAppImages() : (code ?? 1));
});
