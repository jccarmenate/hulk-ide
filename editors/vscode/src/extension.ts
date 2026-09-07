import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const serverPath = resolveServerPath(context);

  const serverOptions: ServerOptions = {
    command: serverPath,
    args: [],
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'hulk' }],
  };

  client = new LanguageClient(
    'hulk',
    'HULK Language Server',
    serverOptions,
    clientOptions
  );
  context.subscriptions.push({ dispose: () => client?.stop() });
  client.start();

  context.subscriptions.push(
    vscode.commands.registerCommand('hulk.runFile', () => runActiveFile(context))
  );
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}

/// Resolves the `hulk-lsp` binary path: the `hulk.serverPath` setting if
/// set, otherwise the development build at `target/debug/hulk-lsp(.exe)`
/// relative to the repository root (this extension lives at
/// `editors/vscode/`, so the repository root is two levels up).
function resolveServerPath(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace
    .getConfiguration('hulk')
    .get<string>('serverPath');
  if (configured) {
    return configured;
  }
  const exeName = process.platform === 'win32' ? 'hulk-lsp.exe' : 'hulk-lsp';
  return context.asAbsolutePath(path.join('..', '..', 'target', 'debug', exeName));
}

let hulkTerminal: vscode.Terminal | undefined;

function getOrCreateHulkTerminal(): vscode.Terminal {
  if (!hulkTerminal || hulkTerminal.exitStatus !== undefined) {
    hulkTerminal = vscode.window.createTerminal('HULK');
  }
  return hulkTerminal;
}

/// Compiles and runs the active `.hulk` file via `hulk-cli`, in the
/// integrated terminal. `hulk-cli` already writes compiler errors to
/// stderr in `(line,col) TYPE: message` form and exits non-zero on
/// failure, so `&&` naturally skips execution when compilation fails.
async function runActiveFile(context: vscode.ExtensionContext): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== 'hulk') {
    vscode.window.showErrorMessage('Open a .hulk file to run it.');
    return;
  }

  await editor.document.save();

  const filePath = editor.document.fileName;
  const dir = path.dirname(filePath);
  const cliName = process.platform === 'win32' ? 'hulk-cli.exe' : 'hulk-cli';
  const cliPath = context.asAbsolutePath(
    path.join('..', '..', 'target', 'debug', cliName)
  );

  const terminal = getOrCreateHulkTerminal();
  terminal.show();
  terminal.sendText(`cd "${dir}"`);
  if (process.platform === 'win32') {
    // PowerShell (VS Code's default Windows shell): `hulk-cli` writes
    // `./output` per its grader contract, but whether that lands as
    // `output` or `output.exe` on Windows hasn't been verified in this
    // environment (no LLVM 17 to build hulk-cli with) — try both.
    terminal.sendText(
      `& "${cliPath}" "${filePath}" && (if (Test-Path .\\output.exe) { .\\output.exe } else { .\\output })`
    );
  } else {
    terminal.sendText(`"${cliPath}" "${filePath}" && ./output`);
  }
}
