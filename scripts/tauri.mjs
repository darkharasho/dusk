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
import { createRequire } from "node:module";

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

const cli = createRequire(import.meta.url).resolve("@tauri-apps/cli/tauri.js");
const tauri = spawn(process.execPath, [cli, ...process.argv.slice(2)], {
  stdio: "inherit",
});

tauri.on("exit", (code, signal) => {
  // Re-raise a signal as a signal so Ctrl-C out of `tauri dev` looks like one.
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
