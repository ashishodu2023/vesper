const vscode = require("vscode");
const { spawn } = require("child_process");

function runVesper(args) {
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!folder) {
    vscode.window.showErrorMessage("VESPER: open a folder first");
    return;
  }
  const channel = vscode.window.createOutputChannel("VESPER");
  channel.show(true);
  channel.appendLine(`$ vesper ${args.join(" ")}`);
  channel.appendLine(`cwd: ${folder}`);

  const child = spawn("vesper", args, {
    cwd: folder,
    env: process.env,
    shell: false,
  });
  child.stdout.on("data", (d) => channel.append(d.toString()));
  child.stderr.on("data", (d) => channel.append(d.toString()));
  child.on("error", (err) => {
    channel.appendLine(`\nFailed to start vesper: ${err.message}`);
    channel.appendLine("Install with: cargo install --path vesper-cli");
  });
  child.on("close", (code) => {
    channel.appendLine(`\n[exit ${code}]`);
  });
}

function activate(context) {
  context.subscriptions.push(
    vscode.commands.registerCommand("vesper.summarize", () =>
      runVesper(["summarize"])
    ),
    vscode.commands.registerCommand("vesper.fix", () =>
      runVesper(["fix", "-y"])
    ),
    vscode.commands.registerCommand("vesper.doctor", () =>
      runVesper(["doctor"])
    ),
    vscode.commands.registerCommand("vesper.run", async () => {
      const task = await vscode.window.showInputBox({
        prompt: "VESPER task",
        placeHolder: "fix the failing test and verify",
      });
      if (!task) return;
      runVesper(["run", task, "-y"]);
    })
  );
}

function deactivate() {}

module.exports = { activate, deactivate };
