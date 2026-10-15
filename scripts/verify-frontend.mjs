import { spawn } from "node:child_process";

function commandFor(script) {
  const packageManagerCli = process.env.npm_execpath;
  if (packageManagerCli) {
    if (/\.[cm]?js$/i.test(packageManagerCli)) {
      return { command: process.execPath, args: [packageManagerCli, "run", script] };
    }
    if (process.platform === "win32" && /\.cmd$/i.test(packageManagerCli)) {
      return {
        command: "cmd.exe",
        args: ["/d", "/s", "/c", `"${packageManagerCli}" run ${script}`],
      };
    }
    return { command: packageManagerCli, args: ["run", script] };
  }
  return {
    command: process.platform === "win32" ? "cmd.exe" : "pnpm",
    args: process.platform === "win32" ? ["/d", "/s", "/c", `pnpm run ${script}`] : ["run", script],
  };
}

function run(script) {
  return new Promise((resolve) => {
    const { command, args } = commandFor(script);
    const child = spawn(command, args, {
      cwd: process.cwd(),
      stdio: "inherit",
      env: process.env,
    });

    child.on("error", (error) => {
      console.error(`[verify:fast] ${script} failed to start`, error);
      resolve(1);
    });
    child.on("exit", (code, signal) => {
      if (signal) {
        console.error(`[verify:fast] ${script} terminated by ${signal}`);
        resolve(1);
        return;
      }
      resolve(code ?? 1);
    });
  });
}

const checkResult = await run("check");
if (checkResult !== 0) {
  process.exitCode = checkResult;
} else {
  process.exitCode = await run("test");
}
