#!/usr/bin/env node
// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");

const FORWARDED_SIGNALS = ["SIGINT", "SIGTERM", "SIGHUP"];

function executablePath(platform = process.platform, directory = __dirname) {
  const name = platform === "win32" ? "intentional-native.exe" : "intentional-native";
  return path.join(directory, name);
}

function run(argv = process.argv.slice(2), options = {}) {
  const processObject = options.processObject || process;
  const binary = options.binary || executablePath(options.platform, options.directory);
  const exists = options.exists || fs.existsSync;
  const spawnProcess = options.spawn || spawn;

  if (!exists(binary)) {
    (options.error || console.error)(
      "intentional: native executable is missing; reinstall @wyrd-company/intentional or install intentional-cli with Cargo",
    );
    processObject.exitCode = 1;
    return null;
  }

  const child = spawnProcess(binary, argv, { stdio: "inherit" });
  const handlers = new Map();
  const cleanup = () => {
    for (const [signal, handler] of handlers) {
      processObject.removeListener(signal, handler);
    }
  };

  if ((options.platform || process.platform) !== "win32") {
    for (const signal of FORWARDED_SIGNALS) {
      const handler = () => child.kill(signal);
      handlers.set(signal, handler);
      processObject.on(signal, handler);
    }
  }

  child.once("error", (error) => {
    cleanup();
    (options.error || console.error)(`intentional: ${error.message}`);
    processObject.exitCode = 1;
  });
  child.once("exit", (code, signal) => {
    cleanup();
    if (signal) {
      const number = os.constants.signals[signal];
      processObject.exitCode = number ? 128 + number : 1;
    } else {
      processObject.exitCode = code ?? 1;
    }
  });
  return child;
}

if (require.main === module) {
  run();
}

module.exports = { FORWARDED_SIGNALS, executablePath, run };
