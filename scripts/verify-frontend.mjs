import { spawn } from "node:child_process";

function commandFor(script) {
  const npmCli = process.env.npm_execpath;
  if (npmCli) {
    return { command: process.execPath, args: [npmCli, "run", script] };
  }
  return {
    command: process.platform === "win32" ? "cmd.exe" : "npm",
    args: process.platform === "win32" ? ["/d", "/s", "/c", `npm run ${script}`] : ["run", script],
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

const results = await Promise.all([run("check"), run("test")]);
process.exitCode = results.some((code) => code !== 0) ? 1 : 0;
